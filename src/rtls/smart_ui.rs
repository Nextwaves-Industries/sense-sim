//! The plans on the Deploy card: what the site was read as, what each anchor buys, the choices
//! as the simulator found them, why each anchor is there, and what the site asks of the antenna.

use bevy::prelude::*;
use bevy_egui::egui;
use egui::{RichText, pos2, vec2};
use serde_json::json;

use super::deploy::dist;
use super::planning::{Coverage, MapView};
use super::smart::{AntennaBrief, Day, PlanOption, SmartPlan, SmartRequest, SmartResult};
use super::ui::Page;
use crate::theme::{self, icon};

/// What the card reads and changes outside the plan itself.
pub struct Context<'a> {
    pub target_cm: f32,
    pub max_anchors: Option<usize>,
    pub cov: &'a mut Coverage,
    pub selected: &'a mut Option<Entity>,
    pub device_named: &'a dyn Fn(&str) -> Option<Entity>,
    pub outcomes: &'a mut crate::outcomes::ui::OutcomesUi,
    /// A page to go to once the card is drawn.
    pub go_to: &'a mut Option<Page>,
}

pub fn show(ui: &mut egui::Ui, smart: &mut SmartPlan, cx: &mut Context) {
    let busy = smart.busy().map(str::to_string);
    let label = busy.clone().unwrap_or_else(|| theme::label(icon::LIGHTNING, "Find the best plans"));
    let button = egui::Button::new(RichText::new(label).strong().size(15.0).color(theme::ON_ACCENT)).fill(if busy.is_some() { theme::FAINT } else { theme::ACCENT }).stroke(egui::Stroke::NONE).min_size(vec2(ui.available_width(), 36.0)).truncate();
    let hover = "Works out where tags go, searches for plans that hold on a busy shift, with worse materials and half-empty racks, tests them in the simulator and compares them with plain Auto deploy. Takes a minute or so.";
    if ui.add_enabled(busy.is_none(), button).on_hover_text(hover).clicked() {
        let costing = smart.costing;
        smart.start(SmartRequest { target_m: cx.target_cm / 100.0, max_anchors: cx.max_anchors, costing, ..Default::default() });
    }
    if busy.is_some() {
        ui.ctx().request_repaint();
        return;
    }
    let Some(result) = smart.result.as_ref() else { return };
    if let Some(why) = &result.failed {
        ui.label(RichText::new(format!("{} {why}", icon::WARNING_CIRCLE)).color(theme::DANGER));
        return;
    }
    let mut apply = None;
    let mut reprice = false;
    let mut costing = smart.costing;
    if result.stale {
        ui.label(RichText::new(format!("{} The scene has changed since these plans were tested, so they don't apply to it any more. Find plans again.", icon::WARNING)).color(theme::WARNING));
    }
    site_block(ui, result, cx);
    curve_chart(ui, result);
    theme::caption(ui, "Choices");
    if result.options.is_empty() {
        ui.label(RichText::new("No plan could be made from these mounting points.").color(theme::WARNING));
    }
    for (i, o) in result.options.iter().enumerate() {
        if option_card(ui, o, i, result) {
            apply = Some(i);
        }
    }
    if let Some(text) = &result.compared {
        egui::Frame::new().fill(theme::ACCENT_SOFT).corner_radius(4).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).small());
        });
    }
    if let Some(o) = result.applied.and_then(|i| result.options.get(i)) {
        why_block(ui, o, result.target_m, cx);
        weakest_block(ui, o);
    }
    brief_block(ui, &result.brief, result.target_m, cx);
    egui::CollapsingHeader::new(RichText::new("Prices").small().color(theme::WEAK)).id_salt("smart_prices").default_open(false).show(ui, |ui| {
        let mut edited = costing.edited;
        let mut edit = |ui: &mut egui::Ui, k: usize, title: &str, v: &mut f64| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).small().color(theme::WEAK));
                if ui.add(egui::DragValue::new(v).range(0.0..=100_000.0).speed(1.0)).changed() {
                    edited[k] = true;
                    reprice = true;
                }
            });
        };
        edit(ui, 0, "Anchor", &mut costing.anchor);
        edit(ui, 1, "Fitting one", &mut costing.fit);
        edit(ui, 2, "Cable per metre", &mut costing.cable_per_m);
        edit(ui, 3, "Lift, per high mount", &mut costing.lift_per_device);
        costing.edited = edited;
    });
    let mut foot = format!("Simulated, not validated on hardware. Traffic is worked out from the layout, not measured. Every plan gets the same routes and floor spots, {:.0} s a day, and has to hold on all {} runs; its figure is the worst of them, from runs that took no part in choosing it. A dead anchor means the one the plan leans on most.", result.test_s, super::smart::RUNS_PER_TEST);
    if result.prices_are_placeholders {
        foot.push_str(" Any price you haven't changed is a placeholder.");
    }
    ui.label(RichText::new(foot).small().color(theme::WEAK));
    for n in &result.notes {
        ui.label(RichText::new(format!("• {n}")).small().color(theme::WEAK));
    }
    if apply.is_some() {
        smart.apply = apply;
    }
    if reprice {
        smart.costing = costing;
        if let Some(r) = smart.result.as_mut() {
            r.recost(&costing);
        }
    }
}

