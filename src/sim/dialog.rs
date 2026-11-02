//! The Create Simulation dialog: every analysis type, or a way in by what you're doing.

use bevy_egui::egui::{self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, StrokeKind, vec2};
use sim_core::catalog::{self, Analysis, AnalysisId, Group, Needs, Readiness, WORKFLOWS};

use crate::theme::{self, ACCENT, icon};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Analysis,
    Workflow,
}

#[derive(Default)]
pub struct Dialog {
    pub tab: Tab,
    pub analysis: Option<AnalysisId>,
    pub workflow: Option<usize>,
}

/// What can run here, for the dialog to say.
#[derive(Clone, Copy)]
pub struct Installed {
    pub native_fea: bool,
    pub wsl_available: bool,
    pub solvers_distro: bool,
}

impl Installed {
    fn says(&self, needs: Needs) -> Option<&'static str> {
        match needs {
            Needs::NativeFea if !self.native_fea => Some("Installs Gmsh and CalculiX on first run (about 100 MB)."),
            Needs::Foam | Needs::Elmer if !self.wsl_available => Some("Needs Windows Subsystem for Linux, which isn't available on this PC."),
            Needs::Foam | Needs::Elmer if !self.solvers_distro => Some("Sets up a private Linux environment with OpenFOAM and Elmer on first run (about 4 GB)."),
            _ => None,
        }
    }
}

fn readiness_chip(ui: &mut egui::Ui, r: Readiness) {
    let (text, color) = match r {
        Readiness::Verified => ("VERIFIED", theme::SUCCESS),
        Readiness::Experimental => ("BETA", ACCENT),
    };
    let font = egui::FontId::proportional(9.5);
    let w = ui.painter().layout_no_wrap(text.to_string(), font.clone(), color).size().x + 12.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
    ui.painter().rect(rect, 8.0, color.gamma_multiply(0.18), Stroke::NONE, StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
}

fn group_header(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::HOVER);
    ui.painter().text(rect.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, text.to_uppercase(), egui::FontId::proportional(11.0), theme::WEAK);
}

fn row(ui: &mut egui::Ui, a: &Analysis, selected: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let bg = if selected {
        theme::HOVER
    } else if r.hovered() {
        theme::HOVER.gamma_multiply(0.6)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    if selected {
        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, ACCENT);
    }
    ui.painter().text(rect.left_center() + vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, a.label, egui::FontId::proportional(13.5), theme::TEXT);
    let mut chip = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 7.0))).layout(Layout::right_to_left(Align::Center)));
    readiness_chip(&mut chip, a.readiness);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A band of the colour scale results use, standing in for a picture of a result.
fn preview(ui: &mut egui::Ui, group: Group) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::hover());
    let p = ui.painter();
    let n = 64;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let x0 = rect.left() + rect.width() * i as f32 / n as f32;
        let x1 = rect.left() + rect.width() * (i + 1) as f32 / n as f32;
        let [r, g, b] = super::view::colour(t);
        p.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())), CornerRadius::ZERO, Color32::from_rgb(r, g, b));
    }
    let glyph = match group {
        Group::Flow => icon::DROP,
        Group::Structural => icon::CUBE,
        Group::Electromagnetic => icon::BROADCAST,
    };
    p.text(rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(56.0), Color32::from_white_alpha(230));
}

fn details(ui: &mut egui::Ui, a: &Analysis, installed: Installed) {
    preview(ui, a.group);
    ui.add_space(10.0);
    egui::Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(a.label).size(18.0).strong());
            readiness_chip(ui, a.readiness);
        });
        ui.add_space(4.0);
        ui.label(a.summary);
        ui.add_space(8.0);
        ui.label(RichText::new("Solved with").color(theme::WEAK).size(11.5));
        ui.label(a.engine);
        ui.add_space(6.0);
        ui.label(RichText::new("Limits").color(theme::WEAK).size(11.5));
        ui.label(a.limits);
        if a.readiness == Readiness::Experimental {
            ui.add_space(6.0);
            ui.colored_label(ACCENT, "Its benchmarks aren't all met yet: check results independently before relying on them.");
        }
        if let Some(note) = installed.says(a.needs) {
            ui.add_space(6.0);
            ui.colored_label(theme::WEAK, note);
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for t in a.tags {
                egui::Frame::new().fill(theme::HOVER).corner_radius(4.0).inner_margin(Margin::symmetric(7, 2)).show(ui, |ui| {
                    ui.label(RichText::new(*t).size(11.0).color(theme::WEAK));
                });
            }
        });
    });
}

