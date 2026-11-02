//! The Factory workbench's cards and its labels in the viewport.

use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use bevy_egui::EguiContexts;
use bevy_egui::egui::{self, Align2, Margin, RichText, vec2};
use twin_proto::model::{Coverage, Device, DeviceClass, DeviceId, Event, Liveness, Severity};

use super::lines::{LineList, lines_page};
use super::yard::Yard;
use super::{DEMO_SIZES, LinkStatus, TwinInventory, TwinMarker, TwinPage, TwinRequest, TwinScene, TwinSelection};
use crate::scene::OrbitCamera;
use crate::theme::{self, Dot, TreeRow, icon};
use crate::ui::{HideOverlays, LeftPanelSlot, Workbench};

/// How many of the nearest devices get a name label when labels are on.
const NEAREST_LABELS: usize = 24;
/// A plant in a bad way shouldn't paint the whole screen in labels.
const MAX_PROBLEM_LABELS: usize = 40;
const MAX_GROUP_LABELS: usize = 12;
const DETAILS_WIDTH: f32 = 320.0;
const DETAILS_TOP: f32 = 104.0;

pub fn class_glyph(class: DeviceClass) -> &'static str {
    match class {
        DeviceClass::Plc => icon::CPU,
        DeviceClass::Hmi => icon::MONITOR,
        DeviceClass::Pc => icon::DESKTOP,
        DeviceClass::Switch => icon::SHARE_NETWORK,
        DeviceClass::Drive => icon::GEAR,
        DeviceClass::Sensor => icon::BROADCAST,
        DeviceClass::IoLinkMaster => icon::TREE_STRUCTURE,
        DeviceClass::Gateway => icon::PLUGS,
        DeviceClass::Robot => icon::ROBOT,
        DeviceClass::Cnc => icon::CUBE,
        DeviceClass::Meter => icon::GAUGE,
        DeviceClass::Unknown => icon::INFO,
    }
}

fn status_dot(liveness: Liveness) -> Dot {
    match liveness {
        Liveness::Up => Dot::Done,
        Liveness::Down => Dot::Todo,
        Liveness::Suspect => Dot::Busy,
        Liveness::Unknown | Liveness::Stale | Liveness::Retired => Dot::Unknown,
    }
}

fn status_color(liveness: Liveness) -> egui::Color32 {
    match liveness {
        Liveness::Up => theme::SUCCESS,
        Liveness::Suspect => theme::WARNING,
        Liveness::Down => theme::DANGER,
        Liveness::Unknown | Liveness::Stale | Liveness::Retired => theme::WEAK,
    }
}

fn severity_color(severity: Severity) -> egui::Color32 {
    match severity {
        Severity::Info => theme::WEAK,
        Severity::Warning => theme::WARNING,
        Severity::Critical => theme::DANGER,
    }
}

/// `at start` for what a demo began with, otherwise how long ago. Relative, so nobody has to
/// wonder which time zone a plant event is in.
fn age(ts_ms: u64) -> String {
    if ts_ms == 0 {
        return "at start".to_string();
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let seconds = now.saturating_sub(ts_ms) / 1000;
    match seconds {
        0..=59 => format!("{seconds} s ago"),
        60..=3599 => format!("{} min ago", seconds / 60),
        3600..=86399 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86400),
    }
}

#[derive(Default)]
pub struct ListState {
    filter: String,
    class: Option<DeviceClass>,
    problems_only: bool,
    /// The rows for this filter, redone only when something changed.
    rows: Vec<DeviceId>,
    built_for: Option<(u64, String, Option<DeviceClass>, bool)>,
}

