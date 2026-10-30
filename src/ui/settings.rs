//! Settings dialog: category list on the left, setting rows on the right.

use bevy_egui::egui::{self, Margin, RichText, vec2};

use crate::agent::Agent;
use crate::agent::catalog::Catalog;
use crate::gpu::{Backend, Gpu, Power, VisionDevice};
use crate::library::Library;
use crate::prototype::PrototypeCache;
use crate::scene::ViewSettings;
use crate::theme::{self, icon};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Viewport,
    Graphics,
    Performance,
    Assistant,
}

impl SettingsTab {
    const ALL: [SettingsTab; 5] = [Self::General, Self::Viewport, Self::Graphics, Self::Performance, Self::Assistant];

    fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::General => (icon::FOLDER_OPEN, "Library"),
            Self::Viewport => (icon::SUN, "Viewport"),
            Self::Graphics => (icon::CUBE_FOCUS, "Graphics & GPU"),
            Self::Performance => (icon::GAUGE, "Performance"),
            Self::Assistant => (icon::SPARKLE, "AI assistant"),
        }
    }
}

/// Draws the dialog. Returns false once it should close.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    tab: &mut SettingsTab,
    library: &mut Library,
    view: &mut ViewSettings,
    cache: &mut PrototypeCache,
    agent: &mut Agent,
    catalog: &mut Catalog,
    gpu: &mut Gpu,
) -> bool {
    let mut open = true;
    let size = vec2(760.0f32.min(ctx.content_rect().width() - 40.0), 520.0f32.min(ctx.content_rect().height() - 60.0));
    let modal = egui::Modal::new(egui::Id::new("settings_modal"))
        .frame(egui::Frame::new().fill(theme::PANEL).stroke(egui::Stroke::new(1.0, theme::STROKE)).corner_radius(12).inner_margin(Margin::ZERO))
        .show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                // Category list.
                egui::Frame::new().fill(theme::BG).inner_margin(Margin::same(12)).corner_radius(egui::CornerRadius { nw: 12, sw: 12, ne: 0, se: 0 }).show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                    ui.set_width(180.0);
                    ui.set_min_height(size.y - 24.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(RichText::new("Settings").text_style(egui::TextStyle::Heading));
                    ui.add_space(8.0);
                    for t in SettingsTab::ALL {
                        let (ic, name) = t.label();
                        let sel = *tab == t;
                        let b = egui::Button::new(theme::label(ic, name))
                            .selected(sel)
                            .frame_when_inactive(false)
                            .min_size(vec2(ui.available_width(), 30.0));
                        if ui.add(b).clicked() {
                            *tab = t;
                        }
                    }
                    });
                });
                // Content.
                egui::Frame::new().inner_margin(Margin::symmetric(20, 14)).show(ui, |ui| {
                    ui.vertical(|ui| {
                    let w = size.x - 180.0 - 24.0 - 40.0;
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tab.label().1).size(17.0).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::icon_button(ui, icon::X, "Close (Esc)").clicked() {
                                open = false;
                            }
                        });
                    });
                    theme::divider(ui);
                    egui::ScrollArea::vertical().max_height(size.y - 80.0).auto_shrink([false, false]).show(ui, |ui| match tab {
                        SettingsTab::General => general(ui, library),
                        SettingsTab::Viewport => viewport(ui, view),
                        SettingsTab::Graphics => graphics(ui, gpu),
                        SettingsTab::Performance => performance(ui, cache),
                        SettingsTab::Assistant => crate::agent::panel::settings_ui(ui, agent, library, catalog),
                    });
                    });
                });
            });
        });
    open && !modal.should_close()
}

fn general(ui: &mut egui::Ui, library: &mut Library) {
    theme::caption(ui, "Dataset");
    theme::setting_row(ui, "Asset folder", "Where SimReady assets live and where downloads go. Rescanned every 15 s.", |ui| {
        if ui.button(theme::label(icon::FOLDER_OPEN, "Browse…")).clicked()
            && let Some(p) = rfd::FileDialog::new().pick_folder()
        {
            library.set_root(p.to_string_lossy().replace('\\', "/"));
        }
    });
    let r = ui.add(egui::TextEdit::singleline(&mut library.root).desired_width(f32::INFINITY));
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let root = library.root.clone();
        library.set_root(root);
    }
    ui.add_space(10.0);
    theme::caption(ui, "Online catalog");
    let status = if library.remote_status.is_empty() { "NVIDIA SimReady on Hugging Face".to_string() } else { library.remote_status.clone() };
    theme::setting_row(ui, "Hugging Face dataset", &status, |ui| {
        if ui.button(theme::label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
            library.refresh_remote();
            library.rescan();
        }
    });
    ui.label(RichText::new("Set HF_TOKEN (or run `hf auth login`) for higher rate limits.").small().color(theme::WEAK));
}