/// Draws the dialog. `Some(analysis)` when the user creates a simulation.
pub fn show(ctx: &egui::Context, d: &mut Dialog, installed: Installed, open: &mut bool) -> Option<AnalysisId> {
    let mut created = None;
    let modal = egui::Modal::new(egui::Id::new("create_simulation")).frame(egui::Frame::popup(&ctx.global_style()).inner_margin(Margin::ZERO)).show(ctx, |ui| {
        ui.set_width(1000.0);
        ui.set_height(640.0);
        egui::Frame::new().inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Create Simulation").size(17.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(icon::X).clicked() {
                        *open = false;
                    }
                });
            });
        });
        ui.separator();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let height = 520.0;
            // Tabs.
            ui.allocate_ui_with_layout(vec2(170.0, height), Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(vec2(170.0, height));
                for (tab, text) in [(Tab::Analysis, "Analysis Type"), (Tab::Workflow, "Workflow Type")] {
                    let (rect, r) = ui.allocate_exact_size(vec2(170.0, 34.0), Sense::click());
                    if d.tab == tab {
                        ui.painter().rect_filled(rect, 0.0, theme::HOVER);
                    }
                    ui.painter().text(rect.left_center() + vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(13.5), if d.tab == tab { ACCENT } else { theme::TEXT });
                    if r.clicked() {
                        d.tab = tab;
                    }
                }
            });
            ui.separator();
            // The list.
            ui.allocate_ui_with_layout(vec2(310.0, height), Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(vec2(310.0, height));
                egui::ScrollArea::vertical().id_salt("types").auto_shrink([false, false]).show(ui, |ui| match d.tab {
                    Tab::Analysis => {
                        for g in Group::ALL {
                            group_header(ui, g.label());
                            for a in catalog::ANALYSES.iter().filter(|a| a.group == g) {
                                if row(ui, a, d.analysis == Some(a.id)).clicked() {
                                    d.analysis = Some(a.id);
                                    d.workflow = None;
                                }
                            }
                        }
                    }
                    Tab::Workflow => {
                        for (i, w) in WORKFLOWS.iter().enumerate() {
                            let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
                            let sel = d.workflow == Some(i);
                            if sel || r.hovered() {
                                ui.painter().rect_filled(rect, 0.0, if sel { theme::HOVER } else { theme::HOVER.gamma_multiply(0.6) });
                            }
                            if sel {
                                ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, ACCENT);
                            }
                            ui.painter().text(rect.left_top() + vec2(12.0, 9.0), egui::Align2::LEFT_TOP, w.label, egui::FontId::proportional(13.5), theme::TEXT);
                            ui.painter().text(rect.left_top() + vec2(12.0, 29.0), egui::Align2::LEFT_TOP, catalog::analysis(w.analysis).label, egui::FontId::proportional(11.0), theme::WEAK);
                            if r.clicked() {
                                d.workflow = Some(i);
                                d.analysis = Some(w.analysis);
                            }
                        }
                    }
                });
            });
            ui.separator();
            // What's selected.
            ui.allocate_ui_with_layout(vec2(ui.available_width(), height), Layout::top_down(Align::Min), |ui| {
                ui.set_min_height(height);
                egui::ScrollArea::vertical().id_salt("detail").auto_shrink([false, false]).show(ui, |ui| match d.analysis {
                    Some(id) => {
                        let a = catalog::analysis(id);
                        if let Some(w) = d.workflow.map(|i| &WORKFLOWS[i]) {
                            egui::Frame::new().inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                                ui.label(RichText::new(w.label).size(15.0).strong());
                                ui.label(RichText::new(w.summary).color(theme::WEAK));
                            });
                        }
                        details(ui, a, installed);
                    }
                    None => {
                        ui.add_space(200.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("Choose what to simulate.").color(theme::WEAK));
                        });
                    }
                });
            });
        });
        ui.separator();
        egui::Frame::new().inner_margin(Margin::symmetric(16, 8)).show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let enabled = d.analysis.is_some();
                let create = ui.add_enabled(enabled, egui::Button::new(RichText::new("Create Simulation").strong()).min_size(vec2(160.0, 32.0)));
                if create.clicked() {
                    created = d.analysis;
                    *open = false;
                }
            });
        });
    });
    if modal.should_close() {
        *open = false;
    }
    created
}