impl ListState {
    fn refresh(&mut self, inventory: &TwinInventory) {
        let key = (inventory.generation, self.filter.clone(), self.class, self.problems_only);
        if self.built_for.as_ref() == Some(&key) {
            return;
        }
        let needle = self.filter.to_lowercase();
        let mut rows: Vec<&Device> = inventory
            .mirror
            .devices
            .values()
            .filter(|d| d.liveness != Liveness::Retired)
            .filter(|d| self.class.is_none_or(|c| d.class == c))
            .filter(|d| !self.problems_only || matches!(inventory.shown_liveness(d), Liveness::Down | Liveness::Suspect))
            .filter(|d| needle.is_empty() || matches_filter(d, &needle))
            .collect();
        // Problems first: that's why anyone opens this list.
        rows.sort_by(|a, b| problem_rank(inventory.shown_liveness(a)).cmp(&problem_rank(inventory.shown_liveness(b))).then_with(|| a.name().cmp(b.name())));
        self.rows = rows.into_iter().map(|d| d.id.clone()).collect();
        self.built_for = Some(key);
    }
}

pub(super) fn problem_rank(liveness: Liveness) -> u8 {
    match liveness {
        Liveness::Down => 0,
        Liveness::Suspect => 1,
        Liveness::Stale | Liveness::Unknown => 2,
        Liveness::Up | Liveness::Retired => 3,
    }
}

fn matches_filter(device: &Device, needle: &str) -> bool {
    let identity = &device.identity;
    let texts = [device.name(), device.ip().unwrap_or(""), identity.vendor.as_deref().unwrap_or(""), identity.model.as_deref().unwrap_or(""), identity.order_number.as_deref().unwrap_or(""), identity.serial.as_deref().unwrap_or(""), device.class.label()];
    texts.iter().any(|t| t.to_lowercase().contains(needle))
}

/// What the Factory card asked for this frame.
#[derive(Default)]
pub(super) struct Asked {
    pub(super) select: Option<DeviceId>,
    pub(super) focus: Option<DeviceId>,
}

#[allow(clippy::too_many_arguments)]
pub fn twin_ui(
    mut contexts: EguiContexts,
    camera: Single<(&Camera, &GlobalTransform, &mut OrbitCamera)>,
    window: Single<&Window, With<bevy::window::PrimaryWindow>>,
    hide: Res<HideOverlays>,
    mut slot: ResMut<LeftPanelSlot>,
    inventory: Res<TwinInventory>,
    mut scene: ResMut<TwinScene>,
    mut selection: ResMut<TwinSelection>,
    yard: Res<Yard>,
    markers: Query<(&TwinMarker, &Transform)>,
    mut list: Local<ListState>,
    mut line_list: Local<LineList>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    if hide.0 > 0 || slot.canvas_taken() || slot.bench.current != Workbench::Factory {
        return Ok(());
    }
    let (cam, cam_gt, mut orbit) = camera.into_inner();
    let mut asked = Asked::default();

    if let Some(rect) = slot.twin.rect.take() {
        egui::Area::new(egui::Id::new("twin_card")).order(egui::Order::Middle).fixed_pos(rect.min).show(&ctx, |ui| {
            theme::fixed_card(ui, rect.width(), rect.height(), |ui| {
                let title = match slot.twin.page {
                    TwinPage::Lines => "Production lines",
                    TwinPage::Devices => "Factory devices",
                    TwinPage::Events => "Factory events",
                    TwinPage::Connection => "Connection",
                };
                let mut close = false;
                theme::card_title(ui, title, |ui| {
                    close = theme::close_button(ui, "Close").clicked();
                    if inventory.showing_demo() {
                        theme::pill(ui, "Demo", theme::WARNING);
                    }
                });
                if close {
                    slot.twin.open = false;
                }
                match slot.twin.page {
                    TwinPage::Lines => lines_page(ui, &inventory, &selection, &mut line_list, &mut asked, &mut slot),
                    TwinPage::Devices => devices_page(ui, &inventory, &selection, &mut list, &mut asked, &mut slot),
                    TwinPage::Events => events_page(ui, &inventory, &mut asked),
                    TwinPage::Connection => connection_page(ui, &inventory, &mut slot),
                }
            });
        });
    }

    let free = slot.free.unwrap_or(ctx.content_rect());
    if let Some(id) = selection.device.clone()
        && let Some(device) = inventory.mirror.devices.get(&id)
    {
        details_card(&ctx, free, device, &inventory, &mut scene, &mut selection, &mut asked);
    }

    if let Some(id) = asked.select {
        selection.device = Some(id);
    }
    if let Some(device) = asked.focus.as_ref().and_then(|id| inventory.mirror.devices.get(id))
        && let Some(at) = yard.ground(device, &scene.placed)
    {
        orbit.focus = at;
        orbit.radius = orbit.radius.min(12.0);
    }

    let scale = window.scale_factor();
    let viewport = cam
        .viewport
        .as_ref()
        .map(|v| egui::Rect::from_min_size(egui::pos2(v.physical_position.x as f32 / scale, v.physical_position.y as f32 / scale), vec2(v.physical_size.x as f32 / scale, v.physical_size.y as f32 / scale)))
        .unwrap_or(ctx.content_rect());
    viewport_labels(&ctx, viewport, cam, cam_gt, &inventory, &selection, &markers, &yard, slot.twin.labels);

    // Impossible to miss, because this view looks exactly like a real plant.
    if inventory.showing_demo() {
        egui::Area::new(egui::Id::new("twin_demo_banner")).order(egui::Order::Middle).interactable(false).pivot(Align2::CENTER_TOP).fixed_pos(egui::pos2(free.center().x, viewport.top() + 58.0)).show(&ctx, |ui| {
            theme::floating_frame().inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::WARNING).color(theme::WARNING));
                    ui.label(RichText::new("Demo data: none of these devices exist").strong().color(theme::WARNING));
                });
            });
        });
    }
    Ok(())
}

