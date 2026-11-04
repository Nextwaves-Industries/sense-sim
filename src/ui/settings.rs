//! Settings dialog: category list on the left, setting rows on the right.

use bevy_egui::egui::{self, RichText, vec2};

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
    Updates,
}

impl SettingsTab {
    const ALL: [SettingsTab; 6] = [Self::General, Self::Viewport, Self::Graphics, Self::Performance, Self::Assistant, Self::Updates];

    fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::General => (icon::FOLDER_OPEN, "Library"),
            Self::Viewport => (icon::SUN, "Viewport"),
            Self::Graphics => (icon::CUBE_FOCUS, "Graphics & GPU"),
            Self::Performance => (icon::GAUGE, "Performance"),
            Self::Assistant => (icon::SPARKLE, "AI assistant"),
            Self::Updates => (icon::ARROWS_CLOCKWISE, "Updates"),
        }
    }

    fn about(self) -> &'static str {
        match self {
            Self::General => "Where assets come from.",
            Self::Viewport => "How the 3D scene is drawn and edited.",
            Self::Graphics => "The GPU the app runs on, and how hard it works.",
            Self::Performance => "What's kept in memory.",
            Self::Assistant => "The model behind the AI Assistant, and what it may do.",
            Self::Updates => "Sense Sim checks for a new release each time it opens.",
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
    updater: &mut crate::updater::Updater,
) -> bool {
    let mut open = true;
    let screen = ctx.content_rect();
    let size = vec2(960.0f32.min(screen.width() - 48.0), 760.0f32.min(screen.height() - 48.0));
    let modal = egui::Modal::new(egui::Id::new("settings_modal")).backdrop_color(theme::BACKDROP).frame(theme::modal_frame()).show(ctx, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        if theme::modal_title(ui, size.x, "Settings", true, "Close (Esc)") {
            open = false;
        }
        let (body, _) = ui.allocate_exact_size(vec2(size.x, size.y - 56.0), egui::Sense::hover());
        let nav_w = 220.0;
        let nav = egui::Rect::from_min_size(body.min, vec2(nav_w, body.height()));
        let content = egui::Rect::from_min_max(egui::pos2(nav.right() + 24.0, body.top() + 18.0), body.max - vec2(10.0, 2.0));
        // The groups are white cards, so what they sit on isn't.
        ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(nav.right(), body.top()), body.max), 0.0, theme::RAISED);
        ui.painter().vline(nav.right(), body.y_range(), egui::Stroke::new(1.0, theme::STROKE));

        // Category list.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(nav));
        child.spacing_mut().item_spacing = vec2(0.0, 0.0);
        for t in SettingsTab::ALL {
            let (ic, name) = t.label();
            if theme::nav_item(&mut child, ic, name, *tab == t, nav_w - 1.0).clicked() {
                *tab = t;
            }
        }

        // Content.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content));
        child.spacing_mut().item_spacing = vec2(8.0, 6.0);
        child.label(RichText::new(tab.label().1).font(theme::bold(18.0)));
        child.label(RichText::new(tab.about()).color(theme::WEAK));
        child.add_space(12.0);
        egui::ScrollArea::vertical().id_salt(*tab as u8).auto_shrink([false, false]).show(&mut child, |ui| {
            // Room for the scroll bar, which would otherwise lie over the cards' controls.
            egui::Frame::new().inner_margin(egui::Margin { right: 14, ..Default::default() }).show(ui, |ui| match tab {
                SettingsTab::General => general(ui, library),
                SettingsTab::Viewport => viewport(ui, view),
                SettingsTab::Graphics => graphics(ui, gpu),
                SettingsTab::Performance => performance(ui, cache),
                SettingsTab::Assistant => crate::agent::panel::settings_ui(ui, agent, library, catalog),
                SettingsTab::Updates => updater.settings(ui),
            });
        });
    });
    open && !modal.should_close()
}

fn general(ui: &mut egui::Ui, library: &mut Library) {
    theme::setting_group(ui, "Dataset", |ui| {
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
    });
    theme::setting_group(ui, "Online catalog", |ui| {
        let status = if library.remote_status.is_empty() { "NVIDIA SimReady on Hugging Face".to_string() } else { library.remote_status.clone() };
        theme::setting_row(ui, "Hugging Face dataset", &status, |ui| {
            if ui.button(theme::label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
                library.refresh_remote();
                library.rescan();
            }
        });
        theme::setting_note(ui, "Set HF_TOKEN (or run `hf auth login`) for higher rate limits.", theme::WEAK);
    });
}

fn viewport(ui: &mut egui::Ui, view: &mut ViewSettings) {
    theme::setting_group(ui, "Display", |ui| {
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
        theme::setting_row(ui, "Sample when zoomed out", "Draw far objects with fewer triangles and skip parts under a pixel, with no visible change. Clicks and simulations still use the full geometry.", |ui| {
            theme::toggle_switch(ui, &mut view.sampling);
        });
        theme::setting_row(ui, "Occlusion culling", "Skip parts hidden behind others. Speeds up scenes with walls and rooms, but can slow open ones.", |ui| {
            theme::toggle_switch(ui, &mut view.occlusion);
        });
    });
    theme::setting_group(ui, "Lighting", |ui| {
        theme::setting_row(ui, "Sun", "Directional light in lux.", |ui| {
            ui.add(egui::Slider::new(&mut view.sun_lux, 0.0..=30000.0).fixed_decimals(0));
        });
        theme::setting_row(ui, "Environment", "Ambient sky light intensity.", |ui| {
            ui.add(egui::Slider::new(&mut view.env_intensity, 0.0..=6000.0).fixed_decimals(0));
        });
    });
    theme::setting_group(ui, "Editing", |ui| {
        theme::setting_row(ui, "Snap step", "Grid step while dragging with Ctrl held.", |ui| {
            ui.add(egui::Slider::new(&mut view.snap, 0.05..=1.0).suffix(" m"));
        });
    });
}

