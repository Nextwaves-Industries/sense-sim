//! The Lines page: each production line as a pipeline of its machines, with what was planned,
//! what was made and how long a part takes.
//!
//! The counts come from the line, but whether a machine's devices answer comes from the
//! inventory. That wins: the numbers of a machine nobody can reach are only what was last known.

use bevy_egui::egui::{self, Margin, RichText, vec2};
use twin_proto::model::{Device, DeviceId, Flow, Line, LineId, Liveness, Machine};

use super::ui::{Asked, problem_rank};
use super::{DEMO_SIZES, LinkStatus, TwinInventory, TwinRequest, TwinSelection};
use crate::theme::{self, Dot, icon};
use crate::ui::LeftPanelSlot;

/// Name, numbers, progress bar and pipeline. Fixed, so thousands of lines can scroll cheaply.
const ROW_H: f32 = 126.0;
const SIDE: f32 = 12.0;

/// How a machine is doing, as far as anyone can tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MachineState {
    Down,
    Suspect,
    Unknown,
    Stopped,
    Starved,
    Blocked,
    Running,
}

impl MachineState {
    fn label(self) -> &'static str {
        match self {
            Self::Down => "Down",
            Self::Suspect => "Suspect",
            Self::Unknown => "Not known",
            Self::Stopped => "Stopped",
            Self::Starved => "Waiting for parts",
            Self::Blocked => "Waiting for room",
            Self::Running => "Running",
        }
    }

    fn dot(self) -> Dot {
        match self {
            Self::Down | Self::Stopped => Dot::Todo,
            Self::Suspect => Dot::Busy,
            Self::Unknown => Dot::Unknown,
            // Nothing wrong here: the cause is elsewhere on the line.
            Self::Starved | Self::Blocked => Dot::Optional,
            Self::Running => Dot::Done,
        }
    }
}

/// How a line is doing, worst first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LineState {
    Down,
    Suspect,
    Unknown,
    Slow,
    Running,
}

impl LineState {
    fn color(self) -> egui::Color32 {
        match self {
            Self::Down => theme::DANGER,
            Self::Suspect | Self::Slow => theme::WARNING,
            Self::Unknown => theme::WEAK,
            Self::Running => theme::SUCCESS,
        }
    }
}

/// The worst of these devices. `None` if there are none, which isn't the same as healthy. A
/// device the inventory doesn't hold can't be vouched for, so it counts as unknown.
fn worst_liveness(devices: &[DeviceId], inventory: &TwinInventory) -> Option<Liveness> {
    devices.iter().map(|id| inventory.mirror.devices.get(id).map_or(Liveness::Unknown, |d| inventory.shown_liveness(d))).min_by_key(|l| problem_rank(*l))
}

pub fn machine_state(machine: &Machine, inventory: &TwinInventory) -> MachineState {
    match worst_liveness(&machine.devices, inventory) {
        Some(Liveness::Down) => MachineState::Down,
        Some(Liveness::Suspect) => MachineState::Suspect,
        Some(Liveness::Up) => match machine.flow {
            Flow::Running => MachineState::Running,
            Flow::Starved => MachineState::Starved,
            Flow::Blocked => MachineState::Blocked,
            Flow::Stopped => MachineState::Stopped,
        },
        _ => MachineState::Unknown,
    }
}

/// What to tell someone looking at the whole line, naming where the trouble is.
pub fn line_state(line: &Line, inventory: &TwinInventory) -> (LineState, String) {
    let states: Vec<MachineState> = line.machines.iter().map(|m| machine_state(m, inventory)).collect();
    let first = |wanted: MachineState| states.iter().position(|s| *s == wanted).map(|i| line.machines[i].name.as_str());
    let control = worst_liveness(&line.control, inventory);

    if control == Some(Liveness::Down) {
        return (LineState::Down, "Line control down".to_string());
    }
    if let Some(name) = first(MachineState::Down) {
        return (LineState::Down, format!("{name} down"));
    }
    if !states.is_empty() && states.iter().all(|s| *s == MachineState::Stopped) {
        return (LineState::Down, "Line stopped".to_string());
    }
    if let Some(name) = first(MachineState::Stopped) {
        return (LineState::Down, format!("{name} stopped"));
    }
    if let Some(name) = first(MachineState::Suspect) {
        return (LineState::Suspect, format!("{name} suspect"));
    }
    if control == Some(Liveness::Suspect) {
        return (LineState::Suspect, "Line control suspect".to_string());
    }
    // Never "running" on the strength of something nobody can see.
    if states.is_empty() || states.contains(&MachineState::Unknown) || matches!(control, Some(Liveness::Unknown | Liveness::Stale)) {
        return (LineState::Unknown, "Not known".to_string());
    }
    if line.is_slow() {
        let slowest = line.bottleneck().map_or("A machine", |i| line.machines[i].name.as_str());
        return (LineState::Slow, format!("{slowest} slow"));
    }
    (LineState::Running, "Running".to_string())
}