fn devices_page(ui: &mut egui::Ui, inventory: &TwinInventory, selection: &TwinSelection, list: &mut ListState, asked: &mut Asked, slot: &mut LeftPanelSlot) {
    let width = ui.available_width();
    if inventory.mirror.devices.is_empty() {
        no_devices(ui, &inventory.status, slot);
        return;
    }
    let counts = &inventory.counts;
    egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{}", slot.twin.total)).font(theme::bold(20.0)));
            ui.label(RichText::new("devices").color(theme::WEAK));
            ui.add_space(4.0);
            theme::badge(ui, &format!("{} up", counts.up), theme::SUCCESS);
            if counts.suspect > 0 {
                theme::badge(ui, &format!("{} suspect", counts.suspect), theme::WARNING);
            }
            if counts.down > 0 {
                theme::badge(ui, &format!("{} down", counts.down), theme::DANGER);
            }
            if counts.stale > 0 {
                theme::badge(ui, &format!("{} stale", counts.stale), theme::WEAK);
            }
        });

        // How many machines, how many Siemens, how many sensors: the question this answers.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 4.0);
            let mut pick = None;
            for (class, count) in &counts.by_class {
                let on = list.class == Some(*class);
                // Non-breaking spaces, so an icon never wraps away from its name and count.
                let chip = format!("{} {} {count}", class_glyph(*class), class.label()).replace(' ', "\u{a0}");
                let text = RichText::new(chip).small().color(if on { theme::ACCENT } else { theme::TEXT });
                if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text("Show only these").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    pick = Some(if on { None } else { Some(*class) });
                }
            }
            if let Some(pick) = pick {
                list.class = pick;
            }
        });
        let vendors: Vec<String> = {
            let mut vendors: Vec<_> = counts.by_vendor.iter().collect();
            vendors.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            vendors.into_iter().take(6).map(|(name, count)| format!("{name} {count}")).collect()
        };
        ui.add(egui::Label::new(RichText::new(vendors.join("  ·  ")).small().color(theme::WEAK)).truncate());
        if counts.by_class.contains_key(&DeviceClass::Sensor) || !inventory.mirror.devices.is_empty() {
            let sensors = counts.by_class.get(&DeviceClass::Sensor).copied().unwrap_or(0);
            ui.add(egui::Label::new(RichText::new(format!("Sensors: at least {sensors}. Hardwired ones can't be seen from the network.")).small().color(theme::WEAK)).wrap());
        }

        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut list.filter).hint_text(format!("{} Name, address, model, serial", icon::MAGNIFYING_GLASS)).desired_width(ui.available_width() - 110.0));
            ui.checkbox(&mut list.problems_only, "Problems only");
        });
    });
    theme::hairline(ui);

    list.refresh(inventory);
    if list.rows.is_empty() {
        theme::empty_state(ui, icon::MAGNIFYING_GLASS, "No matches", "Nothing in the plant fits this filter.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("twin_devices").auto_shrink([false, false]).show_rows(ui, theme::TREE_ROW_H, list.rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // Rows wider than the panel would make it grow every frame.
        let scroll_width = (width - ui.spacing().scroll.allocated_width() - 1.0).floor();
        for id in &list.rows[range] {
            let Some(device) = inventory.mirror.devices.get(id) else { continue };
            let liveness = inventory.shown_liveness(device);
            let note = match (device.ip(), &device.parent) {
                (Some(ip), _) => ip.to_string(),
                (None, Some(parent)) => format!("port {}", parent.port),
                _ => String::new(),
            };
            let (click, response) = theme::tree_row(
                ui,
                scroll_width,
                &TreeRow {
                    depth: 0,
                    text: device.name(),
                    glyph: class_glyph(device.class),
                    dot: status_dot(liveness),
                    selected: selection.device.as_ref() == Some(id),
                    expander: None,
                    guide: false,
                    last: false,
                    note: &note,
                    dim: matches!(liveness, Liveness::Stale | Liveness::Unknown),
                },
            );
            if click.row {
                asked.select = Some(id.clone());
                asked.focus = Some(id.clone());
            }
            response.on_hover_text(liveness.label());
        }
    });
}