fn viewport(ui: &mut egui::Ui, view: &mut ViewSettings) {
    theme::caption(ui, "Display");
    theme::setting_row(ui, "Grid", "Floor grid.", |ui| {
        theme::toggle_switch(ui, &mut view.grid);
    });
    theme::setting_row(ui, "Hover bounds", "Outline the object under the cursor.", |ui| {
        theme::toggle_switch(ui, &mut view.bounds);
    });
    theme::setting_row(ui, "Prevent overlap", "Placing and dragging won't let objects cut into each other.", |ui| {
        theme::toggle_switch(ui, &mut view.no_overlap);
    });
    theme::setting_row(ui, "Gravity", "Objects fall onto whatever is under them. Lifted or Alt-moved objects stay in the air.", |ui| {
        theme::toggle_switch(ui, &mut view.gravity);
    });
    theme::setting_row(ui, "Shadows", "Sun shadows. Turn off for speed in large scenes.", |ui| {
        theme::toggle_switch(ui, &mut view.shadows);
    });
    ui.add_space(6.0);
    theme::caption(ui, "Lighting");
    theme::setting_row(ui, "Sun", "Directional light in lux.", |ui| {
        ui.add(egui::Slider::new(&mut view.sun_lux, 0.0..=30000.0).fixed_decimals(0));
    });
    theme::setting_row(ui, "Environment", "Ambient sky light intensity.", |ui| {
        ui.add(egui::Slider::new(&mut view.env_intensity, 0.0..=6000.0).fixed_decimals(0));
    });
    ui.add_space(6.0);
    theme::caption(ui, "Editing");
    theme::setting_row(ui, "Snap step", "Grid step while dragging with Ctrl held.", |ui| {
        ui.add(egui::Slider::new(&mut view.snap, 0.05..=1.0).suffix(" m"));
    });
}