/// The lines at a glance.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct LineTally {
    pub total: usize,
    pub down: usize,
    pub suspect: usize,
    pub unknown: usize,
    pub slow: usize,
    pub running: usize,
    pub plan: u64,
    pub made: u64,
}

impl LineTally {
    pub fn of(inventory: &TwinInventory) -> Self {
        let mut tally = Self::default();
        for line in inventory.mirror.lines.values() {
            tally.total += 1;
            tally.plan += u64::from(line.plan);
            tally.made += u64::from(line.made());
            match line_state(line, inventory).0 {
                LineState::Down => tally.down += 1,
                LineState::Suspect => tally.suspect += 1,
                LineState::Unknown => tally.unknown += 1,
                LineState::Slow => tally.slow += 1,
                LineState::Running => tally.running += 1,
            }
        }
        tally
    }

    /// The tree's circle. Green only if every line is known to be keeping up.
    pub fn health(&self) -> Dot {
        if self.total == 0 {
            Dot::Optional
        } else if self.down > 0 {
            Dot::Todo
        } else if self.suspect + self.slow > 0 {
            Dot::Busy
        } else if self.unknown > 0 {
            Dot::Unknown
        } else {
            Dot::Done
        }
    }
}

#[derive(Default)]
pub struct LineList {
    filter: String,
    problems_only: bool,
    /// The rows for this filter, redone only when something changed.
    rows: Vec<LineId>,
    built_for: Option<(u64, String, bool)>,
}

impl LineList {
    fn refresh(&mut self, inventory: &TwinInventory) {
        let key = (inventory.generation, self.filter.clone(), self.problems_only);
        if self.built_for.as_ref() == Some(&key) {
            return;
        }
        let needle = self.filter.to_lowercase();
        let mut rows: Vec<(LineState, &LineId)> = inventory
            .mirror
            .lines
            .values()
            .map(|line| (line_state(line, inventory).0, line))
            .filter(|(state, _)| !self.problems_only || *state != LineState::Running)
            .filter(|(_, line)| needle.is_empty() || line_matches(line, inventory, &needle))
            .map(|(state, line)| (state, &line.id))
            .collect();
        // Problems first, as in the device list. Stable, so lines keep their order within each.
        rows.sort_by_key(|(state, _)| *state);
        self.rows = rows.into_iter().map(|(_, id)| id.clone()).collect();
        self.built_for = Some(key);
    }
}

fn line_matches(line: &Line, inventory: &TwinInventory, needle: &str) -> bool {
    line.name.to_lowercase().contains(needle)
        || line.machines.iter().any(|m| m.name.to_lowercase().contains(needle))
        || line.control.iter().chain(line.machines.iter().flat_map(|m| &m.devices)).any(|id| inventory.mirror.devices.get(id).is_some_and(|d| d.name().to_lowercase().contains(needle)))
}

/// `1,440`, because plans run into the thousands.
fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// Whole seconds, unless a part takes so little that the tenths matter.
fn seconds(ms: u32) -> String {
    if ms < 10_000 { format!("{:.1} s", f64::from(ms) / 1000.0) } else { format!("{} s", (ms + 500) / 1000) }
}

fn percent(made: u64, plan: u64) -> u64 {
    if plan == 0 { 0 } else { made * 100 / plan }
}