fn no_devices(ui: &mut egui::Ui, status: &LinkStatus, slot: &mut LeftPanelSlot) {
    let (title, body) = match status {
        LinkStatus::NoService | LinkStatus::Starting => ("No devices yet", "The Sense Twin service isn't running on this PC, so there's nothing to show. You can try this workbench with invented devices."),
        LinkStatus::Connected => ("No devices yet", "The service is running but hasn't found anything. Network scanning arrives in a later version."),
        LinkStatus::Lost(_) => ("Connection lost", "The service stopped answering."),
        LinkStatus::Demo => ("Building the demo", "One moment."),
    };
    egui::Frame::new().inner_margin(Margin { left: 18, right: 18, top: 0, bottom: 18 }).show(ui, |ui| {
        theme::empty_state(ui, icon::FACTORY, title, body);
        if *status != LinkStatus::Demo {
            ui.vertical_centered(|ui| {
                if theme::cta_button(ui, "Show a demo factory", true).clicked() {
                    slot.twin.requests.push(TwinRequest::Demo(Some(DEMO_SIZES[0])));
                }
            });
        }
    });
}

fn events_page(ui: &mut egui::Ui, inventory: &TwinInventory, asked: &mut Asked) {
    let events = &inventory.mirror.events;
    if events.is_empty() {
        theme::empty_state(ui, icon::PULSE, "Nothing has happened yet", "Devices going down, recovering or changing show up here.");
        return;
    }
    let width = ui.available_width();
    let count = events.len();
    egui::ScrollArea::vertical().id_salt("twin_events").auto_shrink([false, false]).show_rows(ui, theme::TREE_ROW_H, count, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = (width - ui.spacing().scroll.allocated_width() - 1.0).floor();
        for i in range {
            // Newest at the top.
            let event = &events[count - 1 - i];
            if event_row(ui, width, event, inventory)
                && let Some(id) = &event.device_id
            {
                asked.select = Some(id.clone());
                asked.focus = Some(id.clone());
            }
        }
    });
}