fn graphics(ui: &mut egui::Ui, gpu: &mut Gpu) {
    let before = gpu.prefs.clone();
    theme::setting_group(ui, "Active GPU", |ui| {
        match gpu.active.clone() {
            Some(a) => {
                let rt = if a.ray_tracing { "Hardware ray tracing (RTX): supported" } else { "Hardware ray tracing: not available" };
                theme::setting_row(ui, &a.name, &format!("{} · {} · {}\n{rt}", a.kind, a.backend, a.driver), |ui| {
                    let (text, color) = if a.software {
                        ("Software", theme::DANGER)
                    } else if a.discrete {
                        ("Discrete", theme::SUCCESS)
                    } else {
                        (a.kind.as_str(), theme::WARNING)
                    };
                    ui.label(RichText::new(text).color(color).strong());
                });
                if a.software {
                    theme::setting_note(ui, "No hardware GPU is in use. Install or update the graphics driver, or pick another adapter below.", theme::DANGER);
                }
            }
            None => {
                ui.add_space(8.0);
                theme::setting_note(ui, "Starting the renderer…", theme::WEAK);
            }
        }
        if gpu.recovered {
            theme::setting_note(ui, "The last start with the chosen GPU or backend failed, so Sense Sim reset them to Auto.", theme::WARNING);
        }
    });

    theme::setting_group(ui, "Renderer (applies after a restart)", |ui| {
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
            theme::setting_row(ui, "Restart Sense Sim to switch the GPU.", "", |ui| {
                if ui.button(theme::label(icon::ARROW_CLOCKWISE, "Restart now")).clicked() {
                    gpu.restart = true;
                }
            });
        }
    });

    theme::setting_group(ui, "Frame rate", |ui| {
        theme::setting_row(ui, "Vertical sync", "Off renders as fast as the GPU can, which uses more power.", |ui| {
            theme::toggle_switch(ui, &mut gpu.prefs.vsync);
        });
        theme::setting_row(ui, "Frame cap", &format!("Limit the frame rate to save GPU power. Now {:.0} fps.", gpu.fps()), |ui| {
            theme::segmented(ui, &mut gpu.prefs.fps_limit, &[(0, "Off"), (30, "30"), (60, "60"), (120, "120"), (144, "144")]);
        });
    });

    theme::setting_group(ui, "AI vision models", |ui| {
        theme::setting_row(ui, "Run on", "DirectML on the chosen GPU, or the CPU. Switching between GPU and CPU applies after a restart.", |ui| {
            theme::segmented(ui, &mut gpu.prefs.vision, &[(VisionDevice::Gpu, "Fast GPU"), (VisionDevice::LowPower, "Power-saving GPU"), (VisionDevice::Cpu, "CPU")]);
        });
    });

    theme::setting_group(ui, "Live telemetry", |ui| {
        let t = gpu.telemetry();
        if let Some(e) = &t.error {
            ui.add_space(8.0);
            theme::setting_note(ui, e, theme::WEAK);
        } else if t.gpus.is_empty() {
            ui.add_space(8.0);
            theme::setting_note(ui, "Reading…", theme::WEAK);
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
    });

    theme::setting_group(ui, "OpenEMS link", |ui| {
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
        let color = if link.ok { theme::SUCCESS } else if gpu.prefs.openems { theme::WARNING } else { theme::WEAK };
        theme::setting_note(ui, &status, color);
        if gpu.prefs.openems {
            theme::setting_row(ui, "Update every", "", |ui| {
                ui.add(egui::Slider::new(&mut gpu.prefs.openems_period_s, 0.5..=30.0).suffix(" s").fixed_decimals(1));
            });
            if link.ems_fps_limit > 0 {
                theme::setting_note(ui, &format!("OpenEMS caps the frame rate at {} fps (gpu0/FpsLimit).", link.ems_fps_limit), theme::WARNING);
            }
        }
    });

    if gpu.prefs != before {
        gpu.save();
    }
}

fn performance(ui: &mut egui::Ui, cache: &mut PrototypeCache) {
    theme::setting_group(ui, "Textures", |ui| {
        theme::setting_row(ui, "Max texture size", "Applies to assets loaded from now on.", |ui| {
            theme::segmented(ui, &mut cache.max_texture, &[(256, "256"), (512, "512"), (1024, "1K"), (2048, "2K"), (4096, "4K")]);
        });
    });
    theme::setting_group(ui, "Cache", |ui| {
        let loaded = cache.states.len();
        theme::setting_row(ui, "Loaded assets", &format!("{loaded} assets in memory. Placed objects keep their meshes when cleared."), |ui| {
            if ui.button(theme::label(icon::TRASH, "Clear cache")).clicked() {
                cache.clear();
            }
        });
    });
}