pub fn lines_page(ui: &mut egui::Ui, inventory: &TwinInventory, selection: &TwinSelection, list: &mut LineList, asked: &mut Asked, slot: &mut LeftPanelSlot) {
    if inventory.mirror.lines.is_empty() {
        no_lines(ui, &inventory.status, slot);
        return;
    }
    let width = ui.available_width();
    let tally = &inventory.lines;
    egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(count(tally.total as u64)).font(theme::bold(20.0)));
            ui.label(RichText::new("lines").color(theme::WEAK));
            ui.add_space(4.0);
            let badges = [
                (tally.running, "running", LineState::Running),
                (tally.slow, "slow", LineState::Slow),
                (tally.suspect, "suspect", LineState::Suspect),
                (tally.down, "down", LineState::Down),
                (tally.unknown, "not known", LineState::Unknown),
            ];
            for (n, text, state) in badges {
                if n > 0 {
                    whole_badge(ui, &format!("{n} {text}"), state.color());
                }
            }
        });
        ui.label(RichText::new(format!("Plan {}   Made {} ({}%)", count(tally.plan), count(tally.made), percent(tally.made, tally.plan))).small().color(theme::WEAK));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut list.filter).hint_text(format!("{} Line, machine or device", icon::MAGNIFYING_GLASS)).desired_width((ui.available_width() - 140.0).max(60.0)));
            ui.checkbox(&mut list.problems_only, "Problems only");
        });
    });
    theme::hairline(ui);

    list.refresh(inventory);
    if list.rows.is_empty() {
        theme::empty_state(ui, icon::MAGNIFYING_GLASS, "No matches", "No line fits this filter.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("twin_lines").auto_shrink([false, false]).show_rows(ui, ROW_H, list.rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // Rows wider than the panel would make it grow every frame.
        let width = (width - ui.spacing().scroll.allocated_width() - 1.0).floor();
        for id in &list.rows[range] {
            if let Some(line) = inventory.mirror.lines.get(id) {
                line_row(ui, width, line, inventory, selection, asked);
            }
        }
    });
}

/// A badge that starts a new row if it wouldn't fit on this one. A frame takes whatever room is
/// left where it starts, so on its own it would break its text up instead of moving.
fn whole_badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    const FRAME_MARGINS: f32 = 14.0;
    let font = egui::TextStyle::Small.resolve(ui.style());
    let needed = ui.painter().layout_no_wrap(text.to_string(), font, color).size().x + FRAME_MARGINS;
    if needed > ui.available_size_before_wrap().x {
        ui.end_row();
    }
    theme::badge(ui, text, color);
}

/// Text cut to fit its space, for names nobody chose with this column in mind.
fn fitted(painter: &egui::Painter, text: String, size: f32, max_width: f32, color: egui::Color32, strong: bool) -> std::sync::Arc<egui::Galley> {
    let font = if strong { theme::bold(size) } else { egui::FontId::proportional(size) };
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping { max_width: max_width.max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    painter.layout_job(job)
}

fn line_row(ui: &mut egui::Ui, width: f32, line: &Line, inventory: &TwinInventory, selection: &TwinSelection, asked: &mut Asked) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (state, status) = line_state(line, inventory);
    let (left, right) = (rect.left() + SIDE, rect.right() - SIDE);

    // Its name, and how it's doing.
    let status = painter.layout_no_wrap(status, egui::FontId::proportional(12.0), state.color());
    let status_x = right - status.size().x;
    let name = fitted(&painter, line.name.clone(), 14.0, status_x - left - 8.0, theme::TEXT, true);
    painter.galley(egui::pos2(left, rect.top() + 8.0), name, theme::TEXT);
    painter.galley(egui::pos2(status_x, rect.top() + 10.0), status, state.color());

    // Plan, made, cycle.
    let (made, plan) = (u64::from(line.made()), u64::from(line.plan));
    let numbers = format!("Plan {}   Made {} ({}%)", count(plan), count(made), percent(made, plan));
    let numbers = fitted(&painter, numbers, 12.0, (right - left) * 0.6, theme::WEAK, false);
    painter.galley(egui::pos2(left, rect.top() + 28.0), numbers, theme::WEAK);
    let (cycle, cycle_color) = match line.cycle_ms() {
        Some(ms) => (format!("Cycle {} / {}", seconds(ms), seconds(line.target_cycle_ms)), if line.is_slow() { theme::WARNING } else { theme::WEAK }),
        None => (format!("Target {}", seconds(line.target_cycle_ms)), theme::WEAK),
    };
    let cycle = painter.layout_no_wrap(cycle, egui::FontId::proportional(12.0), cycle_color);
    painter.galley(egui::pos2(right - cycle.size().x, rect.top() + 28.0), cycle, cycle_color);

    // How far along the plan.
    let bar = egui::Rect::from_min_size(egui::pos2(left, rect.top() + 46.0), vec2(right - left, 4.0));
    painter.rect_filled(bar, 2.0, theme::STROKE);
    let share = if plan == 0 { 0.0 } else { (made as f32 / plan as f32).min(1.0) };
    if share > 0.0 {
        painter.rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * share, bar.height())), 2.0, theme::ACCENT);
    }

    pipeline(ui, &painter, egui::Rect::from_min_max(egui::pos2(left, rect.top() + 54.0), egui::pos2(right, rect.bottom() - 4.0)), line, inventory, selection, asked);
    painter.line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0, theme::STROKE));
}