fn site_block(ui: &mut egui::Ui, r: &SmartResult, cx: &mut Context) {
    theme::caption(ui, "What it read");
    let s = &r.site;
    let mut parts: Vec<String> = Vec::new();
    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if s.racks > 0 {
        parts.push(count(s.racks, "rack", "racks"));
    }
    if s.doors > 0 {
        parts.push(count(s.doors, "door", "doors"));
    }
    if s.vehicles > 0 {
        parts.push(count(s.vehicles, "vehicle", "vehicles"));
    }
    if s.drawn_routes > 0 {
        parts.push(count(s.drawn_routes, "drawn route", "drawn routes"));
    }
    parts.push(format!("{:.0} m² walkable", s.walkable_m2));
    ui.label(RichText::new(parts.join(" · ")).color(theme::WEAK));
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("The busiest fifth of the floor carries {:.0} % of the traffic", s.busiest_fifth * 100.0));
        let shown = cx.cov.visible && cx.cov.view == MapView::Traffic;
        if cx.cov.traffic.is_some() && ui.selectable_label(shown, theme::label(icon::MAP_TRIFOLD, "Show")).on_hover_text("Worked out from the layout, not measured").clicked() {
            if shown {
                cx.cov.view = MapView::Quality;
            } else {
                cx.cov.visible = true;
                cx.cov.view = MapView::Traffic;
            }
            cx.cov.dirty = true;
        }
    });
}