fn event_device_name<'a>(event: &'a Event, inventory: &'a TwinInventory) -> &'a str {
    event.device_id.as_ref().and_then(|id| inventory.mirror.devices.get(id)).map_or("", Device::name)
}

/// One event: its severity colour, what happened, to what, and when. True when clicked.
fn event_row(ui: &mut egui::Ui, width: f32, event: &Event, inventory: &TwinInventory) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, theme::TREE_ROW_H), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme::RAISED);
    }
    let painter = ui.painter_at(rect);
    let y = rect.center().y;
    let color = severity_color(event.severity);
    painter.circle_filled(egui::pos2(rect.left() + 14.0, y), 4.0, color);
    let when = painter.layout_no_wrap(age(event.ts_ms), egui::FontId::proportional(12.0), theme::WEAK);
    let when_x = rect.right() - when.size().x - 10.0;
    let mut text = format!("{}  {}", event.kind.label(), event_device_name(event, inventory));
    if let (Some(before), Some(after)) = (&event.before, &event.after) {
        text.push_str(&format!("  ({before} to {after})"));
    }
    let mut job = egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(13.0), theme::TEXT);
    job.wrap = egui::text::TextWrapping { max_width: (when_x - rect.left() - 34.0).max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = painter.layout_job(job);
    painter.galley(egui::pos2(rect.left() + 26.0, y - galley.size().y / 2.0), galley, theme::TEXT);
    painter.galley(egui::pos2(when_x, y - when.size().y / 2.0), when, theme::WEAK);
    event.device_id.is_some() && response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn connection_page(ui: &mut egui::Ui, inventory: &TwinInventory, slot: &mut LeftPanelSlot) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            let (color, about) = match &inventory.status {
                LinkStatus::Connected => (theme::SUCCESS, "Reading from the Sense Twin service on this PC.".to_string()),
                LinkStatus::Starting => (theme::WARNING, "Looking for the Sense Twin service on this PC.".to_string()),
                LinkStatus::NoService => (theme::WEAK, "The Sense Twin service isn't running on this PC. It has its own installer, because it keeps watching devices with this app closed.".to_string()),
                LinkStatus::Lost(why) => (theme::DANGER, format!("The service stopped answering, so devices show as stale. {why}")),
                LinkStatus::Demo => (theme::WARNING, "Showing invented devices. Nothing here touches a network or a real machine.".to_string()),
            };
            theme::badge(ui, inventory.status.label(), color);
            ui.add(egui::Label::new(RichText::new(about).small().color(theme::WEAK)).wrap());
            theme::hairline(ui);

            ui.label(RichText::new("Demo factory").strong());
            ui.add(egui::Label::new(RichText::new("Try the workbench with invented devices. They change state by themselves.").small().color(theme::WEAK)).wrap());
            ui.horizontal(|ui| {
                for size in DEMO_SIZES {
                    if ui.button(format!("{size} devices")).clicked() {
                        slot.twin.requests.push(TwinRequest::Demo(Some(size)));
                    }
                }
            });
            if inventory.status == LinkStatus::Demo && ui.button("Stop the demo").clicked() {
                slot.twin.requests.push(TwinRequest::Demo(None));
            }
            theme::hairline(ui);

            ui.label(RichText::new("What a scan can and can't tell").strong());
            for line in [
                "A scan can't reveal where a device physically stands. Devices wait in a yard beside the scene until you drag them into place.",
                "Only devices on the network can be seen. Hardwired sensors stay invisible unless a master or PLC reports them.",
                "Control needs setup on the device itself, and a software stop is never a safety function.",
            ] {
                ui.add(egui::Label::new(RichText::new(line).small().color(theme::WEAK)).wrap());
            }
        });
    });
}