/// The machines left to right as a product passes through them. The number on each link is how
/// many parts are waiting there, which shows where the line is backing up.
fn pipeline(ui: &mut egui::Ui, painter: &egui::Painter, area: egui::Rect, line: &Line, inventory: &TwinInventory, selection: &TwinSelection, asked: &mut Asked) {
    let n = line.machines.len();
    if n == 0 {
        return;
    }
    let column = area.width() / n as f32;
    let circle_y = area.top() + 14.0;
    let center = |i: usize| area.left() + column * (i as f32 + 0.5);
    let slowest = line.bottleneck();

    // Backgrounds first, so the links and their numbers stay on top.
    let responses: Vec<egui::Response> = line
        .machines
        .iter()
        .enumerate()
        .map(|(i, machine)| {
            let cell = egui::Rect::from_min_max(egui::pos2(area.left() + column * i as f32, area.top()), egui::pos2(area.left() + column * (i as f32 + 1.0), area.bottom()));
            let response = ui.interact(cell, egui::Id::new(("twin_line_machine", &line.id, i)), egui::Sense::click());
            if response.hovered() {
                painter.rect_filled(cell, 4.0, theme::RAISED);
            } else if selection.device.as_ref().is_some_and(|id| machine.devices.contains(id)) {
                painter.rect_filled(cell, 4.0, theme::ACCENT_SOFT);
            }
            response
        })
        .collect();

    for (i, pair) in line.machines.windows(2).enumerate() {
        let (from, to) = (center(i) + 9.0, center(i + 1) - 9.0);
        let stroke = egui::Stroke::new(1.5, theme::STROKE_STRONG);
        painter.line_segment([egui::pos2(from, circle_y), egui::pos2(to, circle_y)], stroke);
        painter.line_segment([egui::pos2(to - 4.0, circle_y - 3.0), egui::pos2(to, circle_y)], stroke);
        painter.line_segment([egui::pos2(to - 4.0, circle_y + 3.0), egui::pos2(to, circle_y)], stroke);
        let waiting = pair[0].made.saturating_sub(pair[1].made);
        if waiting > 0 {
            painter.text(egui::pos2((from + to) / 2.0, circle_y - 3.0), egui::Align2::CENTER_BOTTOM, waiting.to_string(), egui::FontId::proportional(10.0), theme::WEAK);
        }
    }

    for (i, (machine, response)) in line.machines.iter().zip(responses).enumerate() {
        let state = machine_state(machine, inventory);
        theme::paint_dot(painter, egui::pos2(center(i), circle_y), state.dot());

        let is_slowest = slowest == Some(i);
        let room = column - 4.0;
        let name = fitted(painter, machine.name.clone(), 11.0, room, theme::TEXT, is_slowest);
        painter.galley(egui::pos2(center(i) - name.size().x / 2.0, circle_y + 11.0), name, theme::TEXT);
        // Under the name: what it's made, then how long it takes.
        let made = fitted(painter, count(u64::from(machine.made)), 11.0, room, theme::WEAK, false);
        painter.galley(egui::pos2(center(i) - made.size().x / 2.0, circle_y + 25.0), made, theme::WEAK);
        let too_slow = machine.cycle_ms.is_some_and(|ms| ms > line.target_cycle_ms);
        let cycle_color = if too_slow { theme::WARNING } else { theme::WEAK };
        let cycle = fitted(painter, machine.cycle_ms.map_or_else(|| "-".to_string(), seconds), 11.0, room, cycle_color, is_slowest);
        painter.galley(egui::pos2(center(i) - cycle.size().x / 2.0, circle_y + 39.0), cycle, cycle_color);

        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_ui(|ui| machine_tip(ui, line, machine, state, is_slowest, inventory));
        if response.clicked()
            && let Some(id) = worst_device(machine, inventory)
        {
            asked.select = Some(id.clone());
            asked.focus = Some(id.clone());
        }
    }
}