/// Predicted worst-day error against anchors, with the options as the simulator measured them.
fn curve_chart(ui: &mut egui::Ui, r: &SmartResult) {
    let rows: Vec<(f32, f32)> = r.curve.iter().filter_map(|c| c.predicted_worst_day_m.map(|p| (c.anchors as f32, p))).collect();
    if r.curve.len() < 2 {
        return;
    }
    theme::caption(ui, "What each anchor buys");
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 112.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::RAISED);
    let plot = egui::Rect::from_min_max(rect.min + vec2(34.0, 8.0), rect.max - vec2(8.0, 18.0));
    let simulated: Vec<(f32, f32, bool)> = r.options.iter().enumerate().filter_map(|(i, o)| o.worst_day().and_then(|d| d.1).map(|p| (o.set.anchors.len() as f32, p, r.recommended == Some(i)))).collect();
    let n_of = |c: &(f32, f32)| c.0;
    let x_lo = r.curve.iter().map(|c| c.anchors as f32).fold(f32::MAX, f32::min);
    let x_hi = r.curve.iter().map(|c| c.anchors as f32).fold(f32::MIN, f32::max).max(x_lo + 1.0);
    // Past three times the target it's only "far off": the scale keeps the target readable.
    let cap = r.target_m * 3.0;
    let y_hi = rows.iter().map(|c| c.1).chain(simulated.iter().map(|c| c.1)).fold(r.target_m * 1.5, f32::max).min(cap);
    let at = |n: f32, e: f32| pos2(plot.left() + (n - x_lo) / (x_hi - x_lo) * plot.width(), plot.bottom() - (e.min(y_hi) / y_hi) * plot.height());
    let target_y = at(x_lo, r.target_m).y;
    let dash = egui::Shape::dashed_line(&[pos2(plot.left(), target_y), pos2(plot.right(), target_y)], egui::Stroke::new(1.0, theme::WEAK), 4.0, 3.0);
    painter.extend(dash);
    let small = egui::FontId::proportional(10.5);
    painter.text(pos2(plot.left() - 4.0, target_y), egui::Align2::RIGHT_CENTER, dist(r.target_m), small.clone(), theme::WEAK);
    let line: Vec<egui::Pos2> = rows.iter().map(|c| at(n_of(c), c.1)).collect();
    if line.len() >= 2 {
        painter.add(egui::Shape::line(line, egui::Stroke::new(2.0, theme::ACCENT)));
    }
    // A ring lifts the dots off the line; the recommended one is the largest.
    for (n, p, best) in &simulated {
        let c = at(*n, *p);
        painter.circle_filled(c, if *best { 6.0 } else { 4.5 }, theme::RAISED);
        painter.circle_filled(c, if *best { 4.5 } else { 3.0 }, theme::TEXT);
    }
    let tick = |n: f32| painter.text(pos2(at(n, 0.0).x, plot.bottom() + 3.0), egui::Align2::CENTER_TOP, format!("{n:.0}"), small.clone(), theme::WEAK);
    tick(x_lo);
    tick(x_hi);
    painter.text(pos2(plot.center().x, plot.bottom() + 3.0), egui::Align2::CENTER_TOP, "anchors", small.clone(), theme::WEAK);
    if let Some(mouse) = response.hover_pos() {
        let near = r.curve.iter().min_by(|a, b| (at(a.anchors as f32, 0.0).x - mouse.x).abs().total_cmp(&(at(b.anchors as f32, 0.0).x - mouse.x).abs()));
        if let Some(c) = near {
            let x = at(c.anchors as f32, 0.0).x;
            painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], egui::Stroke::new(1.0, theme::FAINT));
            let text = format!(
                "{} anchors · ≈ {:.0}\nPredicted as modelled: {}\nPredicted worst day: {}",
                c.anchors,
                c.cost,
                c.predicted_as_modelled_m.map_or("too many gaps".to_string(), dist),
                c.predicted_worst_day_m.map_or("too many gaps".to_string(), dist)
            );
            response.on_hover_text_at_pointer(text);
        }
    }
    ui.label(RichText::new("Line: predicted p95 on the worst day. Dots: the choices below, as the simulator measured them.").small().color(theme::WEAK));
}

/// One choice. Returns whether it was clicked.
fn option_card(ui: &mut egui::Ui, o: &PlanOption, i: usize, r: &SmartResult) -> bool {
    let on_floor = r.applied == Some(i);
    let stroke = if on_floor { egui::Stroke::new(1.5, theme::ACCENT) } else { egui::Stroke::new(1.0, theme::FAINT) };
    let frame = egui::Frame::new().stroke(stroke).corner_radius(4).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(o.kind.name()).strong());
            if r.recommended == Some(i) {
                theme::pill(ui, "Recommended", theme::ACCENT);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{} anchors · ≈ {:.0}", o.set.anchors.len(), o.cost)).color(theme::WEAK));
            });
        });
        let holds = o.holds_bad_days();
        let (mark, color) = if holds { (icon::CHECK_CIRCLE, theme::SUCCESS) } else { (icon::WARNING, theme::WARNING) };
        ui.label(RichText::new(format!("{mark} {}", o.verdict(r.target_m))).small().color(color));
        let show = |p: Option<f32>| p.map_or("too many gaps".to_string(), dist);
        let mut cells: Vec<String> = Vec::new();
        if let Some(t) = o.test(Day::AsModelled) {
            cells.push(format!("As modelled {}", show(t.p95_m)));
        }
        if let Some((day, p)) = o.worst_day().filter(|d| d.0 != Day::AsModelled) {
            cells.push(format!("{} {}", day.name(), show(p)));
        }
        if let Some(t) = o.test(Day::AnchorDown) {
            cells.push(format!("Dead anchor {}", show(t.p95_m)));
        }
        ui.label(RichText::new(cells.join(" · ")).small().color(theme::WEAK));
        // What the plan is judged on is where tags go, so what it leaves out has to show.
        if let Some((held, tried)) = o.test(Day::AnywhereOnFloor).and_then(|t| t.spots) {
            ui.label(RichText::new(format!("Anywhere on the floor: {held} of {tried} spots hold {}", dist(r.target_m))).small().color(theme::WEAK));
        }
        if on_floor {
            ui.label(RichText::new("On the floor").small().color(theme::ACCENT));
        }
    });
    let clicked = frame.response.interact(egui::Sense::click());
    !on_floor && clicked.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Put this plan on the floor").clicked()
}