fn details_card(ctx: &egui::Context, free: egui::Rect, device: &Device, inventory: &TwinInventory, scene: &mut TwinScene, selection: &mut TwinSelection, asked: &mut Asked) {
    let width = DETAILS_WIDTH.min(free.width() - 20.0).max(220.0);
    // Below the view toolbar and the demo warning, which must never be covered.
    let top = free.top() + DETAILS_TOP;
    egui::Area::new(egui::Id::new("twin_details")).order(egui::Order::Middle).pivot(Align2::RIGHT_TOP).fixed_pos(egui::pos2(free.right(), top)).constrain_to(ctx.content_rect()).show(ctx, |ui| {
        theme::fixed_card(ui, width, free.height() - DETAILS_TOP - 10.0, |ui| {
            let mut close = false;
            theme::card_title(ui, device.name(), |ui| close = theme::close_button(ui, "Close").clicked());
            if close {
                selection.device = None;
            }
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    let liveness = inventory.shown_liveness(device);
                    ui.horizontal(|ui| {
                        theme::badge(ui, liveness.label(), status_color(liveness));
                        theme::badge(ui, device.class.label(), theme::WEAK);
                        if let Some(mode) = &device.mode {
                            theme::badge(ui, mode, theme::ACCENT);
                        }
                    });

                    egui::Grid::new("twin_identity").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, key: &str, value: Option<&str>| {
                            if let Some(value) = value.filter(|v| !v.is_empty()) {
                                ui.label(RichText::new(key).small().color(theme::WEAK));
                                ui.add(egui::Label::new(RichText::new(value).small()).truncate());
                                ui.end_row();
                            }
                        };
                        let identity = &device.identity;
                        row(ui, "Vendor", identity.vendor.as_deref());
                        row(ui, "Model", identity.model.as_deref());
                        row(ui, "Order no.", identity.order_number.as_deref());
                        row(ui, "Serial", identity.serial.as_deref());
                        row(ui, "Firmware", identity.firmware.as_deref());
                        for address in &device.addresses {
                            row(ui, "Address", address.ip.as_deref());
                            row(ui, "MAC", address.mac.as_deref());
                            row(ui, "Seen by", Some(&address.vantage));
                        }
                        if let Some(parent) = &device.parent {
                            let master = inventory.mirror.devices.get(&parent.device).map_or(parent.device.0.as_str(), Device::name);
                            row(ui, "Through", Some(&format!("{master}, port {}", parent.port)));
                        }
                        row(ui, "Last seen", device.last_seen_ms.map(age).as_deref());
                    });

                    let placed = scene.placed.contains_key(&device.id);
                    ui.horizontal(|ui| {
                        if ui.button(format!("{} Focus", icon::CROSSHAIR)).clicked() {
                            asked.focus = Some(device.id.clone());
                        }
                        if ui.add_enabled(placed, egui::Button::new("Back to yard")).on_disabled_hover_text("Drag it in the scene to place it").clicked() {
                            scene.placed.remove(&device.id);
                        }
                    });
                    ui.add(egui::Label::new(RichText::new(if placed { "Placed by hand in this scene." } else { "Not placed. Drag it to where it really stands." }).small().color(theme::WEAK)).wrap());
                    theme::hairline(ui);

                    ui.label(RichText::new("Control").strong());
                    ui.add(egui::Label::new(RichText::new(coverage_note(device.coverage)).small().color(theme::WEAK)).wrap());
                    ui.horizontal(|ui| {
                        for text in ["Run", "Stop"] {
                            ui.add_enabled(false, egui::Button::new(text)).on_disabled_hover_text("Device commands arrive with the operator console. This app will never send them itself.");
                        }
                    });
                    ui.add(egui::Label::new(RichText::new("Not available yet: commands will go through a separate operator console.").small().color(theme::WEAK)).wrap());

                    let recent: Vec<&Event> = inventory.mirror.events.iter().rev().filter(|e| e.device_id.as_ref() == Some(&device.id)).take(8).collect();
                    if !recent.is_empty() {
                        theme::hairline(ui);
                        ui.label(RichText::new("Recent events").strong());
                        for event in recent {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(event.kind.label()).small().color(severity_color(event.severity)));
                                ui.label(RichText::new(age(event.ts_ms)).small().color(theme::WEAK));
                            });
                        }
                    }
                });
            });
        });
    });
}

