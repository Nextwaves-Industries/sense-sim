//! Hosts SolidCraft's whole UI off-screen, with no window and no eframe: a private egui context
//! (SolidCraft replaces fonts, theme and zoom on its context, so it cannot share ours), a private
//! `egui_wgpu::Renderer` (bevy_egui skips foreign paint callbacks, which is how SolidCraft draws
//! its viewport) and a wgpu device of its own, so a SolidCraft GPU error cannot reach Bevy's
//! error handler. Finished frames are read back as RGBA bytes for the embedding app to show.
//!
//! This file knows nothing about Bevy, so it also runs headless (`examples/design_host.rs`).
//! The frame loop follows `BlurFixture` in SolidCraft's `crates/ui-egui/src/gpu.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result};
use egui_wgpu::wgpu;
use solidcraft_engine::Session;
use solidcraft_ui_egui::{Services, SolveApp};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// As SolidCraft's desktop app: a stencil lets the section cap hide the inside of cut bodies.
const STENCIL_BITS: u8 = 8;
const FLOAT_DEPTH_BITS: u8 = 32;
const DEPTH_BITS: u8 = 24;
/// Readbacks in flight. When all are busy a frame simply isn't shown; the UI never waits.
const RING: usize = 3;
/// The workbench canvas's colour, so a model sits on the same ground as the scene.
const CANVAS: [u8; 3] = [242, 243, 244];

const FREE: u8 = 0;
const MAPPING: u8 = 1;
const READY: u8 = 2;

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// The frame this slot holds, to show the newest of those ready.
    frame: u64,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 2],
    stride: u32,
    ring: Vec<Slot>,
}

impl Target {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let size = [size[0].max(1), size[1].max(1)];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("design host output"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let stride = (size[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let ring = (0..RING)
            .map(|_| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("design host readback"),
                    size: u64::from(stride) * u64::from(size[1]),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                frame: 0,
            })
            .collect();
        Self { texture, view, size, stride, ring }
    }
}

/// What a frame asks of the embedding app.
#[derive(Default)]
pub struct FrameOutput {
    pub platform: egui::PlatformOutput,
    /// When the UI next wants to draw. `Duration::MAX`: only when there is input.
    pub repaint_after: Duration,
    /// A screenshot was asked for (issue reports); the host has none to give.
    pub wants_screenshot: bool,
    /// Time spent in the UI and in recording GPU work, in milliseconds.
    pub cost_ms: f64,
}

pub struct DesignHost {
    pub app: SolveApp,
    pub ctx: egui::Context,
    rs: egui_wgpu::RenderState,
    target: Target,
    frames: u64,
    started: std::time::Instant,
    adapter_name: String,
    gpu_viewport: bool,
}

impl DesignHost {
    /// Starts SolidCraft on `session`, drawing `size` pixels. `gpu_viewport: false` is the
    /// fallback: the model is drawn by SolidCraft's CPU rasteriser, the UI still by the GPU.
    pub fn new(session: Session, size: [u32; 2], gpu_viewport: bool) -> Result<Self> {
        let (rs, depth_bits) = render_state()?;
        let ctx = egui::Context::default();
        solidcraft_ui_egui::fonts::setup_bundled_fonts(&ctx);
        solidcraft_ui_egui::fonts::load_system_fonts_in_background(&ctx);

        // `Services::default()`: no update checks, agent chat, slicer upload or control port.
        let mut app = SolveApp::new(session, Services::default());
        app.apply_off_thread = Some(solidcraft_ui_egui::dialogs::APPLY_AT_ONCE);
        app.custom_titlebar = false;
        app.native_resize_border = false;
        app.integrated_titlebar = false;
        // One window, one look: Sense Sim draws the bars, and it's light. None of this is saved: the
        // embedded app never writes SolidCraft's preferences.
        app.host_chrome = true;
        app.ui.dark = false;
        app.preferences.theme = "light".into();
        app.preferences.viewport_background = Some(CANVAS);
        if gpu_viewport {
            let depth = egui_wgpu::depth_format_from_bits(depth_bits, STENCIL_BITS);
            let supported = egui_wgpu::supported_msaa_samples(&rs.adapter, &rs.device, FORMAT, depth);
            let samples = if supported.contains(&4) { 4 } else { 1 };
            app.set_wgpu_split(&rs, depth_bits, STENCIL_BITS, samples);
            app.msaa_supported = supported;
            solidcraft_ui_egui::gpu::install_picture_renderer(&rs);
        }
        let adapter_name = rs.adapter.get_info().name;
        let target = Target::new(&rs.device, size);
        Ok(Self { app, ctx, rs, target, frames: 0, started: std::time::Instant::now(), adapter_name, gpu_viewport })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn gpu_viewport(&self) -> bool {
        self.gpu_viewport
    }

    pub fn size(&self) -> [u32; 2] {
        self.target.size
    }

    /// Changes the drawing size. Frames in flight are dropped.
    pub fn resize(&mut self, size: [u32; 2]) {
        let size = [size[0].max(1), size[1].max(1)];
        if size != self.target.size {
            self.target = Target::new(&self.rs.device, size);
        }
    }

    /// Runs the UI for one frame on `raw` and renders it. `native_ppp` is the display's scale;
    /// SolidCraft's own UI zoom comes on top of it. `raw.screen_rect` and `raw.time` are filled in
    /// if the caller left them empty.
    pub fn frame(&mut self, mut raw: egui::RawInput, native_ppp: f32) -> FrameOutput {
        let t0 = std::time::Instant::now();
        let [w, h] = self.target.size;
        let ppp = (native_ppp * self.ctx.zoom_factor()).max(0.1);
        raw.screen_rect.get_or_insert(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32 / ppp, h as f32 / ppp)));
        raw.time.get_or_insert(self.started.elapsed().as_secs_f64());
        if let Some(root) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
            root.native_pixels_per_point = Some(native_ppp);
            root.focused = Some(true);
        }
        self.app.raw_input_hook(&mut raw);

