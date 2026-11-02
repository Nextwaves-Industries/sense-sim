//! The project tree on the left, for the workbench in front: its own pages (each
//! shows how far it is set up and opens its detail card), the asset library,
//! downloads and the "Job status" card under it all.

use bevy_egui::egui::{self, Align, Layout, Margin, RichText, vec2};

use super::{LeftPanelSlot, UiState, Workbench, downloads, library};
use crate::library::Library;
use crate::prototype::PrototypeCache;
use crate::rtls::DevicePlace;
use crate::rtls::device::Profiles;
use crate::rtls::ui::{Page, RtlsUiState, Tab};
use crate::scene::PlaceMode;
use crate::theme::{self, Dot, TreeRow, icon};

const GAP: f32 = 8.0;

#[allow(clippy::too_many_arguments)]
pub fn left_card(
    ctx: &egui::Context,
    rect: egui::Rect,
    state: &mut UiState,
    lib: &mut Library,
    place: &mut PlaceMode,
    cache: &mut PrototypeCache,
    devices: (&Profiles, &mut DevicePlace),
    slot: &mut LeftPanelSlot,
    rtls: &mut RtlsUiState,
    agent_running: bool,
) {
    // The library takes whatever the other sections and the job card leave.
    let measure = egui::Id::new("left_card_rest");
    let rest = ctx.data(|d| d.get_temp::<f32>(measure)).unwrap_or(280.0);
    let library_h = (rect.height() - rest).max(150.0);

    let (profiles, device_place) = devices;
    let bench = slot.bench.current;
    let main = egui::Area::new(egui::Id::new("left_tree")).order(egui::Order::Middle).fixed_pos(rect.min).show(ctx, |ui| {
        theme::fixed_card(ui, rect.width(), rect.height(), |ui| {
            // ---- The workbench's own pages
            if bench != Workbench::Layout {
                let mut open = state.sections.bench;
                theme::section_header(ui, "bench", &mut open, bench.glyph(), theme::SUCCESS, bench.name(), |ui| {
                    if bench == Workbench::Simulation && slot.rtls_alerts > 0 {
                        theme::pill(ui, &format!("{} alarms", slot.rtls_alerts), theme::DANGER);
                    }
                });
                state.sections.bench = open;
                if open {
                    if bench == Workbench::Plant {
                        plant_row(ui, rect.width(), slot);
                    } else {
                        page_rows(ui, rect.width(), bench, slot, rtls);
                    }
                    ui.add_space(4.0);
                }
                theme::hairline(ui);
            }

            // ---- Library
            if bench.has_library() {
                let mut open = state.sections.library;
                theme::section_header(ui, "library", &mut open, icon::CUBE, theme::ACCENT, "Library", |ui| {
                    if theme::add_button(ui, "Choose the dataset folder").clicked()
                        && let Some(p) = rfd::FileDialog::new().pick_folder()
                    {
                        lib.set_root(p.to_string_lossy().replace('\\', "/"));
                    }
                });
                state.sections.library = open;
                if open {
                    ui.allocate_ui_with_layout(vec2(rect.width(), library_h), Layout::top_down(Align::Min), |ui| {
                        ui.set_height(library_h);
                        egui::Frame::new().inner_margin(Margin { left: 10, right: 8, top: 2, bottom: 6 }).show(ui, |ui| {
                            ui.set_height(library_h - 8.0);
                            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                            library::show(ui, lib, place, cache, state, (profiles, device_place));
                        });
                    });
                }
                theme::hairline(ui);
            }

            // ---- Downloads
            let list_len = {
                let (a, q) = lib.download_count();
                a + q
            };
            let mut open = state.sections.downloads;
            theme::section_header(ui, "downloads", &mut open, icon::CLOUD_ARROW_DOWN, theme::WEAK, &format!("Downloads ({list_len})"), |_| {});
            state.sections.downloads = open;
            if open {
                egui::Frame::new().inner_margin(Margin { left: 10, right: 10, top: 2, bottom: 8 }).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    downloads::show(ui, lib, state);
                });
            }
        });
    });

    // ---- Job status, its own small card under the tree.
    let jobs = collect_jobs(lib, cache, slot, agent_running);
    let jobs_area = egui::Area::new(egui::Id::new("job_status")).order(egui::Order::Middle).fixed_pos(egui::pos2(rect.left(), main.response.rect.bottom() + GAP)).show(ctx, |ui| {
        theme::fixed_card(ui, rect.width(), rect.height(), |ui| {
            let w = rect.width();
            let (r, resp) = ui.allocate_exact_size(vec2(w, 36.0), egui::Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, theme::RAISED);
            }
            ui.painter().text(r.left_center() + vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, "Job status", theme::bold(15.0), theme::TEXT);
            if !jobs.is_empty() {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(10.0, 0.0))).layout(Layout::right_to_left(Align::Center)));
                child.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
                child.label(RichText::new(format!("{}", jobs.len())).small().color(theme::WEAK));
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                state.sections.jobs = !state.sections.jobs;
            }
            if state.sections.jobs {
                theme::hairline(ui);
                egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if jobs.is_empty() {
                        ui.label(RichText::new("Nothing running.").small().color(theme::WEAK));
                    }
                    for j in &jobs {
                        ui.add(egui::Label::new(RichText::new(&j.text).small()).truncate());
                        if j.progress >= 0.0 {
                            ui.add(egui::ProgressBar::new(j.progress).desired_height(4.0).fill(theme::ACCENT).corner_radius(2));
                        }
                    }
                });
            }
        });
    });

    let library_used = if bench.has_library() && state.sections.library { library_h } else { 0.0 };
    let used = main.response.rect.height() - library_used + GAP + jobs_area.response.rect.height();
    ctx.data_mut(|d| d.insert_temp(measure, used));
}