fn coverage_note(coverage: Coverage) -> &'static str {
    match coverage {
        Coverage::Counted => "Counted only: the twin knows its address and whether it answers.",
        Coverage::Identified => "Identified: model and serial are known, but it isn't monitored.",
        Coverage::Monitored => "Monitored: mode and status are read. It isn't enrolled for control.",
        Coverage::Controlled => "Enrolled for control, once the operator console exists.",
    }
}

/// Names in the viewport for what matters: the selected and hovered device, anything failing,
/// and the nearest few. Painted on the background layer, so cards stay on top.
#[allow(clippy::too_many_arguments)]
fn viewport_labels(
    ctx: &egui::Context,
    viewport: egui::Rect,
    cam: &Camera,
    cam_gt: &GlobalTransform,
    inventory: &TwinInventory,
    selection: &TwinSelection,
    markers: &Query<(&TwinMarker, &Transform)>,
    yard: &Yard,
    show_nearest: bool,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("twin_labels"))).with_clip_rect(viewport);
    let eye = cam_gt.translation();

    let mut chosen: Vec<(Vec3, String, egui::Color32)> = Vec::new();
    let mut problems: Vec<(f32, &Device, Liveness, Vec3)> = Vec::new();
    let mut nearest: Vec<(f32, &Device, Vec3)> = Vec::new();
    for (marker, transform) in markers {
        let at = transform.translation + Vec3::Y * 0.7;
        let Some(device) = inventory.mirror.devices.get(&marker.id) else { continue };
        let liveness = inventory.shown_liveness(device);
        let distance = at.distance_squared(eye);
        if selection.device.as_ref() == Some(&marker.id) || selection.hovered.as_ref() == Some(&marker.id) {
            chosen.push((at, device.name().to_string(), theme::TEXT));
        } else if matches!(liveness, Liveness::Down | Liveness::Suspect) {
            problems.push((distance, device, liveness, at));
        } else if show_nearest {
            nearest.push((distance, device, at));
        }
    }
    problems.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, device, liveness, at) in problems.into_iter().take(MAX_PROBLEM_LABELS) {
        chosen.push((at, format!("{} {}", device.name(), liveness.label().to_lowercase()), status_color(liveness)));
    }
    if nearest.len() > NEAREST_LABELS {
        nearest.select_nth_unstable_by(NEAREST_LABELS, |a, b| a.0.total_cmp(&b.0));
        nearest.truncate(NEAREST_LABELS);
    }
    for (_, device, at) in nearest {
        chosen.push((at, device.name().to_string(), theme::WEAK));
    }
    let mut groups: Vec<_> = yard.groups.iter().map(|g| (Vec3::new(g.min.x, 0.0, g.min.y - 0.6), g)).collect();
    groups.sort_by(|a, b| a.0.distance_squared(eye).total_cmp(&b.0.distance_squared(eye)));
    for (at, group) in groups.into_iter().take(MAX_GROUP_LABELS) {
        chosen.push((at, format!("Not placed: {} ({})", group.label, group.count), theme::WEAK));
    }

    // In priority order, skipping whatever would land on a label already drawn: from far
    // away a plant would otherwise be an unreadable heap.
    let mut drawn: Vec<egui::Rect> = Vec::new();
    for (at, text, color) in chosen {
        let Ok(on_screen) = cam.world_to_viewport(cam_gt, at) else { continue };
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(11.0), color);
        let rect = egui::Rect::from_center_size(egui::pos2(on_screen.x, on_screen.y), galley.size() + vec2(8.0, 2.0));
        if !viewport.intersects(rect) || drawn.iter().any(|d| d.intersects(rect)) {
            continue;
        }
        drawn.push(rect);
        theme::viewport_chip(&painter, rect);
        painter.galley(rect.min + vec2(4.0, 1.0), galley, color);
    }
}