        let app = &mut self.app;
        let mut out = self.ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx());
            let content = ui.ctx().content_rect();
            ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| app.ui(ui));
        });

        let jobs = self.ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [w, h], pixels_per_point: out.pixels_per_point };
        let mut encoder = self.rs.device.create_command_encoder(&Default::default());
        let mut renderer = self.rs.renderer.write();
        for (id, deltas) in out.textures_delta.set.drain() {
            for delta in deltas {
                renderer.update_texture(&self.rs.device, &self.rs.queue, id, &delta);
            }
        }
        if let Some(effect) = &renderer.frame_compositor {
            effect.begin_viewport(egui::ViewportId::ROOT.0.value());
        }
        let extra = renderer.update_buffers(&self.rs.device, &self.rs.queue, &mut encoder, &jobs, &screen);
        let composed = renderer
            .frame_compositor
            .as_ref()
            .is_some_and(|effect| effect.render(&renderer, &mut encoder, &self.target.view, &jobs, &screen, [0.0, 0.0, 0.0, 1.0]));
        if !composed {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("design host UI"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.target.view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            renderer.render(&mut pass, &jobs, &screen);
        }

        // Copy into a free readback buffer, if there is one.
        let stride = self.target.stride;
        let free = self.target.ring.iter().position(|s| s.state.load(Ordering::Acquire) == FREE);
        if let Some(i) = free {
            encoder.copy_texture_to_buffer(
                self.target.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.target.ring[i].buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride), rows_per_image: Some(h) },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        self.rs.queue.submit(extra.into_iter().chain([encoder.finish()]));
        if let Some(effect) = &renderer.frame_compositor {
            effect.submitted();
        }
        for id in out.textures_delta.free.drain() {
            renderer.free_texture(&id);
        }
        drop(renderer);

        self.frames += 1;
        if let Some(i) = free {
            let slot = &mut self.target.ring[i];
            slot.frame = self.frames;
            slot.state.store(MAPPING, Ordering::Release);
            let state = slot.state.clone();
            slot.buffer.map_async(wgpu::MapMode::Read, .., move |r| state.store(if r.is_ok() { READY } else { FREE }, Ordering::Release));
        }
        let _ = self.rs.device.poll(wgpu::PollType::Poll);

        let root = out.viewport_output.get(&egui::ViewportId::ROOT);
        let wants_screenshot = root.is_some_and(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Screenshot(_))));
        let repaint_after = root.map_or(Duration::MAX, |v| v.repaint_delay);
        FrameOutput { platform: out.platform_output, repaint_after, wants_screenshot, cost_ms: t0.elapsed().as_secs_f64() * 1000.0 }
    }

    /// The newest finished frame as tightly packed RGBA, if one arrived since the last call.
    pub fn take_pixels(&mut self) -> Option<(Vec<u8>, [u32; 2])> {
        let _ = self.rs.device.poll(wgpu::PollType::Poll);
        let newest = self.target.ring.iter().enumerate().filter(|(_, s)| s.state.load(Ordering::Acquire) == READY).max_by_key(|(_, s)| s.frame).map(|(i, _)| i)?;
        let [w, h] = self.target.size;
        let stride = self.target.stride as usize;
        let row = w as usize * 4;
        let mut pixels = vec![0u8; row * h as usize];
        {
            let slot = &self.target.ring[newest];
            let mapped = slot.buffer.slice(..).get_mapped_range().ok()?;
            for (dst, src) in pixels.chunks_exact_mut(row).zip(mapped.chunks_exact(stride)) {
                dst.copy_from_slice(&src[..row]);
            }
        }
        let frame = self.target.ring[newest].frame;
        // This frame and anything older are done with.
        for slot in &self.target.ring {
            if slot.state.load(Ordering::Acquire) == READY && slot.frame <= frame {
                slot.buffer.unmap();
                slot.state.store(FREE, Ordering::Release);
            }
        }
        Some((pixels, [w, h]))
    }

    /// Waits for the GPU, then returns the newest frame. For tests and headless runs.
    pub fn wait_pixels(&mut self) -> Option<(Vec<u8>, [u32; 2])> {
        let _ = self.rs.device.poll(wgpu::PollType::wait_indefinitely());
        self.take_pixels()
    }

    /// What SolidCraft does on the way out: end a running save, wait for pictures still being
    /// drawn. Call before the process exits; `process::exit` would skip all of it.
    pub fn shutdown(&mut self) {
        solidcraft_ui_egui::save_job::tick(&mut self.app, true);
        let _ = solidcraft_engine::capture::close(Duration::from_secs(5));
    }
}