fn machine_tip(ui: &mut egui::Ui, line: &Line, machine: &Machine, state: MachineState, is_slowest: bool, inventory: &TwinInventory) {
    ui.label(RichText::new(&machine.name).strong());
    ui.label(state.label());
    ui.label(format!("Made {}", count(u64::from(machine.made))));
    if let Some(ms) = machine.cycle_ms {
        ui.label(format!("Last cycle {}, target {}", seconds(ms), seconds(line.target_cycle_ms)));
    }
    if is_slowest {
        ui.label("Slowest on this line: it sets the pace");
    }
    for id in &machine.devices {
        let name = inventory.mirror.devices.get(id).map_or(id.0.as_str(), Device::name);
        ui.label(RichText::new(name).small().color(theme::WEAK));
    }
}

/// The device to show for a machine: the one most likely to be why someone clicked.
fn worst_device<'a>(machine: &'a Machine, inventory: &TwinInventory) -> Option<&'a DeviceId> {
    machine.devices.iter().min_by_key(|id| inventory.mirror.devices.get(*id).map_or(problem_rank(Liveness::Unknown), |d| problem_rank(inventory.shown_liveness(d))))
}

fn no_lines(ui: &mut egui::Ui, status: &LinkStatus, slot: &mut LeftPanelSlot) {
    egui::Frame::new().inner_margin(Margin { left: 18, right: 18, top: 0, bottom: 18 }).show(ui, |ui| {
        theme::empty_state(ui, icon::FLOW_ARROW, "No production lines yet", "A network scan can't tell which machines form a line or what they've made. You can try this page with an invented factory.");
        if *status != LinkStatus::Demo {
            ui.vertical_centered(|ui| {
                if theme::cta_button(ui, "Show a demo factory", true).clicked() {
                    slot.twin.requests.push(TwinRequest::Demo(Some(DEMO_SIZES[0])));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use twin_proto::demo::DemoFactory;
    use twin_proto::inventory::{Inventory, Mirror};
    use twin_proto::model::{Coverage, DeviceClass};

    use super::*;

    fn device(id: &str, liveness: Liveness) -> Device {
        Device { id: DeviceId(id.into()), class: DeviceClass::Plc, liveness, coverage: Coverage::Counted, identity: Default::default(), addresses: Vec::new(), mode: None, parent: None, last_seen_ms: None, rev: 0 }
    }

    fn machine(name: &str, device: &str, flow: Flow, cycle_ms: u32) -> Machine {
        Machine { name: name.into(), devices: vec![DeviceId(device.into())], flow, made: 10, cycle_ms: Some(cycle_ms) }
    }

    /// PLC `p`, then machines on devices `a` and `b`, all up unless changed.
    fn plant(devices: &[(&str, Liveness)], machines: Vec<Machine>) -> (TwinInventory, Line) {
        let mut source = Inventory::new(1);
        for (id, liveness) in devices {
            source.upsert(device(id, *liveness));
        }
        let line = Line { id: LineId("l".into()), name: "Line 1".into(), control: vec![DeviceId("p".into())], machines, plan: 100, target_cycle_ms: 1000, rev: 0 };
        source.upsert_line(line.clone());
        let mut mirror = Mirror::default();
        mirror.apply(source.sync(None, false));
        (TwinInventory { mirror, status: LinkStatus::Connected, generation: 1, ..Default::default() }, line)
    }

    fn healthy() -> (TwinInventory, Line) {
        plant(&[("p", Liveness::Up), ("a", Liveness::Up), ("b", Liveness::Up)], vec![machine("Press", "a", Flow::Running, 800), machine("Pack", "b", Flow::Running, 700)])
    }

    fn demo(count: usize) -> TwinInventory {
        let mut mirror = Mirror::default();
        mirror.apply(DemoFactory::new(count).sync(None));
        let mut inventory = TwinInventory { mirror, status: LinkStatus::Demo, generation: 1, ..Default::default() };
        inventory.recount();
        inventory
    }

    #[test]
    fn a_line_with_everything_up_and_on_pace_is_running() {
        let (inventory, line) = healthy();
        assert_eq!(line_state(&line, &inventory), (LineState::Running, "Running".to_string()));
    }

    #[test]
    fn the_line_says_which_machine_is_the_trouble() {
        let (inventory, line) = plant(&[("p", Liveness::Up), ("a", Liveness::Up), ("b", Liveness::Down)], vec![machine("Press", "a", Flow::Blocked, 800), machine("Pack", "b", Flow::Stopped, 700)]);
        assert_eq!(line_state(&line, &inventory), (LineState::Down, "Pack down".to_string()));

        let (inventory, line) = plant(&[("p", Liveness::Up), ("a", Liveness::Suspect), ("b", Liveness::Up)], vec![machine("Press", "a", Flow::Running, 800), machine("Pack", "b", Flow::Running, 700)]);
        assert_eq!(line_state(&line, &inventory), (LineState::Suspect, "Press suspect".to_string()));

        let (inventory, line) = plant(&[("p", Liveness::Up), ("a", Liveness::Up), ("b", Liveness::Up)], vec![machine("Press", "a", Flow::Running, 1500), machine("Pack", "b", Flow::Running, 700)]);
        assert_eq!(line_state(&line, &inventory), (LineState::Slow, "Press slow".to_string()));
    }

    #[test]
    fn losing_the_plc_is_a_problem_even_with_every_machine_up() {
        let (inventory, line) = plant(&[("p", Liveness::Down), ("a", Liveness::Up), ("b", Liveness::Up)], vec![machine("Press", "a", Flow::Stopped, 800), machine("Pack", "b", Flow::Stopped, 700)]);
        assert_eq!(line_state(&line, &inventory), (LineState::Down, "Line control down".to_string()));
    }

    #[test]
    fn every_machine_stopped_reads_as_the_line_stopped() {
        let (inventory, line) = plant(&[("p", Liveness::Up), ("a", Liveness::Up), ("b", Liveness::Up)], vec![machine("Press", "a", Flow::Stopped, 800), machine("Pack", "b", Flow::Stopped, 700)]);
        assert_eq!(line_state(&line, &inventory), (LineState::Down, "Line stopped".to_string()));
    }

    #[test]
    fn a_waiting_machine_does_not_make_the_line_a_problem() {
        let (inventory, line) = plant(&[("p", Liveness::Up), ("a", Liveness::Up), ("b", Liveness::Up)], vec![machine("Press", "a", Flow::Running, 800), machine("Pack", "b", Flow::Starved, 700)]);
        assert_eq!(machine_state(&line.machines[1], &inventory), MachineState::Starved);
        assert_eq!(line_state(&line, &inventory).0, LineState::Running);
    }

    #[test]
    fn a_lost_service_shows_no_line_as_running_whatever_it_last_said() {
        let (mut inventory, line) = healthy();
        inventory.status = LinkStatus::Lost("pipe closed".into());
        assert_eq!(machine_state(&line.machines[0], &inventory), MachineState::Unknown);
        assert_eq!(line_state(&line, &inventory).0, LineState::Unknown);
    }

    #[test]
    fn what_nobody_can_vouch_for_is_not_known_rather_than_running() {
        // A machine with no devices, and one whose device the inventory has never heard of.
        let mut empty = machine("Press", "a", Flow::Running, 800);
        empty.devices.clear();
        let (inventory, line) = plant(&[("p", Liveness::Up)], vec![empty, machine("Pack", "missing", Flow::Running, 700)]);
        assert_eq!(machine_state(&line.machines[0], &inventory), MachineState::Unknown);
        assert_eq!(machine_state(&line.machines[1], &inventory), MachineState::Unknown);
        assert_eq!(line_state(&line, &inventory).0, LineState::Unknown);

        let (inventory, mut line) = healthy();
        line.machines.clear();
        assert_eq!(line_state(&line, &inventory).0, LineState::Unknown);
    }

    #[test]
    fn the_worst_device_is_what_a_click_shows() {
        let (inventory, line) = plant(&[("p", Liveness::Up), ("a", Liveness::Up), ("b", Liveness::Down)], vec![Machine { name: "Press".into(), devices: vec![DeviceId("a".into()), DeviceId("b".into())], flow: Flow::Stopped, made: 1, cycle_ms: None }]);
        assert_eq!(worst_device(&line.machines[0], &inventory), Some(&DeviceId("b".into())));
    }

    #[test]
    fn the_list_puts_problems_first_and_keeps_each_group_in_line_order() {
        let inventory = demo(500);
        let mut list = LineList::default();
        list.refresh(&inventory);
        assert_eq!(list.rows.len(), inventory.mirror.lines.len());
        let state = |id: &LineId| line_state(&inventory.mirror.lines[id], &inventory).0;
        assert!(list.rows.is_sorted_by_key(state));
        assert_eq!(state(&list.rows[0]), LineState::Down);
        let downs: Vec<&LineId> = list.rows.iter().filter(|id| state(id) == LineState::Down).collect();
        assert!(downs.is_sorted());
    }

    #[test]
    fn filters_narrow_the_list() {
        let inventory = demo(500);
        let mut problems = LineList { problems_only: true, ..Default::default() };
        problems.refresh(&inventory);
        assert!(!problems.rows.is_empty());
        assert!(problems.rows.iter().all(|id| line_state(&inventory.mirror.lines[id], &inventory).0 != LineState::Running));

        let mut by_line = LineList { filter: "demo line 7".into(), ..Default::default() };
        by_line.refresh(&inventory);
        assert!(by_line.rows.iter().all(|id| inventory.mirror.lines[id].name.starts_with("DEMO Line 7")));
        assert!(!by_line.rows.is_empty());

        let mut by_device = LineList { filter: "demo-l3-cnc".into(), ..Default::default() };
        by_device.refresh(&inventory);
        assert_eq!(by_device.rows.len(), 1);
        assert_eq!(inventory.mirror.lines[&by_device.rows[0]].name, "DEMO Line 3");

        let mut nothing = LineList { filter: "no such thing".into(), ..Default::default() };
        nothing.refresh(&inventory);
        assert!(nothing.rows.is_empty());
    }

    #[test]
    fn the_tally_adds_up_and_the_tree_only_shows_green_when_every_line_keeps_up() {
        let inventory = demo(500);
        let tally = inventory.lines;
        assert_eq!(tally.total, inventory.mirror.lines.len());
        assert_eq!(tally.down + tally.suspect + tally.unknown + tally.slow + tally.running, tally.total);
        assert!(tally.plan > tally.made && tally.made > 0);

        let tally = |down, suspect, unknown, slow, running| LineTally { total: down + suspect + unknown + slow + running, down, suspect, unknown, slow, running, plan: 0, made: 0 };
        assert_eq!(LineTally::default().health(), Dot::Optional);
        assert_eq!(tally(0, 0, 0, 0, 3).health(), Dot::Done);
        assert_eq!(tally(0, 0, 1, 0, 3).health(), Dot::Unknown);
        assert_eq!(tally(0, 0, 0, 1, 3).health(), Dot::Busy);
        assert_eq!(tally(0, 1, 1, 0, 3).health(), Dot::Busy);
        assert_eq!(tally(1, 1, 1, 1, 3).health(), Dot::Todo);
    }

    #[test]
    fn the_demo_button_shows_every_state_a_line_can_be_in() {
        let tally = demo(crate::twin::DEMO_SIZES[0]).lines;
        assert!(tally.down > 0 && tally.suspect > 0 && tally.unknown > 0 && tally.slow > 0 && tally.running > 0, "{tally:?}");
    }

    #[test]
    fn a_fresh_demo_is_mostly_working_lines() {
        let tally = demo(5000).lines;
        assert!(tally.running * 2 > tally.total, "{tally:?}");
    }

    #[test]
    fn numbers_read_naturally() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(seconds(8500), "8.5 s");
        assert_eq!(seconds(23_400), "23 s");
        assert_eq!(seconds(23_600), "24 s");
        assert_eq!(percent(1, 0), 0);
        assert_eq!(percent(60, 120), 50);
    }
}