#[cfg(test)]
mod tests {
    use twin_proto::demo::DemoFactory;
    use twin_proto::inventory::Mirror;

    use super::*;

    fn inventory(count: usize) -> TwinInventory {
        let mut mirror = Mirror::default();
        mirror.apply(DemoFactory::new(count).sync(None));
        let mut inventory = TwinInventory { mirror, status: LinkStatus::Demo, generation: 1, ..Default::default() };
        inventory.recount();
        inventory
    }

    #[test]
    fn the_list_puts_problems_first() {
        let inventory = inventory(200);
        let mut list = ListState::default();
        list.refresh(&inventory);
        let first = &inventory.mirror.devices[&list.rows[0]];
        assert_eq!(first.liveness, Liveness::Down);
        let last = &inventory.mirror.devices[list.rows.last().unwrap()];
        assert_eq!(last.liveness, Liveness::Up);
    }

    #[test]
    fn filters_narrow_the_list() {
        let inventory = inventory(200);
        let mut list = ListState { class: Some(DeviceClass::Plc), ..Default::default() };
        list.refresh(&inventory);
        assert!(!list.rows.is_empty());
        assert!(list.rows.iter().all(|id| inventory.mirror.devices[id].class == DeviceClass::Plc));

        let mut problems = ListState { problems_only: true, ..Default::default() };
        problems.refresh(&inventory);
        assert!(problems.rows.iter().all(|id| matches!(inventory.mirror.devices[id].liveness, Liveness::Down | Liveness::Suspect)));

        let mut search = ListState { filter: "6ES7 516".into(), ..Default::default() };
        search.refresh(&inventory);
        assert!(!search.rows.is_empty());
    }

    #[test]
    fn a_lost_service_shows_everything_as_stale_rather_than_up() {
        let mut inventory = inventory(50);
        inventory.status = LinkStatus::Lost("pipe closed".into());
        inventory.recount();
        assert_eq!((inventory.counts.up, inventory.counts.down, inventory.counts.suspect), (0, 0, 0));
        assert_eq!(inventory.counts.stale, 50);
    }

    #[test]
    fn the_plant_only_shows_green_when_every_device_is_known_to_be_up() {
        let slot = |total, down, suspect, stale| crate::twin::TwinSlot { total, down, suspect, stale, ..Default::default() };
        assert_eq!(slot(0, 0, 0, 0).health(), Dot::Optional);
        assert_eq!(slot(10, 0, 0, 0).health(), Dot::Done);
        assert_eq!(slot(10, 0, 0, 10).health(), Dot::Unknown, "a lost service must not look healthy");
        assert_eq!(slot(10, 0, 0, 1).health(), Dot::Unknown);
        assert_eq!(slot(10, 0, 1, 1).health(), Dot::Busy);
        assert_eq!(slot(10, 1, 1, 1).health(), Dot::Todo);
    }

    #[test]
    fn demo_data_is_flagged_whichever_side_invented_it() {
        let mut from_service = inventory(10);
        from_service.status = LinkStatus::Connected;
        assert!(from_service.mirror.demo);
        assert!(from_service.showing_demo(), "a service running a demo must still be labelled");

        let mut real = TwinInventory { status: LinkStatus::Connected, ..Default::default() };
        real.mirror.apply(twin_proto::inventory::Inventory::new(1).sync(None, false));
        assert!(!real.showing_demo());
    }

    #[test]
    fn counts_answer_how_many_of_what() {
        let inventory = inventory(200);
        assert!(inventory.counts.by_vendor["Siemens"] > 0);
        assert!(inventory.counts.by_class[&DeviceClass::Plc] > 0);
        assert!(inventory.counts.by_class[&DeviceClass::Sensor] > 0);
        let total: usize = inventory.counts.by_class.values().sum();
        assert_eq!(total, 200);
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(0), "at start");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        assert_eq!(age(now - 90_000), "1 min ago");
        assert_eq!(age(now + 5_000), "0 s ago");
    }
}