/// A device of our own on D3D12, the path SolidCraft's Windows CI exercises.
fn render_state() -> Result<(egui_wgpu::RenderState, u8)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
    // Never a `dxcompiler.dll` another program put on PATH.
    desc.backend_options.dx12.shader_compiler = solidcraft_gpu::dx12_compiler();
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }))
        .context("no GPU adapter for the design workspace")?;
    let features = solidcraft_gpu::depth_features(&adapter);
    let depth_bits = if features.contains(wgpu::Features::DEPTH32FLOAT_STENCIL8) { FLOAT_DEPTH_BITS } else { DEPTH_BITS };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("design host"),
        required_features: features,
        required_limits: wgpu::Limits { max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d, ..wgpu::Limits::default() },
        ..Default::default()
    }))
    .context("opening a GPU device for the design workspace")?;
    let renderer = egui_wgpu::Renderer::new(&device, FORMAT, Default::default());
    let msaa_supported = egui_wgpu::supported_msaa_samples(&adapter, &device, FORMAT, None);
    let device_lost = egui_wgpu::DeviceLost::watch(&device);
    let rs = egui_wgpu::RenderState {
        available_adapters: vec![adapter.clone()],
        adapter,
        instance,
        device,
        queue,
        target_format: FORMAT,
        msaa_samples: 1,
        msaa_supported,
        renderer: Arc::new(egui::epaint::mutex::RwLock::new(renderer)),
        surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
        device_lost,
    };
    Ok((rs, depth_bits))
}
