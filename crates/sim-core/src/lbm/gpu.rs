//! Runs the LBM shader on a GPU device of its own: Sense Sim's viewport and the CAD workspace keep
//! theirs, and a long solve here can't stall them through a shared queue.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytemuck::{Pod, Zeroable};

use super::shader::{self, C, opposite, weight};
use crate::jobs::Cancel;

/// What a run's cells are, in lattice units.
#[derive(Debug, Clone)]
pub struct Setup {
    pub size: [u32; 3],
    /// Kinematic viscosity, lattice units. Above zero; 0.0005 and up in practice.
    pub viscosity: f32,
    /// Smagorinsky constant. Zero: no turbulence model.
    pub smagorinsky: f32,
    pub force: [f32; 3],
    pub wall_velocity: [f32; 3],
    pub inlet_velocity: [f32; 3],
    /// Roughness length in cells: a log-law inlet with height (with `inlet_reference_height`
    /// giving where the velocity above applies). Zero: uniform inlet.
    pub inlet_z0: f32,
    pub inlet_reference_height: f32,
    /// A parabolic inlet across y, for a channel this many cells high with walls just outside
    /// cells 0 and this: `inlet_velocity` is the peak. Zero: uniform inlet.
    pub inlet_channel: f32,
    /// Drag per cell on `ROUGH_LAYER` cells: (kappa / ln((h + z0) / z0))^2 / dz, the log-law wall.
    pub rough: f32,
    /// Above zero, turbulence uses a mixing length growing with height up to this many cells, for
    /// boundary layers over a floor at z = 0. Needs `smagorinsky` above zero to switch the model on.
    pub mixing_max: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    nx: u32,
    ny: u32,
    nz: u32,
    n: u32,
    omega_plus: f32,
    omega_minus: f32,
    les: f32,
    /// Cells in one row of workgroups: a dispatch may only have 65535 groups per dimension.
    row: u32,
    fx: f32,
    fy: f32,
    fz: f32,
    pad1: f32,
    wall_x: f32,
    wall_y: f32,
    wall_z: f32,
    pad2: f32,
    inlet_x: f32,
    inlet_y: f32,
    inlet_z: f32,
    inlet_z0: f32,
    inlet_log_ref: f32,
    inlet_channel: f32,
    rough: f32,
    mixing_max: f32,
}

/// Per-cell values read back from the GPU.
#[derive(Debug, Clone, Default)]
pub struct Macros {
    pub rho: Vec<f32>,
    pub ux: Vec<f32>,
    pub uy: Vec<f32>,
    pub uz: Vec<f32>,
}

pub struct Lbm {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_name: String,
    size: [u32; 3],
    cells: u32,
    f: Vec<wgpu::Buffer>,
    flags: wgpu::Buffer,
    cell_force: wgpu::Buffer,
    params: wgpu::Buffer,
    outputs: [wgpu::Buffer; 4],
    group0: wgpu::BindGroup,
    group1: wgpu::BindGroup,
    even: wgpu::ComputePipeline,
    odd: wgpu::ComputePipeline,
    macros: wgpu::ComputePipeline,
    /// Steps done. A-A needs an even count before reading.
    steps: u64,
    /// Current batch size: adapts so each submission is short enough not to trip a driver reset.
    batch: u64,
}

/// Each submission is kept under this. Windows resets a GPU busy for 2 s.
const SUBMIT_BUDGET: Duration = Duration::from_millis(50);

/// The shader's `@workgroup_size`.
const WORKGROUP: u32 = 128;
/// Groups in one row of a dispatch, well under the 65535 a dimension may have.
const MAX_ROW_GROUPS: u32 = 32768;

impl Lbm {
    /// Workgroups to dispatch in x and y to cover `cells`.
    fn dispatch_size(cells: u32) -> (u32, u32) {
        let groups = cells.div_ceil(WORKGROUP);
        let gx = groups.min(MAX_ROW_GROUPS);
        (gx, groups.div_ceil(gx))
    }