fn collect_jobs(lib: &Library, cache: &PrototypeCache, slot: &LeftPanelSlot, agent_running: bool) -> Vec<super::Job> {
    let mut jobs = Vec::new();
    let loading = cache.loading_count();
    if loading > 0 {
        jobs.push(super::Job { text: format!("Loading {loading} asset{}", if loading == 1 { "" } else { "s" }), progress: -1.0 });
    }
    let (active, queued) = lib.download_count();
    if active + queued > 0 {
        jobs.push(super::Job { text: format!("Downloading {active}, {queued} queued"), progress: -1.0 });
    }
    if agent_running {
        jobs.push(super::Job { text: "Assistant working".into(), progress: -1.0 });
    }
    for j in &slot.jobs {
        jobs.push(super::Job { text: j.text.clone(), progress: j.progress });
    }
    jobs
}

/// The cards this workbench opens, each row showing how far it is set up.
fn page_rows(ui: &mut egui::Ui, w: f32, bench: Workbench, slot: &mut LeftPanelSlot, rtls: &mut RtlsUiState) {
    let page = rtls.local.page();
    let st = slot.rtls_status;
    let devices_note = match (st.devices, st.zones) {
        (0, 0) => String::new(),
        (d, 0) => d.to_string(),
        (d, z) => format!("{d} · {z} zones"),
    };
    let rows: Vec<(Page, &str, &str, Dot, &str)> = match bench {
        Workbench::Placement => vec![
            (Page::Deploy, "Deploy", icon::ROCKET_LAUNCH, st.deploy, ""),
            (Page::Tool(Tab::Devices), "Devices & zones", icon::BROADCAST, if st.devices > 0 { Dot::Done } else { Dot::Optional }, &devices_note),
            (Page::Tool(Tab::Inspector), "Inspector", icon::INFO, Dot::Optional, ""),
            (Page::Tool(Tab::Planning), "Planning", icon::MAP_TRIFOLD, Dot::Optional, ""),
            (Page::Tool(Tab::Settings), "Settings", icon::SLIDERS, Dot::Optional, ""),
        ],
        Workbench::Simulation => vec![
            (Page::Test, "Test live", icon::PLAY_CIRCLE, st.test, ""),
            (Page::Tool(Tab::Live), "Live data", icon::PULSE, Dot::Optional, ""),
            (Page::Tool(Tab::Rf), "RF analysis", icon::CHART_LINE, Dot::Optional, ""),
            (Page::Outcomes, "Predictions", icon::TARGET, Dot::None, ""),
            (Page::Tool(Tab::Outputs), "Outputs", icon::SHARE_NETWORK, Dot::Optional, ""),
            (Page::OpenEms, "OpenEMS Edge", icon::LIGHTNING, st.ems, ""),
            (Page::Fdtd, "RF solver (FDTD)", icon::WAVE_SINE, st.fdtd, ""),
        ],
        _ => Vec::new(),
    };
    for (p, text, glyph, dot, note) in rows {
        let selected = slot.rtls_open && page == p;
        let (c, _) = theme::tree_row(ui, w, &TreeRow { depth: 0, text, glyph, dot, selected, expander: None, guide: false, last: false, note, dim: false });
        if c.row {
            slot.rtls_open = !selected;
            if !selected {
                rtls.local.set_page(p);
            }
        }
    }
}

/// The plant in the scene: its controls are the strip at the bottom of the canvas.
fn plant_row(ui: &mut egui::Ui, w: f32, slot: &mut LeftPanelSlot) {
    let Some(running) = slot.plant else {
        egui::Frame::new().inner_margin(Margin::symmetric(12, 4)).show(ui, |ui| {
            ui.label(RichText::new("No plant yet. Place pumps, tanks or conveyors from the library.").small().color(theme::WEAK));
        });
        return;
    };
    let (c, r) = theme::tree_row(ui, w, &TreeRow {
        depth: 0,
        text: "Plant",
        glyph: icon::DROP,
        dot: if running { Dot::Done } else { Dot::Optional },
        selected: false,
        expander: None,
        guide: false,
        last: false,
        note: if running { "running" } else { "paused" },
        dim: false,
    });
    r.on_hover_text(if running { "Pause the plant" } else { "Run the plant" });
    if c.row {
        slot.plant_toggle = true;
    }
}