fn graphics(ui: &mut egui::Ui, gpu: &mut Gpu) {
    let before = gpu.prefs.clone();
    theme::caption(ui, "Active GPU");
    match gpu.active.clone() {
        Some(a) => {
            let rt = if a.ray_tracing { "Hardware ray tracing (RTX): supported" } else { "Hardware ray tracing: not available" };
            theme::setting_row(ui, &a.name, &format!("{} · {} · {}\n{rt}", a.kind, a.backend, a.driver), |ui| {
                let (text, color) = if a.software {
                    ("Software", theme::DANGER)
                } else if a.discrete {
                    ("Discrete", theme::SUCCESS)
                } else {
                    (a.kind.as_str(), theme::ACCENT)
                };
                ui.label(RichText::new(text).color(color).strong());
            });
            if a.software {
                ui.label(RichText::new("No hardware GPU is in use. Install or update the graphics driver, or pick another adapter below.").small().color(theme::DANGER));
            }
        }
        None => {
            ui.label(RichText::new("Starting the renderer…").small().color(theme::WEAK));
        }
    }
    if gpu.recovered {
        ui.label(RichText::new("The last start with the chosen GPU or backend failed, so Sense Sim reset them to Auto.").small().color(theme::ACCENT));
    }

    ui.add_space(6.0);
    theme::caption(ui, "Renderer (applies after a restart)");
    theme::setting_row(ui, "GPU preference", "High performance uses the discrete GPU (NVIDIA RTX) on machines that also have integrated graphics.", |ui| {
        theme::segmented(ui, &mut gpu.prefs.power, &[(Power::HighPerformance, "High performance"), (Power::LowPower, "Power saving")]);
    });
    theme::setting_row(ui, "Graphics API", "Auto picks DirectX 12 on Windows. Try Vulkan if you see driver problems.", |ui| {
        theme::segmented(ui, &mut gpu.prefs.backend, &[(Backend::Auto, "Auto"), (Backend::Dx12, "DX12"), (Backend::Vulkan, "Vulkan"), (Backend::Gl, "OpenGL")]);
    });
    let adapters = gpu.adapters();
    theme::setting_row(ui, "Adapter", "Use a specific GPU. Otherwise the preference above decides.", |ui| {
        let shown = if gpu.prefs.adapter.is_empty() { "Automatic".to_string() } else { gpu.prefs.adapter.clone() };
        egui::ComboBox::from_id_salt("gpu_adapter").selected_text(shown).width(230.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut gpu.prefs.adapter, String::new(), "Automatic");
            match &adapters {
                None => {
                    ui.label(RichText::new("Looking for GPUs…").color(theme::WEAK));
                }
                Some(list) => {
                    let mut names: Vec<(&str, &str)> = Vec::new();
                    for a in list {
                        if !names.iter().any(|(n, _)| *n == a.name) {
                            names.push((&a.name, &a.kind));
                        }
                    }
                    for (name, kind) in names {
                        let backends: Vec<&str> = list.iter().filter(|a| a.name == name).map(|a| a.backend.as_str()).collect();
                        ui.selectable_value(&mut gpu.prefs.adapter, name.to_string(), format!("{name}  ({kind}, {})", backends.join("/")));
                    }
                }
            }
        });
    });
    if gpu.restart_needed() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Restart Sense Sim to switch the GPU.").small().color(theme::ACCENT));
            if ui.button(theme::label(icon::ARROW_CLOCKWISE, "Restart now")).clicked() {
                gpu.restart = true;
            }
        });
        ui.add_space(6.0);
    }

    ui.add_space(6.0);
    theme::caption(ui, "Frame rate");
    theme::setting_row(ui, "Vertical sync", "Off renders as fast as the GPU can, which uses more power.", |ui| {
        theme::toggle_switch(ui, &mut gpu.prefs.vsync);
    });
    theme::setting_row(ui, "Frame cap", &format!("Limit the frame rate to save GPU power. Now {:.0} fps.", gpu.fps()), |ui| {
        theme::segmented(ui, &mut gpu.prefs.fps_limit, &[(0, "Off"), (30, "30"), (60, "60"), (120, "120"), (144, "144")]);
    });

    ui.add_space(6.0);
    theme::caption(ui, "AI vision models");
    theme::setting_row(ui, "Run on", "DirectML on the chosen GPU, or the CPU. Switching between GPU and CPU applies after a restart.", |ui| {
        theme::segmented(ui, &mut gpu.prefs.vision, &[(VisionDevice::Gpu, "Fast GPU"), (VisionDevice::LowPower, "Power-saving GPU"), (VisionDevice::Cpu, "CPU")]);
    });

    ui.add_space(6.0);
    theme::caption(ui, "Live telemetry");
    let t = gpu.telemetry();
    if let Some(e) = &t.error {
        ui.label(RichText::new(e).small().color(theme::WEAK));
    } else if t.gpus.is_empty() {
        ui.label(RichText::new("Reading…").small().color(theme::WEAK));
    }
    let f = |v: Option<f32>, unit: &str| v.map(|x| format!("{x:.0} {unit}")).unwrap_or_else(|| "–".into());
    for g in &t.gpus {
        let mem = match (g.mem_used_mb, g.mem_total_mb) {
            (Some(u), Some(t)) => format!("{:.1} / {:.1} GB", u / 1024.0, t / 1024.0),
            _ => "–".into(),
        };
        let desc = format!(
            "{} of {} · {} load · {} · memory {mem} · {} · fan {}",
            f(g.power_w, "W"),
            f(g.power_limit_w, "W"),
            f(g.util_pct, "%"),
            f(g.temp_c, "°C"),
            f(g.clock_mhz, "MHz"),
            f(g.fan_pct, "%"),
        );
        theme::setting_row(ui, &g.name, &desc, |ui| {
            ui.label(RichText::new(f(g.power_w, "W")).strong());
        });
    }

    ui.add_space(6.0);
    theme::caption(ui, "OpenEMS link");
    let link = gpu.link();
    let status = if !gpu.prefs.openems {
        "Off".to_string()
    } else if link.text.is_empty() {
        "Connecting…".to_string()
    } else {
        link.text.clone()
    };
    theme::setting_row(
        ui,
        "Send GPU to OpenEMS",
        "Writes GPU power, load and temperature to the gpu0 meter (Gpu.SenseSim) in the OpenEMS the RTLS outputs use. OpenEMS can write gpu0/FpsLimit to cap the frame rate.",
        |ui| {
            theme::toggle_switch(ui, &mut gpu.prefs.openems);
        },
    );
    let color = if link.ok { theme::SUCCESS } else if gpu.prefs.openems { theme::ACCENT } else { theme::WEAK };
    ui.label(RichText::new(status).small().color(color));
    if gpu.prefs.openems {
        theme::setting_row(ui, "Update every", "", |ui| {
            ui.add(egui::Slider::new(&mut gpu.prefs.openems_period_s, 0.5..=30.0).suffix(" s").fixed_decimals(1));
        });
        if link.ems_fps_limit > 0 {
            ui.label(RichText::new(format!("OpenEMS caps the frame rate at {} fps (gpu0/FpsLimit).", link.ems_fps_limit)).small().color(theme::ACCENT));
        }
    }

    if gpu.prefs != before {
        gpu.save();
    }
}

fn performance(ui: &mut egui::Ui, cache: &mut PrototypeCache) {
    theme::caption(ui, "Textures");
    theme::setting_row(ui, "Max texture size", "Applies to assets loaded from now on.", |ui| {
        theme::segmented(ui, &mut cache.max_texture, &[(256, "256"), (512, "512"), (1024, "1K"), (2048, "2K"), (4096, "4K")]);
    });
    ui.add_space(6.0);
    theme::caption(ui, "Cache");
    let loaded = cache.states.len();
    theme::setting_row(ui, "Loaded assets", &format!("{loaded} assets in memory. Placed objects keep their meshes when cleared."), |ui| {
        if ui.button(theme::label(icon::TRASH, "Clear cache")).clicked() {
            cache.clear();
        }
    });
}
