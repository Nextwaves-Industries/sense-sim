//! The time stepper on a GPU, through wgpu compute shaders, in single precision.
//!
//! It does what [`crate::engine::Engine`] does, pass for pass, so the two can be compared number
//! for number. Shaders are generated for the grid at hand, because a constant in the source is
//! cheaper than one read from a buffer in a loop this hot. Bounds checks are off: every index is
//! in range by construction, which the CPU engine's checked slices confirm on the same stencils.
//!
//! Port readings and recorded fields go in buffers that come back once per batch of steps, so a
//! run's cost in the driver is per batch rather than per step.

use anyhow::{Context, Result, anyhow, bail};
use wgpu::util::DeviceExt;

use crate::assemble::Assembled;
use crate::engine::{PortTrace, Stop};
use crate::problem::{Pulse, Wall};
use crate::record::{NodeDump, NodePlane, NodeRecorder, Recorder, Window};

/// Shared by every shader: the grid's shape.
fn header(n: [usize; 3]) -> String {
    format!("const NX: u32 = {}u;\nconst NY: u32 = {}u;\nconst NZ: u32 = {}u;\nconst NL: u32 = NX * NY;\n", n[0], n[1], n[2])
}

const WG: u32 = 64;
const WG_Y: u32 = 4;

/// One main-loop pass over a component.
struct MainPass {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    dispatch: [u32; 3],
}

/// What's in a run but not in the main loop: the absorber and each kind of edge list.
struct SmallPass {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    dispatch: [u32; 3],
}

struct PortBuffers {
    edges: wgpu::Buffer,
    /// `scale` per edge for when this port is driven.
    scale: Vec<f32>,
    edge_data: Vec<[u32; 4]>,
}

/// One batch's recording of a dump at grid nodes.
struct NodeExtract {
    rec: NodeRecorder,
    pass: SmallPass,
    out: wgpu::Buffer,
    staging: wgpu::Buffer,
}

/// One batch's recording of a window.
struct WindowExtract {
    pass: SmallPass,
    out: wgpu::Buffer,
    staging: wgpu::Buffer,
    cells: usize,
}

pub struct GpuEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub n: [usize; 3],
    pub dt: f64,
    pub step: u64,
    pub adapter_name: String,
    fields: [wgpu::Buffer; 6],
    h_main: Vec<MainPass>,
    e_main: Vec<MainPass>,
    h_small: Vec<SmallPass>,
    e_small: Vec<SmallPass>,
    psi: Vec<wgpu::Buffer>,
    ports_pass: Vec<SmallPass>,
    port_buffers: Vec<PortBuffers>,
    tick: SmallPass,
    sample_tick: SmallPass,
    state: wgpu::Buffer,
    wave: wgpu::Buffer,
    wave_len: usize,
    trace: wgpu::Buffer,
    trace_staging: wgpu::Buffer,
    pub traces: Vec<PortTrace>,
    energy_pass: Vec<SmallPass>,
    energy_rows: wgpu::Buffer,
    energy_staging: wgpu::Buffer,
    extract: Vec<WindowExtract>,
    recorders: Vec<(Recorder, Vec<usize>)>,
    node_extract: Vec<NodeExtract>,
    pub wave_ports: Vec<crate::waveport::WavePortRun>,
    wave_inject_h: Vec<SmallPass>,
    wave_inject_e: Vec<SmallPass>,
    wave_read: Vec<SmallPass>,
    wave_trace: Option<(wgpu::Buffer, wgpu::Buffer)>,
    batch: u64,
    every: u64,
    samples_per_batch: u64,
    pulse: Option<Pulse>,
    pub peak_energy: f64,
}

fn storage(device: &wgpu::Device, label: &str, size: u64, extra: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size.max(4), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC | extra, mapped_at_creation: false })
}

fn storage_init<T: bytemuck_like::Pod>(device: &wgpu::Device, label: &str, data: &[T]) -> wgpu::Buffer {
    let bytes = bytemuck_like::bytes_of(data);
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: if bytes.is_empty() { &[0; 4] } else { bytes }, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC })
}

/// Plain-old-data views, without another dependency for four lines.
mod bytemuck_like {
    pub trait Pod: Copy {}
    impl Pod for f32 {}
    impl Pod for u32 {}
    impl Pod for [f32; 4] {}
    impl Pod for [u32; 4] {}
    impl Pod for [u32; 2] {}
    pub fn bytes_of<T: Pod>(v: &[T]) -> &[u8] {
        // SAFETY: `Pod` types have no padding or invalid bit patterns.
        unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
    }
    pub fn f32s(b: &[u8]) -> Vec<f32> {
        b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }
}

struct Maker<'a> {
    device: &'a wgpu::Device,
}

impl Maker<'_> {
    /// A compute pipeline over the given buffers, in binding order; `rw` says which are written.
    fn pipeline(&self, label: &str, source: &str, buffers: &[(&wgpu::Buffer, bool)]) -> (wgpu::ComputePipeline, wgpu::BindGroup) {
        // SAFETY: the generated shaders only index within their buffers: every loop's range is
        // taken from the grid they were generated for, as in the CPU engine's checked loops.
        let module = unsafe {
            self.device.create_shader_module_trusted(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) }, wgpu::ShaderRuntimeChecks::unchecked())
        };
        let entries: Vec<wgpu::BindGroupLayoutEntry> = buffers
            .iter()
            .enumerate()
            .map(|(k, (_, rw))| wgpu::BindGroupLayoutEntry {
                binding: k as u32,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: !*rw }, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            })
            .collect();
        let layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries: &entries });
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(label), layout: Some(&pl), module: &module, entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let binds: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(k, (b, _))| wgpu::BindGroupEntry { binding: k as u32, resource: b.as_entire_binding() }).collect();
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout: &layout, entries: &binds });
        (pipeline, bind)
    }

    fn small(&self, label: &str, source: &str, buffers: &[(&wgpu::Buffer, bool)], dispatch: [u32; 3]) -> SmallPass {
        let (pipeline, bind) = self.pipeline(label, source, buffers);
        SmallPass { pipeline, bind, dispatch }
    }
}

/// `@binding` lines for buffers by name; `true` is read-write.
fn bindings(list: &[(&str, &str, bool)]) -> String {
    let mut s = String::new();
    for (k, (name, ty, rw)) in list.iter().enumerate() {
        let access = if *rw { "read_write" } else { "read" };
        s.push_str(&format!("@group(0) @binding({k}) var<storage, {access}> {name}: {ty};\n"));
    }
    s
}

fn wg_count(n: usize) -> u32 {
    (n as u32).div_ceil(WG).max(1)
}