fn why_block(ui: &mut egui::Ui, o: &PlanOption, target_m: f32, cx: &mut Context) {
    egui::CollapsingHeader::new(RichText::new("Why each anchor is here").strong()).id_salt("smart_why").default_open(true).show(ui, |ui| {
        let mut order: Vec<usize> = (0..o.set.anchors.len()).collect();
        order.sort_by(|a, b| o.set.anchors[*b].why.lost_share.total_cmp(&o.set.anchors[*a].why.lost_share));
        for i in order {
            let a = &o.set.anchors[i];
            ui.horizontal_wrapped(|ui| {
                let entity = (cx.device_named)(&a.id);
                let picked = entity.is_some() && *cx.selected == entity;
                let name = if a.id.is_empty() { format!("{} {}", a.mount, i + 1) } else { a.id.clone() };
                if ui.selectable_label(picked, RichText::new(name).small().strong()).on_hover_text(format!("On the {}. Click to select it.", a.mount)).clicked() {
                    *cx.selected = entity;
                }
                ui.label(RichText::new(a.why.text(target_m)).small());
            });
        }
    });
}

fn weakest_block(ui: &mut egui::Ui, o: &PlanOption) {
    let worst = o.tests.iter().filter(|t| t.day != Day::AnchorDown && t.weakest_at.is_some()).max_by(|a, b| a.p95_m.unwrap_or(f32::INFINITY).total_cmp(&b.p95_m.unwrap_or(f32::INFINITY)));
    if let Some(t) = worst {
        let near = t.weakest_near.as_ref().map_or(String::new(), |n| format!(" near {n}"));
        ui.label(RichText::new(format!("{} Weakest spot{near}, on: {}", icon::INFO, t.day.name())).small().color(theme::WEAK));
    }
}

fn brief_block(ui: &mut egui::Ui, b: &AntennaBrief, target_m: f32, cx: &mut Context) {
    if b.rows.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(RichText::new("What this site asks of the antenna").strong()).id_salt("smart_brief").default_open(false).show(ui, |ui| {
        let plural = if b.anchors == 1 { "anchor" } else { "anchors" };
        ui.label(format!("{} {} on the {}: 95 % of what they tell about position arrives within {:.0}° of where they face.", b.anchors, plural, b.mount, b.cone_deg));
        if b.cone_deg > 120.0 {
            ui.label(RichText::new("That's nearly all round: a narrow beam would lose positions these anchors are counted on for.").small().color(theme::WEAK));
        }
        egui::Grid::new("smart_brief_rows").num_columns(4).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
            for h in ["Off boresight", "Share", "Longest link", "Least gain"] {
                ui.label(RichText::new(h).small().color(theme::WEAK));
            }
            ui.end_row();
            for r in &b.rows {
                ui.label(RichText::new(format!("{:.0}–{:.0}°", r.from_deg, r.to_deg)).small());
                ui.label(RichText::new(format!("{:.0} %", r.share * 100.0)).small());
                ui.label(RichText::new(dist(r.longest_m)).small());
                ui.label(RichText::new(format!("{:+.0} dBi", r.need_dbi)).small());
                ui.end_row();
            }
        });
        ui.label(format!("Its shape can shift a range by about {} across that cone before {} is at risk (geometry ×{:.1}).", dist(b.shift_budget_m), dist(target_m), b.hdop));
        ui.label(RichText::new("Least gain closes the longest line-of-sight link in that band, tag antenna included. A catalogue pick would be judged against the horizon.").small().color(theme::WEAK));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut cx.outcomes.template).desired_width(110.0));
            let (lo, hi) = b.theta_range(super::smart::axis_is_facing(&cx.outcomes.template));
            if ui.button(theme::label(icon::BROADCAST, "Test this antenna")).on_hover_text("Solves the antenna and reports its range shift across this site's cone, against the budget above. Results are under Predictions.").clicked() {
                let channel = if b.freq_mhz > 7000.0 { 9 } else { 5 };
                cx.outcomes.pending.push(("design_uwb_bias".into(), json!({ "template": cx.outcomes.template, "channel": channel, "theta_min": lo, "theta_max": hi, "target_cm": b.shift_budget_m * 100.0 })));
                *cx.go_to = Some(Page::Outcomes);
            }
        });
    });
}