    /// `flags`: one per cell, or empty for all fluid.
    pub fn new(setup: &Setup, flags: &[u32]) -> Result<Self> {
        let [nx, ny, nz] = setup.size;
        let cells = u64::from(nx) * u64::from(ny) * u64::from(nz);
        if cells == 0 {
            bail!("the grid has no cells");
        }
        if setup.viscosity <= 0.0 {
            bail!("viscosity must be positive, found {}", setup.viscosity);
        }
        if !flags.is_empty() && flags.len() as u64 != cells {
            bail!("{} flags for {cells} cells", flags.len());
        }
        let per_buffer = cells * 4;
        let instance = wgpu::Instance::new({
            let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
            d.backends = wgpu::Backends::VULKAN | wgpu::Backends::DX12;
            d
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).context("no GPU for the LBM solver")?;
        let limits = adapter.limits();
        if limits.max_storage_buffers_per_shader_stage < 25 {
            bail!("this GPU allows {} storage buffers per shader; the LBM solver needs 25", limits.max_storage_buffers_per_shader_stage);
        }
        if per_buffer > limits.max_storage_buffer_binding_size || per_buffer > limits.max_buffer_size {
            bail!("{cells} cells need {per_buffer} bytes per direction; this GPU allows {} per buffer", limits.max_storage_buffer_binding_size.min(limits.max_buffer_size));
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("LBM"),
            required_limits: wgpu::Limits {
                max_storage_buffers_per_shader_stage: 25,
                max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
                max_buffer_size: limits.max_buffer_size,
                max_bind_groups: 2,
                max_bindings_per_bind_group: 25,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        }))
        .context("opening a GPU device for the LBM solver")?;
        device.on_uncaptured_error(std::sync::Arc::new(|e| eprintln!("LBM GPU error: {e}")));
        let adapter_name = adapter.get_info().name;

        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        // A grid too big for what's free of the card's memory has to stop here, in words. Past here a
        // lost allocation would only show up as a run of nonsense.
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let f: Vec<wgpu::Buffer> = (0..19).map(|_| device.create_buffer(&wgpu::BufferDescriptor { label: Some("population"), size: per_buffer, usage: storage, mapped_at_creation: false })).collect();
        let flags_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("flags"), size: per_buffer, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let cell_force = device.create_buffer(&wgpu::BufferDescriptor { label: Some("cell force"), size: per_buffer, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let output = || device.create_buffer(&wgpu::BufferDescriptor { label: Some("macro"), size: per_buffer, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let outputs = [output(), output(), output(), output()];
        if let Some(e) = pollster::block_on(out_of_memory.pop()) {
            bail!("{cells} cells need {:.1} GB of GPU memory, which isn't free on {}. Use fewer cells across the body. ({e})", (per_buffer * 25) as f64 / 1e9, adapter.get_info().name);
        }
        if !flags.is_empty() {
            queue.write_buffer(&flags_buf, 0, bytemuck::cast_slice(flags));
        }

        let omega_plus = 1.0 / (3.0 * setup.viscosity + 0.5);
        // TRT with the "magic" product 3/16: (1/w+ - 1/2)(1/w- - 1/2) = 3/16, which places
        // bounce-back walls exactly halfway whatever the viscosity.
        let omega_minus = 1.0 / (0.5 + 0.1875 / (1.0 / omega_plus - 0.5));
        let log_ref = if setup.inlet_z0 > 0.0 { ((setup.inlet_reference_height + setup.inlet_z0) / setup.inlet_z0).ln() } else { 1.0 };
        let params = Params {
            nx,
            ny,
            nz,
            n: cells as u32,
            omega_plus,
            omega_minus,
            // 18 sqrt(2) C^2, the Smagorinsky term of the local relaxation time.
            les: 18.0 * std::f32::consts::SQRT_2 * setup.smagorinsky * setup.smagorinsky,
            row: Self::dispatch_size(cells as u32).0 * WORKGROUP,
            fx: setup.force[0],
            fy: setup.force[1],
            fz: setup.force[2],
            pad1: 0.0,
            wall_x: setup.wall_velocity[0],
            wall_y: setup.wall_velocity[1],
            wall_z: setup.wall_velocity[2],
            pad2: 0.0,
            inlet_x: setup.inlet_velocity[0],
            inlet_y: setup.inlet_velocity[1],
            inlet_z: setup.inlet_velocity[2],
            inlet_z0: setup.inlet_z0,
            inlet_log_ref: log_ref,
            inlet_channel: setup.inlet_channel,
            rough: setup.rough,
            mixing_max: setup.mixing_max,
        };
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("params"), size: std::mem::size_of::<Params>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));

        let storage_entry = |binding, read_only| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None }, count: None };
        let mut entries: Vec<wgpu::BindGroupLayoutEntry> = (0..19).map(|b| storage_entry(b, false)).collect();
        entries.push(storage_entry(19, true));
        entries.push(wgpu::BindGroupLayoutEntry { binding: 20, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None });
        entries.push(storage_entry(21, true));
        let layout0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("lbm 0"), entries: &entries });
        let out_entries: Vec<wgpu::BindGroupLayoutEntry> = (0..4).map(|b| storage_entry(b, false)).collect();
        let layout1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("lbm 1"), entries: &out_entries });
        let empty = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("lbm none"), entries: &[] });

        let mut bind: Vec<wgpu::BindGroupEntry> = f.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
        bind.push(wgpu::BindGroupEntry { binding: 19, resource: flags_buf.as_entire_binding() });
        bind.push(wgpu::BindGroupEntry { binding: 20, resource: params_buf.as_entire_binding() });
        bind.push(wgpu::BindGroupEntry { binding: 21, resource: cell_force.as_entire_binding() });
        let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("lbm 0"), layout: &layout0, entries: &bind });
        let bind1: Vec<wgpu::BindGroupEntry> = outputs.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
        let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("lbm 1"), layout: &layout1, entries: &bind1 });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("lbm"), source: wgpu::ShaderSource::Wgsl(shader::source().into()) });
        let step_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("lbm step"), bind_group_layouts: &[Some(&layout0), Some(&empty)], immediate_size: 0 });
        let macro_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("lbm macros"), bind_group_layouts: &[Some(&layout0), Some(&layout1)], immediate_size: 0 });
        let pipeline = |entry: &str, layout: &wgpu::PipelineLayout| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(entry), layout: Some(layout), module: &module, entry_point: Some(entry), compilation_options: Default::default(), cache: None })
        };
        let even = pipeline("even", &step_layout);
        let odd = pipeline("odd", &step_layout);
        let macros = pipeline("macros", &macro_layout);

        Ok(Self { device, queue, adapter_name, size: setup.size, cells: cells as u32, f, flags: flags_buf, cell_force, params: params_buf, outputs, group0, group1, even, odd, macros, steps: 0, batch: 2 })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn cells(&self) -> u32 {
        self.cells
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Replaces the cell flags, for instance to open a boundary later.
    pub fn set_flags(&self, flags: &[u32]) {
        self.queue.write_buffer(&self.flags, 0, bytemuck::cast_slice(flags));
    }

    /// A vertical (+z) force per cell, added to the uniform one: buoyancy.
    pub fn set_cell_force(&self, force: &[f32]) {
        self.queue.write_buffer(&self.cell_force, 0, bytemuck::cast_slice(force));
    }

    /// Writes a new parameter block's inlet, for example a changed wind direction.
    pub fn set_inlet(&self, velocity: [f32; 3]) {
        // inlet_x is the 16th 4-byte field.
        self.queue.write_buffer(&self.params, 64, bytemuck::cast_slice(&velocity));
    }

    /// Starts from density (as `rho - 1`, which keeps the precision of small differences) and
    /// velocity per cell, as the equilibrium for them.
    pub fn initialise(&mut self, drho: &[f32], u: &[[f32; 3]]) -> Result<()> {
        let n = self.cells as usize;
        if drho.len() != n || u.len() != n {
            bail!("initial state has {} and {} values for {n} cells", drho.len(), u.len());
        }
        let mut data = vec![0f32; n];
        for (i, (buffer, c)) in self.f.iter().zip(C.iter()).enumerate() {
            let w = weight(i) as f32;
            for (cell, slot) in data.iter_mut().enumerate() {
                let (d, [ux, uy, uz]) = (drho[cell], u[cell]);
                let cu = c[0] as f32 * ux + c[1] as f32 * uy + c[2] as f32 * uz;
                let usq = 1.5 * (ux * ux + uy * uy + uz * uz);
                *slot = w * (d + (1.0 + d) * (3.0 * cu + 4.5 * cu * cu - usq));
            }
            self.queue.write_buffer(buffer, 0, bytemuck::cast_slice(&data));
        }
        self.steps = 0;
        Ok(())
    }

    fn dispatches(&self, pass: &mut wgpu::ComputePass, steps: u64) {
        let (gx, gy) = Self::dispatch_size(self.cells);
        pass.set_bind_group(0, &self.group0, &[]);
        for k in 0..steps {
            pass.set_pipeline(if (self.steps + k).is_multiple_of(2) { &self.even } else { &self.odd });
            pass.dispatch_workgroups(gx, gy, 1);
        }
    }

    /// Advances `steps` time steps (rounded up to an even number, the only readable state), in
    /// short submissions. `on_progress` gets the steps done so far.
    pub fn run(&mut self, steps: u64, cancel: &Cancel, mut on_progress: impl FnMut(u64)) -> Result<()> {
        let goal = self.steps + steps + (steps % 2);
        let started_at = self.steps;
        while self.steps < goal {
            if cancel.is_cancelled() {
                bail!("cancelled");
            }
            let n = self.batch.min(goal - self.steps);
            let t0 = Instant::now();
            let mut encoder = self.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                self.dispatches(&mut pass, n);
            }
            self.queue.submit([encoder.finish()]);
            self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| anyhow!("the GPU stopped responding: {e}"))?;
            self.steps += n;
            let took = t0.elapsed();
            if took < SUBMIT_BUDGET / 2 {
                self.batch = (self.batch * 2).min(4096);
            } else if took > SUBMIT_BUDGET && self.batch > 2 {
                self.batch = (self.batch / 2).max(2);
            }
            on_progress(self.steps - started_at);
        }
        Ok(())
    }

    fn read_buffer(&self, buffer: &wgpu::Buffer) -> Result<Vec<f32>> {
        let size = buffer.size();
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("readback"), size, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| anyhow!("the GPU stopped responding: {e}"))?;
        rx.recv().context("reading from the GPU")?.context("reading from the GPU")?;
        let data = {
            let view = staging.slice(..).get_mapped_range().map_err(|e| anyhow!("reading from the GPU: {e:?}"))?;
            bytemuck::cast_slice::<u8, f32>(&view).to_vec()
        };
        staging.unmap();
        Ok(data)
    }

    /// Density and velocity of every cell. Needs an even number of steps, which `run` keeps.
    pub fn read_macros(&mut self) -> Result<Macros> {
        if !self.steps.is_multiple_of(2) {
            bail!("the state can only be read after an even number of steps");
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.macros);
            pass.set_bind_group(0, &self.group0, &[]);
            pass.set_bind_group(1, &self.group1, &[]);
            let (gx, gy) = Self::dispatch_size(self.cells);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        self.queue.submit([encoder.finish()]);
        let [rho, ux, uy, uz] = &self.outputs;
        Ok(Macros { rho: self.read_buffer(rho)?, ux: self.read_buffer(ux)?, uy: self.read_buffer(uy)?, uz: self.read_buffer(uz)? })
    }

    /// Force of the fluid on the wall cells for which `select(cell)`, by momentum exchange, in
    /// lattice units. At an even step a wall cell holds what has just arrived from the fluid: each
    /// such population `f_i` came in along `c_i` and will go back out along `-c_i`, handing the wall
    /// `2 c_i f_i`. What arrives at the wall is the thing to count, not what's at the fluid node
    /// beside it: that hasn't collided yet, and the difference is the viscous stress.
    pub fn wall_force(&self, flags: &[u32], select: impl Fn(usize) -> bool) -> Result<[f64; 3]> {
        if !self.steps.is_multiple_of(2) {
            bail!("the state can only be read after an even number of steps");
        }
        let f: Vec<Vec<f32>> = self.f.iter().map(|b| self.read_buffer(b)).collect::<Result<_>>()?;
        let [nx, ny, nz] = self.size.map(|v| v as i64);
        let index = |x: i64, y: i64, z: i64| ((x.rem_euclid(nx)) + nx * (y.rem_euclid(ny) + ny * z.rem_euclid(nz))) as usize;
        let mut force = [0.0f64; 3];
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let w = index(x, y, z);
                    if flags[w] & 15 != shader::WALL || !select(w) {
                        continue;
                    }
                    for (i, c) in C.iter().enumerate().skip(1) {
                        // Where this population came from.
                        let from = index(x - i64::from(c[0]), y - i64::from(c[1]), z - i64::from(c[2]));
                        if flags[from] & 15 == shader::WALL {
                            continue;
                        }
                        let arriving = f64::from(f[i][w]) + weight(i);
                        for a in 0..3 {
                            force[a] += 2.0 * f64::from(c[a]) * arriving;
                        }
                    }
                }
            }
        }
        Ok(force)
    }
}