impl GpuEngine {
    /// Finds a GPU able to hold this model. An error means use the CPU.
    pub fn new(asm: &Assembled, dt: f64, far_every: u64) -> Result<Self> {
        let n = asm.grid.n();
        let cells = asm.grid.nodes() as u64;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, force_fallback_adapter: false, compatible_surface: None, apply_limit_buckets: false })).map_err(|e| anyhow!("no GPU: {e}"))?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            bail!("only a software GPU is available");
        }
        let limits = adapter.limits();
        let array_bytes = cells * 4;
        if array_bytes > limits.max_storage_buffer_binding_size as u64 || array_bytes > limits.max_buffer_size {
            bail!("the model's arrays are {:.1} GB and this GPU binds at most {:.1} GB", array_bytes as f64 / 1e9, limits.max_storage_buffer_binding_size as f64 / 1e9);
        }
        let required = wgpu::Limits { max_storage_buffers_per_shader_stage: 16.min(limits.max_storage_buffers_per_shader_stage), max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size, max_buffer_size: limits.max_buffer_size, max_compute_workgroups_per_dimension: limits.max_compute_workgroups_per_dimension, ..wgpu::Limits::downlevel_defaults() };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("fdtd"), required_features: wgpu::Features::empty(), required_limits: required, experimental_features: Default::default(), memory_hints: wgpu::MemoryHints::Performance, trace: Default::default() })).context("creating the GPU device")?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let maker = Maker { device: &device };
        let nodes = asm.grid.nodes();
        // The coefficients, as the CPU engine makes them.
        let mut ce: [Vec<f32>; 3] = std::array::from_fn(|a| asm.me[a].iter().map(|m| if m.is_finite() && *m > 0.0 { (dt / m) as f32 } else { 0.0 }).collect());
        let ch: [Vec<f32>; 3] = std::array::from_fn(|a| asm.imu[a].iter().map(|m| (dt * m) as f32).collect());
        let m_dt = |a: usize, p: usize| asm.me[a][p] / dt;
        let mut rl_lists: [Vec<[f32; 4]>; 3] = Default::default();
        let mut rl_nodes: [Vec<u32>; 3] = Default::default();
        for e in &asm.rl {
            let m = asm.me[e.axis][e.node];
            let den = e.l / dt + e.r / 2.0;
            rl_lists[e.axis].push([(m / dt) as f32, ((e.l / dt - e.r / 2.0) / den) as f32, (0.5 / den) as f32, 0.0]);
            rl_nodes[e.axis].push(e.node as u32);
            ce[e.axis][e.node] = 0.0;
        }
        let mut lossy: [Vec<[f32; 4]>; 3] = Default::default();
        let mut lossy_nodes: [Vec<u32>; 3] = Default::default();
        for a in 0..3 {
            if let Some(g) = &asm.g[a] {
                for p in 0..nodes {
                    if g[p] > 0.0 && asm.me[a][p].is_finite() {
                        let s = g[p] * dt / (2.0 * asm.me[a][p]);
                        lossy[a].push([((1.0 - s) / (1.0 + s)) as f32, (1.0 / (1.0 + s)) as f32, 0.0, 0.0]);
                        lossy_nodes[a].push(p as u32);
                    }
                }
            }
        }
        let mut debye: [Vec<[f32; 4]>; 3] = Default::default();
        let mut debye_nodes: [Vec<u32>; 3] = Default::default();
        for d in &asm.debye {
            let a = dt / (2.0 * d.r * d.c);
            debye[d.axis].push([m_dt(d.axis, d.node) as f32, (1.0 / (d.r * (1.0 + a))) as f32, a as f32, 0.0]);
            debye_nodes[d.axis].push(d.node as u32);
            ce[d.axis][d.node] = 0.0;
        }
        // Ports: each edge updated from its list, the main loop leaving it alone.
        let mut port_data = Vec::new();
        for p in &asm.ports {
            let series = p.columns.first().map_or(1, |c| c.len()) as f64;
            let mut edge_data = Vec::with_capacity(p.edges.len());
            let mut scale = Vec::with_capacity(p.edges.len());
            for &(node, r) in &p.edges {
                ce[p.axis][node] = 0.0;
                edge_data.push([node as u32, (m_dt(p.axis, node) as f32).to_bits(), ((1.0 / r) as f32).to_bits(), 0u32]);
                scale.push((-p.sign / series) as f32);
            }
            port_data.push((edge_data, scale));
        }
        let fields: [wgpu::Buffer; 6] = std::array::from_fn(|k| storage(&device, ["ex", "ey", "ez", "hx", "hy", "hz"][k], nodes as u64 * 4, wgpu::BufferUsages::empty()));
        let ce_b: [wgpu::Buffer; 3] = std::array::from_fn(|a| storage_init(&device, "ce", &ce[a]));
        let ch_b: [wgpu::Buffer; 3] = std::array::from_fn(|a| storage_init(&device, "ch", &ch[a]));
        let (e, h) = ([&fields[0], &fields[1], &fields[2]], [&fields[3], &fields[4], &fields[5]]);
        // Main loops, all three components of a half step in one pass: the neighbours in memory are
        // shared, and two dispatches a step cost the driver far less than six. Every index stays
        // inside the grid: the last line along each axis is a wall, which never updates.
        let [nx, ny, nz] = n;
        let main_binds = bindings(&[("fx", "array<f32>", true), ("fy", "array<f32>", true), ("fz", "array<f32>", true), ("cx", "array<f32>", false), ("cy", "array<f32>", false), ("cz", "array<f32>", false), ("gx", "array<f32>", false), ("gy", "array<f32>", false), ("gz", "array<f32>", false)]);
        let h_src = format!(
            "{}{main_binds}@compute @workgroup_size({WG},{WG_Y},1)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  if (id.x >= NX - 1u || id.y >= NY - 1u) {{ return; }}\n  let p = id.x + NX * (id.y + NY * id.z);\n  fx[p] += cx[p] * ((gy[p + NL] - gy[p]) - (gz[p + NX] - gz[p]));\n  fy[p] += cy[p] * ((gz[p + 1u] - gz[p]) - (gx[p + NL] - gx[p]));\n  fz[p] += cz[p] * ((gx[p + NX] - gx[p]) - (gy[p + 1u] - gy[p]));\n}}\n",
            header(n)
        );
        let e_src = format!(
            "{}{main_binds}@compute @workgroup_size({WG},{WG_Y},1)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  let i = id.x;\n  let j = id.y;\n  let k = id.z;\n  if (i >= NX - 1u || j >= NY - 1u) {{ return; }}\n  let p = i + NX * (j + NY * k);\n  if (j > 0u && k > 0u) {{ fx[p] += cx[p] * ((gz[p] - gz[p - NX]) - (gy[p] - gy[p - NL])); }}\n  if (i > 0u && k > 0u) {{ fy[p] += cy[p] * ((gx[p] - gx[p - NL]) - (gz[p] - gz[p - 1u])); }}\n  if (i > 0u && j > 0u) {{ fz[p] += cz[p] * ((gy[p] - gy[p - 1u]) - (gx[p] - gx[p - NX])); }}\n}}\n",
            header(n)
        );
        let dispatch = [wg_count(nx - 1), ((ny - 1) as u32).div_ceil(WG_Y), (nz - 1) as u32];
        let h_main: Vec<MainPass> = {
            let (pipeline, bind) = maker.pipeline("h", &h_src, &[(h[0], true), (h[1], true), (h[2], true), (&ch_b[0], false), (&ch_b[1], false), (&ch_b[2], false), (e[0], false), (e[1], false), (e[2], false)]);
            vec![MainPass { pipeline, bind, dispatch }]
        };
        let e_main: Vec<MainPass> = {
            let (pipeline, bind) = maker.pipeline("e", &e_src, &[(e[0], true), (e[1], true), (e[2], true), (&ce_b[0], false), (&ce_b[1], false), (&ce_b[2], false), (h[0], false), (h[1], false), (h[2], false)]);
            vec![MainPass { pipeline, bind, dispatch }]
        };
        // The absorber.
        let strides = [1usize, nx, nx * ny];
        let mut h_small = Vec::new();
        let mut e_small = Vec::new();
        let mut psi = Vec::new();
        for axis in 0..3 {
            let lines = &asm.grid.lines[axis];
            let (t1, t2) = ((axis + 1) % 3, (axis + 2) % 3);
            let (n1, n2) = (n[t1], n[t2]);
            for side in 0..2 {
                let Wall::Pml(depth) = asm.walls[axis * 2 + side] else { continue };
                if depth == 0 || depth + 2 >= lines.len() {
                    continue;
                }
                let (prof_e, plane_e, prof_h, plane_h) = crate::engine::pml_tables(lines, side, depth, dt);
                let prof_eb = storage_init(&device, "pml", &prof_e);
                let prof_hb = storage_init(&device, "pml", &prof_h);
                let size = (n1 * n2 * depth) as u64 * 4;
                let (pe1, pe2, ph1, ph2) = (storage(&device, "psi", size, wgpu::BufferUsages::empty()), storage(&device, "psi", size, wgpu::BufferUsages::empty()), storage(&device, "psi", size, wgpu::BufferUsages::empty()), storage(&device, "psi", size, wgpu::BufferUsages::empty()));
                let (s_a, s1, s2) = (strides[axis], strides[t1], strides[t2]);
                let names = bindings(&[("f1", "array<f32>", true), ("f2", "array<f32>", true), ("g1", "array<f32>", false), ("g2", "array<f32>", false), ("c1", "array<f32>", false), ("c2", "array<f32>", false), ("p1", "array<f32>", true), ("p2", "array<f32>", true), ("prof", "array<vec4<f32>>", false)]);
                let body = |plane0: usize, is_h: bool| {
                    let (d2, d1) = if is_h { (format!("g2[q + {s_a}u] - g2[q]"), format!("g1[q + {s_a}u] - g1[q]")) } else { (format!("g2[q] - g2[q - {s_a}u]"), format!("g1[q] - g1[q - {s_a}u]")) };
                    let (sign1, sign2) = if is_h { ("", "-") } else { ("-", "") };
                    format!(
                        "{}{names}@compute @workgroup_size({WG},1,1)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  let i1 = id.x;\n  if (i1 >= {n1}u) {{ return; }}\n  let i2 = id.y;\n  let l = id.z;\n  let q = ({plane0}u + l) * {s_a}u + i1 * {s1}u + i2 * {s2}u;\n  let w = i1 + {n1}u * (i2 + {n2}u * l);\n  let pr = prof[l];\n  let d2 = {d2};\n  let a1 = pr.y * p1[w] + pr.x * d2;\n  p1[w] = a1;\n  f1[q] += {sign1}c1[q] * (pr.z * d2 + a1);\n  let d1 = {d1};\n  let a2 = pr.y * p2[w] + pr.x * d1;\n  p2[w] = a2;\n  f2[q] += {sign2}c2[q] * (pr.z * d1 + a2);\n}}\n",
                        header(n)
                    )
                };
                let dispatch = [wg_count(n1), n2 as u32, depth as u32];
                // H_t1 gets +∂a E_t2 and H_t2 gets -∂a E_t1; E_t1 gets -∂a H_t2 and E_t2 gets +∂a H_t1.
                h_small.push(maker.small("pml h", &body(plane_h, true), &[(h[t1], true), (h[t2], true), (e[t1], false), (e[t2], false), (&ch_b[t1], false), (&ch_b[t2], false), (&ph1, true), (&ph2, true), (&prof_hb, false)], dispatch));
                e_small.push(maker.small("pml e", &body(plane_e, false), &[(e[t1], true), (e[t2], true), (h[t1], false), (h[t2], false), (&ce_b[t1], false), (&ce_b[t2], false), (&pe1, true), (&pe2, true), (&prof_eb, false)], dispatch));
                psi.extend([pe1, pe2, ph1, ph2]);
            }
        }
        // Edge lists. Curl around an edge along a: (h[a+2][p] - h[a+2][p - S1]) - (h[a+1][p] - h[a+1][p - S2]).
        for a in 0..3 {
            let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
            let (s1, s2) = (strides[t1], strides[t2]);
            let curl = format!("(h2[p] - h2[p - {s1}u]) - (h1[p] - h1[p - {s2}u])");
            if !lossy[a].is_empty() {
                let src = format!(
                    "{}{}@compute @workgroup_size({WG})\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  if (id.x >= {}u) {{ return; }}\n  let p = nodes[id.x];\n  let v = list[id.x];\n  let k = c[p];\n  let curl = {curl};\n  let old = e[p] - k * curl;\n  e[p] = v.x * old + v.y * k * curl;\n}}\n",
                    header(n), bindings(&[("e", "array<f32>", true), ("c", "array<f32>", false), ("h1", "array<f32>", false), ("h2", "array<f32>", false), ("nodes", "array<u32>", false), ("list", "array<vec4<f32>>", false)]), lossy[a].len()
                );
                let (nb, lb) = (storage_init(&device, "lossy", &lossy_nodes[a]), storage_init(&device, "lossy", &lossy[a]));
                e_small.push(maker.small("lossy", &src, &[(e[a], true), (&ce_b[a], false), (h[t1], false), (h[t2], false), (&nb, false), (&lb, false)], [wg_count(lossy[a].len()), 1, 1]));
                let _ = (nb, lb);
            }
            if !rl_lists[a].is_empty() {
                let src = format!(
                    "{}{}@compute @workgroup_size({WG})\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  if (id.x >= {}u) {{ return; }}\n  let p = nodes[id.x];\n  let v = list[id.x];\n  let curl = {curl};\n  let old = e[p];\n  let next = (old * (v.x - v.z * 0.5) + curl - (1.0 + v.y) * cur[id.x] * 0.5) / (v.x + v.z * 0.5);\n  cur[id.x] = v.y * cur[id.x] + v.z * (next + old);\n  e[p] = next;\n}}\n",
                    header(n), bindings(&[("e", "array<f32>", true), ("h1", "array<f32>", false), ("h2", "array<f32>", false), ("nodes", "array<u32>", false), ("list", "array<vec4<f32>>", false), ("cur", "array<f32>", true)]), rl_lists[a].len()
                );
                let (nb, lb, st) = (storage_init(&device, "rl", &rl_nodes[a]), storage_init(&device, "rl", &rl_lists[a]), storage(&device, "rl state", rl_lists[a].len() as u64 * 4, wgpu::BufferUsages::empty()));
                e_small.push(maker.small("rl", &src, &[(e[a], true), (h[t1], false), (h[t2], false), (&nb, false), (&lb, false), (&st, true)], [wg_count(rl_lists[a].len()), 1, 1]));
                let _ = (nb, lb);
                psi.push(st);
            }
            if !debye[a].is_empty() {
                let src = format!(
                    "{}{}@compute @workgroup_size({WG})\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  if (id.x >= {}u) {{ return; }}\n  let p = nodes[id.x];\n  let v = list[id.x];\n  let curl = {curl};\n  let old = e[p];\n  let next = (old * (v.x - v.y * 0.5) + curl + v.y * cap[id.x]) / (v.x + v.y * 0.5);\n  let mean = (next + old) * 0.5;\n  cap[id.x] = (cap[id.x] * (1.0 - v.z) + 2.0 * v.z * mean) / (1.0 + v.z);\n  e[p] = next;\n}}\n",
                    header(n), bindings(&[("e", "array<f32>", true), ("h1", "array<f32>", false), ("h2", "array<f32>", false), ("nodes", "array<u32>", false), ("list", "array<vec4<f32>>", false), ("cap", "array<f32>", true)]), debye[a].len()
                );
                let (nb, lb, st) = (storage_init(&device, "debye", &debye_nodes[a]), storage_init(&device, "debye", &debye[a]), storage(&device, "debye state", debye[a].len() as u64 * 4, wgpu::BufferUsages::empty()));
                e_small.push(maker.small("debye", &src, &[(e[a], true), (h[t1], false), (h[t2], false), (&nb, false), (&lb, false), (&st, true)], [wg_count(debye[a].len()), 1, 1]));
                let _ = (nb, lb);
                psi.push(st);
            }
        }
        // Steps between readbacks, a whole number of recording intervals.
        let every = far_every.max(1);
        // Few enough on a big model that one submission stays well under a second: the driver can
        // reset a GPU that goes quiet for about two.
        let target = (500_000_000 / cells.max(1)).clamp(4, 64);
        let batch = if every >= target { every } else { every * target.div_ceil(every) };
        let samples_per_batch = batch / every;
        let nports = asm.ports.len().max(1) as u64;
        // The pulse, up front: nothing after it matters.
        let state = storage_init(&device, "state", &[0u32; 4]);
        let wave_len = 1usize;
        let wave = storage(&device, "wave", 4 * 16_000_000.min(1 << 24), wgpu::BufferUsages::empty());
        let trace = storage(&device, "trace", batch * nports * 8, wgpu::BufferUsages::empty());
        let trace_staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("trace back"), size: batch * nports * 8, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        // Ports. One invocation, in order: the current first, as H is at this half step, then E.
        let mut ports_pass = Vec::new();
        let mut port_buffers = Vec::new();
        for (pi, p) in asm.ports.iter().enumerate() {
            let a = p.axis;
            let (t1, t2) = ((a + 1) % 3, (a + 2) % 3);
            let (s1, s2) = (strides[t1], strides[t2]);
            let (edge_data, scale) = &port_data[pi];
            let edges_b = storage_init(&device, "port edges", edge_data);
            let mid: Vec<u32> = p.mid.iter().map(|v| *v as u32).collect();
            let col: Vec<u32> = p.columns.iter().flatten().map(|v| *v as u32).collect();
            let mid_b = storage_init(&device, "port mid", &mid);
            let col_b = storage_init(&device, "port col", &col);
            let curl = |node: &str| format!("(h2[{node}] - h2[{node} - {s1}u]) - (h1[{node}] - h1[{node} - {s2}u])");
            let src = format!(
                "{}{}@compute @workgroup_size(1)\nfn main() {{\n  let s = state[0];\n  let b = state[1];\n  var src = 0.0;\n  if (s < {wave_max}u) {{ src = wave[s]; }}\n  var current = 0.0;\n  for (var k = 0u; k < {ne}u; k++) {{\n    let v = edges[k];\n    let p = v.x;\n    let m = bitcast<f32>(v.y);\n    let g = bitcast<f32>(v.z);\n    let sc = bitcast<f32>(v.w);\n    let curl = {curl_edge};\n    let old = e[p];\n    let next = (old * (m - g * 0.5) + curl + g * sc * src) / (m + g * 0.5);\n    e[p] = next;\n    for (var q = 0u; q < {nm}u; q++) {{ if (mid[q] == p) {{ current += g * ((next + old) * 0.5 - sc * src); }} }}\n  }}\n  var voltage = 0.0;\n  for (var k = 0u; k < {nc}u; k++) {{ voltage += e[col[k]]; }}\n  trace[(b * {nports}u + {pi}u) * 2u] = {u_sign} * voltage / {columns}.0;\n  trace[(b * {nports}u + {pi}u) * 2u + 1u] = {i_sign} * current;\n}}\n",
                header(n),
                bindings(&[("e", "array<f32>", true), ("h1", "array<f32>", false), ("h2", "array<f32>", false), ("edges", "array<vec4<u32>>", false), ("mid", "array<u32>", false), ("col", "array<u32>", false), ("state", "array<u32>", false), ("wave", "array<f32>", false), ("trace", "array<f32>", true)]),
                wave_max = 16_000_000u64.min(1 << 24),
                nm = mid.len(),
                ne = edge_data.len(),
                nc = col.len(),
                nports = nports,
                pi = pi,
                curl_edge = curl("p"),
                u_sign = if p.sign > 0.0 { "-1.0" } else { "1.0" },
                i_sign = if p.sign > 0.0 { "1.0" } else { "-1.0" },
                columns = p.columns.len().max(1),
            );
            ports_pass.push(maker.small("port", &src, &[(e[a], true), (h[t1], false), (h[t2], false), (&edges_b, false), (&mid_b, false), (&col_b, false), (&state, false), (&wave, false), (&trace, true)], [1, 1, 1]));
            port_buffers.push(PortBuffers { edges: edges_b, scale: scale.clone(), edge_data: edge_data.clone() });
            // Kept alive with the bind group, which holds its own reference.
            let _ = (mid_b, col_b);
        }
        let tick_src = format!(
            "{}\n@compute @workgroup_size(1)\nfn main() {{\n  state[0] += 1u;\n  state[1] += 1u;\n}}\n",
            bindings(&[("state", "array<u32>", true)])
        );
        let tick = maker.small("tick", &tick_src, &[(&state, true)], [1, 1, 1]);
        let sample_src = format!("{}
@compute @workgroup_size(1)
fn main() {{
  state[2] += 1u;
}}
", bindings(&[("state", "array<u32>", true)]));
        let sample_tick = maker.small("sample tick", &sample_src, &[(&state, true)], [1, 1, 1]);
        // Energy, a row at a time, comes back as one number per row.
        let energy_rows = storage(&device, "energy", (ny * nz) as u64 * 4 * 6, wgpu::BufferUsages::empty());
        let energy_staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("energy back"), size: (ny * nz) as u64 * 4 * 6, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut energy_pass = Vec::new();
        for k in 0..6 {
            let src = format!(
                "{}{}@compute @workgroup_size(16)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  let j = id.x;\n  if (j >= NY) {{ return; }}\n  let base = NX * (j + NY * id.y);\n  var sum = 0.0;\n  for (var i = 0u; i < NX; i++) {{\n    let k = c[base + i];\n    if (k > 0.0) {{ let v = f[base + i]; sum += v * v / k; }}\n  }}\n  rows[(j + NY * id.y) + NY * NZ * {k}u] = sum;\n}}\n",
                header(n), bindings(&[("f", "array<f32>", false), ("c", "array<f32>", false), ("rows", "array<f32>", true)])
            );
            let c = if k < 3 { &ce_b[k] } else { &ch_b[k - 3] };
            energy_pass.push(maker.small("energy", &src, &[(&fields[k], false), (c, false), (&energy_rows, true)], [(ny as u32).div_ceil(16), nz as u32, 1]));
        }
        let traces = asm.ports.iter().map(|p| PortTrace { z0: p.z0, ..Default::default() }).collect();
        let err = pollster::block_on(scope.pop());
        if let Some(e) = err {
            bail!("the GPU ran out of memory for this model: {e}");
        }
        let mut me = Self {
            adapter_name: info.name.clone(),
            device,
            queue,
            n,
            dt,
            step: 0,
            fields,
            h_main,
            e_main,
            h_small,
            e_small,
            psi,
            ports_pass,
            port_buffers,
            tick,
            sample_tick,
            state,
            wave,
            wave_len,
            trace,
            trace_staging,
            traces,
            energy_pass,
            energy_rows,
            energy_staging,
            extract: Vec::new(),
            recorders: Vec::new(),
            node_extract: Vec::new(),
            wave_ports: Vec::new(),
            wave_inject_h: Vec::new(),
            wave_inject_e: Vec::new(),
            wave_read: Vec::new(),
            wave_trace: None,
            batch,
            every,
            samples_per_batch,
            pulse: None,
            peak_energy: 0.0,
        };
        me.reset();
        // Reading `state` read-only in the ports while `tick` writes it is fine: passes are ordered.
        Ok(me)
    }

    /// Adds a recording. Its sampling interval must be the one given at construction.
    pub fn record(&mut self, asm: &Assembled, rec: Recorder, windows: &[Window]) {
        let grid = &asm.grid;
        let n = self.n;
        let strides = [1usize, n[0], n[0] * n[1]];
        let maker = Maker { device: &self.device };
        let mut ids = Vec::new();
        for w in windows {
            let (t1, t2) = ((w.axis + 1) % 3, (w.axis + 2) % 3);
            let (m1, m2) = (w.r1.1 - w.r1.0, w.r2.1 - w.r2.0);
            let cells = m1 * m2;
            let (st1, st2, sa) = (strides[t1], strides[t2], strides[w.axis]);
            let table = |t: usize, r: (usize, usize), prim: bool| -> Vec<f32> { (r.0..=r.1).map(|i| (if prim { grid.primal(t, i) } else { grid.dual(t, i) }) as f32).collect() };
            let (l1, l2, d1, d2) = (table(t1, w.r1, true), table(t2, w.r2, true), table(t1, w.r1, false), table(t2, w.r2, false));
            let (lb1, lb2, db1, db2) = (storage_init(&self.device, "len", &l1), storage_init(&self.device, "len", &l2), storage_init(&self.device, "len", &d1), storage_init(&self.device, "len", &d2));
            let bytes = cells as u64 * 20 * self.samples_per_batch;
            let out = storage(&self.device, "window", bytes, wgpu::BufferUsages::empty());
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("window back"), size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let src = format!(
                "{}{}@compute @workgroup_size({WG},1,1)\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  let a = id.x;\n  if (a >= {m1}u) {{ return; }}\n  let b = id.y;\n  let q = {plane}u * {sa}u + ({r10}u + a) * {st1}u + ({r20}u + b) * {st2}u;\n  let ev1 = (e1[q] + e1[q + {st2}u]) * 0.5 / l1[a];\n  let ev2 = (e2[q] + e2[q + {st1}u]) * 0.5 / l2[b];\n  let hv1 = (h1[q] / d1[a] + h1[q + {st1}u] / d1[a + 1u] + h1[q - {sa}u] / d1[a] + h1[q - {sa}u + {st1}u] / d1[a + 1u]) * 0.25;\n  let hv2 = (h2[q] / d2[b] + h2[q + {st2}u] / d2[b + 1u] + h2[q - {sa}u] / d2[b] + h2[q - {sa}u + {st2}u] / d2[b + 1u]) * 0.25;\n  let en = ((ea[q] + ea[q + {st1}u] + ea[q + {st2}u] + ea[q + {st1}u + {st2}u]) / {up} + (ea[q - {sa}u] + ea[q - {sa}u + {st1}u] + ea[q - {sa}u + {st2}u] + ea[q - {sa}u + {st1}u + {st2}u]) / {down}) * 0.125;\n  let at = (a + {m1}u * b + {cells}u * state[2]) * 5u;\n  out[at] = ev1;\n  out[at + 1u] = ev2;\n  out[at + 2u] = hv1;\n  out[at + 3u] = hv2;\n  out[at + 4u] = en;\n}}\n",
                header(n),
                bindings(&[("e1", "array<f32>", false), ("e2", "array<f32>", false), ("h1", "array<f32>", false), ("h2", "array<f32>", false), ("l1", "array<f32>", false), ("l2", "array<f32>", false), ("d1", "array<f32>", false), ("d2", "array<f32>", false), ("out", "array<f32>", true), ("state", "array<u32>", false), ("ea", "array<f32>", false)]),
                up = format!("{:e}", grid.primal(w.axis, w.plane) as f32),
                down = format!("{:e}", grid.primal(w.axis, w.plane - 1) as f32),
                plane = w.plane,
                r10 = w.r1.0,
                r20 = w.r2.0,
            );
            let pass = maker.small("window", &src, &[(&self.fields[t1], false), (&self.fields[t2], false), (&self.fields[3 + t1], false), (&self.fields[3 + t2], false), (&lb1, false), (&lb2, false), (&db1, false), (&db2, false), (&out, true), (&self.state, false), (&self.fields[w.axis], false)], [wg_count(m1), m2 as u32, 1]);
            ids.push(self.extract.len());
            self.extract.push(WindowExtract { pass, out, staging, cells });
        }
        self.recorders.push((rec, ids));
    }

    /// Adds dumps of the field at grid nodes. `every` must be the interval given at construction.
    pub fn record_nodes(&mut self, asm: &Assembled, dumps: &[NodeDump], every: u64) {
        let maker = Maker { device: &self.device };
        for d in dumps {
            let rec = NodeRecorder::new(&asm.grid, d.clone(), every);
            let total = rec.table.len();
            let idx: Vec<[u32; 4]> = rec.table.iter().map(|n| [n.p, n.lo[0], n.lo[1], n.lo[2]]).collect();
            let w: Vec<[f32; 4]> = rec.table.iter().flat_map(|n| [[n.w[0][0], n.w[0][1], n.w[1][0], n.w[1][1]], [n.w[2][0], n.w[2][1], 0.0, 0.0]]).collect();
            let (ib, wb) = (storage_init(&self.device, "nodes", &idx), storage_init(&self.device, "weights", &w));
            let bytes = total as u64 * 12 * self.samples_per_batch;
            let out = storage(&self.device, "node dump", bytes, wgpu::BufferUsages::empty());
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("node dump back"), size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let body = if d.magnetic {
                // H along a on the four faces around the node's line, in the other two axes.
                "  let vx = (fx[p] + fx[n.z] + fx[n.w] + fx[n.z + n.w - p]) * 0.25 * a.x;\n  let vy = (fy[p] + fy[n.w] + fy[n.y] + fy[n.w + n.y - p]) * 0.25 * a.z;\n  let vz = (fz[p] + fz[n.y] + fz[n.z] + fz[n.y + n.z - p]) * 0.25 * b.x;\n"
            } else {
                "  let vx = fx[n.y] * a.x + fx[p] * a.y;\n  let vy = fy[n.z] * a.z + fy[p] * a.w;\n  let vz = fz[n.w] * b.x + fz[p] * b.y;\n"
            };
            let src = format!(
                "{}{}@compute @workgroup_size({WG})\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  if (id.x >= {total}u) {{ return; }}\n  let n = idx[id.x];\n  let p = n.x;\n  let a = wt[id.x * 2u];\n  let b = wt[id.x * 2u + 1u];\n{body}  let at = (id.x + {total}u * state[2]) * 3u;\n  out[at] = vx;\n  out[at + 1u] = vy;\n  out[at + 2u] = vz;\n}}\n",
                header(self.n),
                bindings(&[("fx", "array<f32>", false), ("fy", "array<f32>", false), ("fz", "array<f32>", false), ("idx", "array<vec4<u32>>", false), ("wt", "array<vec4<f32>>", false), ("out", "array<f32>", true), ("state", "array<u32>", false)]),
            );
            let base = if d.magnetic { 3 } else { 0 };
            let pass = maker.small("node dump", &src, &[(&self.fields[base], false), (&self.fields[base + 1], false), (&self.fields[base + 2], false), (&ib, false), (&wb, false), (&out, true), (&self.state, false)], [wg_count(total), 1, 1]);
            self.node_extract.push(NodeExtract { rec, pass, out, staging });
        }
    }

    pub fn node_planes(&self) -> Vec<NodePlane> {
        self.node_extract.iter().flat_map(|x| x.rec.planes()).collect()
    }

    pub fn recorder_planes(&self, which: usize) -> Vec<crate::record::Plane> {
        self.recorders[which].0.planes()
    }

    pub fn recorder_count(&self) -> usize {
        self.recorders.len()
    }

    /// Adds a wave port, which reads from now on and injects once driven.
    pub fn add_wave_port(&mut self, run: crate::waveport::WavePortRun) {
        self.wave_ports.push(run);
        // One invocation each, like a lumped port: a few thousand terms a step at most.
        let count = self.wave_ports.len() as u64;
        let bytes = self.batch * count * 8;
        let trace = storage(&self.device, "wave trace", bytes, wgpu::BufferUsages::empty());
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("wave trace back"), size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let maker = Maker { device: &self.device };
        let binds = bindings(&[("ex", "array<f32>", false), ("ey", "array<f32>", false), ("ez", "array<f32>", false), ("hx", "array<f32>", false), ("hy", "array<f32>", false), ("hz", "array<f32>", false), ("le", "array<vec4<u32>>", false), ("lh", "array<vec4<u32>>", false), ("state", "array<u32>", false), ("trace", "array<f32>", true)]);
        let packed = |list: &[(usize, usize, f64)]| -> Vec<[u32; 4]> { list.iter().map(|&(c, q, w)| [c as u32, q as u32, (w as f32).to_bits(), 0]).collect() };
        let mut read = Vec::with_capacity(self.wave_ports.len());
        for (pi, run) in self.wave_ports.iter().enumerate() {
            let (le, lh) = (packed(&run.read_e), packed(&run.read_h));
            let (eb, hb) = (storage_init(&self.device, "wave e", &le), storage_init(&self.device, "wave h", &lh));
            let src = format!(
                "{}{binds}@compute @workgroup_size(1)\nfn main() {{\n  let b = state[1];\n  var u = 0.0;\n  for (var k = 0u; k < {ne}u; k++) {{\n    let v = le[k];\n    var x = 0.0;\n    if (v.x == 0u) {{ x = ex[v.y]; }} else if (v.x == 1u) {{ x = ey[v.y]; }} else {{ x = ez[v.y]; }}\n    u += x * bitcast<f32>(v.z);\n  }}\n  var i = 0.0;\n  for (var k = 0u; k < {nh}u; k++) {{\n    let v = lh[k];\n    var x = 0.0;\n    if (v.x == 0u) {{ x = hx[v.y]; }} else if (v.x == 1u) {{ x = hy[v.y]; }} else {{ x = hz[v.y]; }}\n    i += x * bitcast<f32>(v.z);\n  }}\n  trace[(b * {count}u + {pi}u) * 2u] = u;\n  trace[(b * {count}u + {pi}u) * 2u + 1u] = i;\n}}\n",
                header(self.n),
                ne = le.len(),
                nh = lh.len(),
            );
            read.push(maker.small("wave read", &src, &[(&self.fields[0], false), (&self.fields[1], false), (&self.fields[2], false), (&self.fields[3], false), (&self.fields[4], false), (&self.fields[5], false), (&eb, false), (&hb, false), (&self.state, false), (&trace, true)], [1, 1, 1]));
        }
        self.wave_read = read;
        self.wave_trace = Some((trace, staging));
    }

    /// Drives a wave port with `pulse` in place of any lumped port.
    pub fn drive_wave(&mut self, asm: &Assembled, port: usize, pulse: Pulse) {
        self.pulse = Some(pulse);
        for pb in &self.port_buffers {
            let data: Vec<[u32; 4]> = pb.edge_data.iter().map(|v| [v[0], v[1], v[2], 0f32.to_bits()]).collect();
            self.queue.write_buffer(&pb.edges, 0, bytemuck_like::bytes_of(&data));
        }
        let run = &self.wave_ports[port];
        let dt = self.dt;
        let len = ((2.0 * pulse.delay() / dt).ceil() as usize + 4).min(16_000_000);
        let z0 = run.mode.z0;
        let delay = run.half_cell_delay();
        let at_h: Vec<f32> = (0..len).map(|n| pulse.at(n as f64 * dt) as f32).collect();
        let at_e: Vec<f32> = (0..len).map(|n| (pulse.at((n as f64 + 0.5) * dt - delay) / z0 * run.def.direction) as f32).collect();
        let (src_h, src_e) = (storage_init(&self.device, "wave at h", &at_h), storage_init(&self.device, "wave at e", &at_e));
        // These edges update from their own lists, which is where the CPU leaves them alone too.
        let mut own: std::collections::HashSet<(usize, usize)> = asm.rl.iter().map(|e| (e.axis, e.node)).chain(asm.debye.iter().map(|d| (d.axis, d.node))).collect();
        for p in &asm.ports {
            own.extend(p.edges.iter().map(|(n, _)| (p.axis, *n)));
        }
        let d = run.def.axis;
        let (t1, t2) = ((d + 1) % 3, (d + 2) % 3);
        let up = run.def.direction > 0.0;
        let sa = asm.grid.strides()[d];
        // The same targets and signs as the CPU's `wave_h` and `wave_e`, as one list per field.
        let mut to_h: [Vec<[u32; 2]>; 3] = Default::default();
        for &(comp, node, val) in &run.mode.e {
            let (target, sign) = if comp == t1 { (t2, if up { 1.0 } else { -1.0 }) } else { (t1, if up { -1.0 } else { 1.0 }) };
            let q = if up { node } else { node - sa };
            let c = dt * asm.imu[target][q];
            to_h[target].push([q as u32, ((sign * c * val) as f32).to_bits()]);
        }
        let mut to_e: [Vec<[u32; 2]>; 3] = Default::default();
        for &(comp, node, val) in &run.mode.h {
            let (target, sign) = if comp == t2 { (t1, if up { 1.0 } else { -1.0 }) } else { (t2, if up { -1.0 } else { 1.0 }) };
            let m = asm.me[target][node];
            let c = if m.is_finite() && m > 0.0 && !own.contains(&(target, node)) { dt / m } else { 0.0 };
            to_e[target].push([node as u32, ((sign * c * val) as f32).to_bits()]);
        }
        let maker = Maker { device: &self.device };
        let inject = |list: &[Vec<[u32; 2]>; 3], source: &wgpu::Buffer, base: usize| -> Vec<SmallPass> {
            let mut out = Vec::new();
            for (k, l) in list.iter().enumerate() {
                if l.is_empty() {
                    continue;
                }
                let b = storage_init(&self.device, "wave list", l);
                let src = format!(
                    "{}{}@compute @workgroup_size({WG})\nfn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n  if (id.x >= {n}u) {{ return; }}\n  let s = state[0];\n  var v = 0.0;\n  if (s < {len}u) {{ v = src[s]; }}\n  let e = list[id.x];\n  f[e.x] += bitcast<f32>(e.y) * v;\n}}\n",
                    header(self.n),
                    bindings(&[("f", "array<f32>", true), ("list", "array<vec2<u32>>", false), ("src", "array<f32>", false), ("state", "array<u32>", false)]),
                    n = l.len(),
                );
                out.push(maker.small("wave inject", &src, &[(&self.fields[base + k], true), (&b, false), (source, false), (&self.state, false)], [wg_count(l.len()), 1, 1]));
            }
            out
        };
        self.wave_inject_h = inject(&to_h, &src_h, 3);
        self.wave_inject_e = inject(&to_e, &src_e, 0);
    }

    /// Which port the pulse goes in at.
    pub fn drive(&mut self, port: usize, pulse: Pulse) {
        self.pulse = Some(pulse);
        self.wave_inject_h.clear();
        self.wave_inject_e.clear();
        // The waveform at half steps, for as long as the pulse lasts.
        let len = ((2.0 * pulse.delay() / self.dt).ceil() as usize + 2).min(16_000_000);
        let wave: Vec<f32> = (0..len).map(|n| pulse.at((n as f64 + 0.5) * self.dt) as f32).collect();
        self.queue.write_buffer(&self.wave, 0, bytemuck_like::bytes_of(&wave));
        self.wave_len = len;
        for (pi, pb) in self.port_buffers.iter().enumerate() {
            let data: Vec<[u32; 4]> = pb.edge_data.iter().zip(&pb.scale).map(|(v, s)| [v[0], v[1], v[2], if pi == port { s.to_bits() } else { 0f32.to_bits() }]).collect();
            self.queue.write_buffer(&pb.edges, 0, bytemuck_like::bytes_of(&data));
        }
    }

    /// Back to rest.
    pub fn reset(&mut self) {
        let mut enc = self.device.create_command_encoder(&Default::default());
        for b in self.fields.iter().chain(&self.psi) {
            if b.usage().contains(wgpu::BufferUsages::COPY_DST) {
                enc.clear_buffer(b, 0, None);
            }
        }
        enc.clear_buffer(&self.state, 0, None);
        self.queue.submit([enc.finish()]);
        for t in &mut self.traces {
            t.u.clear();
            t.i.clear();
        }
        for (r, _) in &mut self.recorders {
            r.clear();
        }
        for x in &mut self.node_extract {
            x.rec.clear();
        }
        for w in &mut self.wave_ports {
            w.u.clear();
            w.i.clear();
        }
        self.step = 0;
        self.peak_energy = 0.0;
    }

    fn read_back(&self, buffer: &wgpu::Buffer, bytes: u64) -> Result<Vec<f32>> {
        let slice = buffer.slice(0..bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| anyhow!("waiting for the GPU: {e}"))?;
        rx.recv().map_err(|_| anyhow!("the GPU's read back never came"))?.map_err(|e| anyhow!("reading from the GPU: {e}"))?;
        let v = bytemuck_like::f32s(&slice.get_mapped_range().map_err(|e| anyhow!("reading from the GPU: {e}"))?);
        buffer.unmap();
        Ok(v)
    }

    /// One batch of steps.
    pub fn advance_batch(&mut self) -> Result<()> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.clear_buffer(&self.state, 4, Some(8));
        // state[0] is the step, state[1] the step within the batch, state[2] the sample within it.
        self.queue.write_buffer(&self.state, 0, bytemuck_like::bytes_of(&[self.step as u32, 0u32, 0u32, 0u32]));
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for s in 1..=self.batch {
                for m in &self.h_main {
                    pass.set_pipeline(&m.pipeline);
                    pass.set_bind_group(0, &m.bind, &[]);
                    pass.dispatch_workgroups(m.dispatch[0], m.dispatch[1], m.dispatch[2]);
                }
                for p in self.h_small.iter().chain(&self.wave_inject_h) {
                    pass.set_pipeline(&p.pipeline);
                    pass.set_bind_group(0, &p.bind, &[]);
                    pass.dispatch_workgroups(p.dispatch[0], p.dispatch[1], p.dispatch[2]);
                }
                for m in &self.e_main {
                    pass.set_pipeline(&m.pipeline);
                    pass.set_bind_group(0, &m.bind, &[]);
                    pass.dispatch_workgroups(m.dispatch[0], m.dispatch[1], m.dispatch[2]);
                }
                for p in self.wave_inject_e.iter().chain(&self.e_small).chain(&self.ports_pass).chain(&self.wave_read) {
                    pass.set_pipeline(&p.pipeline);
                    pass.set_bind_group(0, &p.bind, &[]);
                    pass.dispatch_workgroups(p.dispatch[0], p.dispatch[1], p.dispatch[2]);
                }
                if s % self.every == 0 {
                    for x in &self.extract {
                        pass.set_pipeline(&x.pass.pipeline);
                        pass.set_bind_group(0, &x.pass.bind, &[]);
                        pass.dispatch_workgroups(x.pass.dispatch[0], x.pass.dispatch[1], x.pass.dispatch[2]);
                    }
                    for x in &self.node_extract {
                        pass.set_pipeline(&x.pass.pipeline);
                        pass.set_bind_group(0, &x.pass.bind, &[]);
                        pass.dispatch_workgroups(x.pass.dispatch[0], x.pass.dispatch[1], x.pass.dispatch[2]);
                    }
                }
                pass.set_pipeline(&self.tick.pipeline);
                pass.set_bind_group(0, &self.tick.bind, &[]);
                pass.dispatch_workgroups(1, 1, 1);
                if s % self.every == 0 && !(self.extract.is_empty() && self.node_extract.is_empty()) {
                    pass.set_pipeline(&self.sample_tick.pipeline);
                    pass.set_bind_group(0, &self.sample_tick.bind, &[]);
                    pass.dispatch_workgroups(1, 1, 1);
                }
            }
        }
        let trace_bytes = self.batch * self.traces.len().max(1) as u64 * 8;
        enc.copy_buffer_to_buffer(&self.trace, 0, &self.trace_staging, 0, trace_bytes);
        let wave_bytes = self.batch * self.wave_ports.len() as u64 * 8;
        if let Some((t, st)) = &self.wave_trace {
            enc.copy_buffer_to_buffer(t, 0, st, 0, wave_bytes);
        }
        for x in &self.extract {
            enc.copy_buffer_to_buffer(&x.out, 0, &x.staging, 0, x.cells as u64 * 20 * self.samples_per_batch);
        }
        for x in &self.node_extract {
            enc.copy_buffer_to_buffer(&x.out, 0, &x.staging, 0, x.rec.table.len() as u64 * 12 * self.samples_per_batch);
        }
        self.queue.submit([enc.finish()]);
        let data = self.read_back(&self.trace_staging, trace_bytes)?;
        let ports = self.traces.len().max(1);
        for b in 0..self.batch as usize {
            for (p, t) in self.traces.iter_mut().enumerate() {
                t.u.push(data[(b * ports + p) * 2] as f64);
                t.i.push(data[(b * ports + p) * 2 + 1] as f64);
            }
        }
        if let Some((_, st)) = &self.wave_trace {
            let wave = self.read_back(st, wave_bytes)?;
            let count = self.wave_ports.len();
            for b in 0..self.batch as usize {
                for (p, run) in self.wave_ports.iter_mut().enumerate() {
                    run.u.push(wave[(b * count + p) * 2] as f64);
                    run.i.push(wave[(b * count + p) * 2 + 1] as f64);
                }
            }
        }
        // Back to the recorders, which do the transforms in double precision.
        if !self.recorders.is_empty() {
            let mut per_window: Vec<Vec<f32>> = Vec::with_capacity(self.extract.len());
            for x in &self.extract {
                per_window.push(self.read_back(&x.staging, x.cells as u64 * 20 * self.samples_per_batch)?);
            }
            for sample in 0..self.samples_per_batch {
                let at = self.step + (sample + 1) * self.every;
                for (rec, ids) in &mut self.recorders {
                    for (wi, id) in ids.iter().enumerate() {
                        let cells = self.extract[*id].cells;
                        let from = sample as usize * cells * 5;
                        rec.add_cells(at, self.dt, wi, &per_window[*id][from..from + cells * 5]);
                    }
                }
            }
        }
        if !self.node_extract.is_empty() {
            let mut all: Vec<Vec<f32>> = Vec::with_capacity(self.node_extract.len());
            for x in &self.node_extract {
                all.push(self.read_back(&x.staging, x.rec.table.len() as u64 * 12 * self.samples_per_batch)?);
            }
            let (step, every, dt, samples) = (self.step, self.every, self.dt, self.samples_per_batch);
            for (x, data) in self.node_extract.iter_mut().zip(&all) {
                let total = x.rec.table.len() * 3;
                for sample in 0..samples {
                    let from = sample as usize * total;
                    x.rec.add(step + (sample + 1) * every, dt, &data[from..from + total]);
                }
            }
        }
        self.step += self.batch;
        Ok(())
    }

    /// Stored field energy.
    pub fn energy(&self) -> Result<f64> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for p in &self.energy_pass {
                pass.set_pipeline(&p.pipeline);
                pass.set_bind_group(0, &p.bind, &[]);
                pass.dispatch_workgroups(p.dispatch[0], p.dispatch[1], p.dispatch[2]);
            }
        }
        let bytes = (self.n[1] * self.n[2]) as u64 * 4 * 6;
        enc.copy_buffer_to_buffer(&self.energy_rows, 0, &self.energy_staging, 0, bytes);
        self.queue.submit([enc.finish()]);
        let v = self.read_back(&self.energy_staging, bytes)?;
        Ok(v.iter().map(|x| *x as f64).sum::<f64>() * 0.5 * self.dt)
    }

    /// Runs until the energy has fallen `end_db` below its peak, after the pulse has gone in.
    pub fn run(&mut self, end_db: f64, max_steps: u64, mut tick: impl FnMut(u64, f64) -> bool) -> Result<Stop> {
        let source_end = self.pulse.map_or(0.0, |p| 2.0 * p.delay());
        let limit = 10f64.powf(-end_db / 10.0);
        while self.step < max_steps {
            self.advance_batch()?;
            let w = self.energy()?;
            if !w.is_finite() {
                return Ok(Stop::Diverged);
            }
            let t = self.step as f64 * self.dt;
            // As in the CPU engine: past the source the peak stops following, or this could never fire.
            if t > source_end && w > 2.0 * self.peak_energy.max(1e-300) {
                return Ok(Stop::Diverged);
            }
            if t <= source_end {
                self.peak_energy = self.peak_energy.max(w);
            }
            let db = if self.peak_energy > 0.0 { 10.0 * (w / self.peak_energy).max(1e-300).log10() } else { 0.0 };
            if !tick(self.step, db) {
                return Ok(Stop::Cancelled);
            }
            if t > source_end && w < self.peak_energy * limit {
                return Ok(Stop::Decayed);
            }
        }
        Ok(Stop::StepLimit)
    }
}
