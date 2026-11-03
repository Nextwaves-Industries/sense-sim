//! The variables on the canvas, drawn the way a sketch's dimensions are: a line standing clear
//! of what it measures, and `L = 70 mm` on it. What they measure comes from
//! [`antenna_core::dims`]; this is only where they go on the screen.

use antenna_core::dims::{Dim, DimKind};
use antenna_core::geometry::{norm, sub};
use bevy_egui::egui::{self, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::view3d::Projector;

const INK: Color32 = Color32::from_rgb(24, 78, 170);
const TEXT_SIZE: f32 = 12.5;
/// The first line stands this far from what it measures, in points, the next ones further out.
const FIRST: f32 = 20.0;
const NEXT: f32 = 19.0;
const TIERS: usize = 8;
/// Where a label may sit along its line, as a part of it: the middle if there's room.
const SLIDES: [f32; 5] = [0.5, 0.3, 0.7, 0.15, 0.85];
/// Parallel lines nearer than this, in points, read as one.
const TOO_NEAR: f32 = 7.0;
/// Seen this nearly end on, a line says nothing: a thickness from directly above.
const END_ON: f32 = 0.15;

/// A dimension's place on the screen.
#[derive(Clone, Debug)]
struct Placed {
    text: String,
    label: Rect,
    /// Leaders, faint.
    leaders: Vec<[Pos2; 2]>,
    /// The line and its end marks.
    lines: Vec<[Pos2; 2]>,
    /// The one whose row is being pointed at.
    focus: bool,
}

/// Labels are painted last, on a solid ground, so a line passing behind one does no harm.
fn is_clear(label: Rect, placed: &[Placed]) -> bool {
    let room = label.expand(2.0);
    placed.iter().all(|p| !p.label.intersects(room))
}

/// Two lines so close along each other that they'd read as one.
fn runs_along(l: [Pos2; 2], m: [Pos2; 2]) -> bool {
    let (d, e) = (l[1] - l[0], m[1] - m[0]);
    let (long, other) = (d.length(), e.length());
    if long < 1.0 || other < 1.0 {
        return false;
    }
    let u = d / long;
    let across = |v: Vec2| u.x * v.y - u.y * v.x;
    let from = m[0] - l[0];
    if across(e).abs() > other * 0.05 || across(from).abs() > TOO_NEAR {
        return false;
    }
    let (s0, s1) = (u.dot(from), u.dot(m[1] - l[0]));
    s0.max(s1) > 1.0 && s0.min(s1) < long - 1.0
}

/// Where each one goes in this view. The short ones stay nearest the model and the long ones go
/// round them, as on a drawing. No two labels overlap, nor two lines lie along each other, and a
/// label stays where it can be seen: first outward, sliding along its line to find room, then
/// back across the model rather than under a card. Only the main ones go, and `focus` if it's
/// asked for, placed last so that the others stay put while a row is pointed at.
fn place(dims: &[Dim], pj: &Projector, middle: Pos2, view: Rect, size_of: impl Fn(&str) -> Vec2, focus: Option<&str>) -> Vec<Placed> {
    let is_focus = |d: &Dim| focus == Some(d.name.as_str());
    let mut order: Vec<(f32, &Dim)> = Vec::with_capacity(dims.len());
    for d in dims.iter().filter(|d| d.main || is_focus(d)) {
        let (a, b) = (pj.screen(d.line[0]), pj.screen(d.line[1]));
        let seen = a.distance(b);
        let whole = norm(sub(d.line[1], d.line[0])) as f32 * pj.points_per_metre();
        // Too small to tell which way it runs is a different thing from pointing at the viewer.
        if seen < whole * END_ON {
            continue;
        }
        order.push((seen, d));
    }
    order.sort_by(|x, y| (!x.1.main).cmp(&!y.1.main).then(x.0.total_cmp(&y.0)));
    let mut placed: Vec<Placed> = Vec::with_capacity(order.len());
    for (_, d) in order {
        let text = d.text();
        let size = size_of(&text) + vec2(8.0, 4.0);
        let (a, b) = (pj.screen(d.line[0]), pj.screen(d.line[1]));
        let along = if a.distance(b) > 0.01 { (b - a).normalized() } else { vec2(1.0, 0.0) };
        let mut chosen: Option<Placed> = None;
        let mut spare: Option<Placed> = None;
        match d.kind {
            DimKind::Between => {
                let halfway = a + (b - a) * 0.5;
                let side = along.rot90();
                let out = if (halfway + side - middle).length() >= (halfway - side - middle).length() { side } else { -side };
                let to = pj.screen(d.to);
                let at = |way: Vec2, tier: usize, slide: f32| {
                    let n = way * (FIRST + NEXT * tier as f32);
                    let (x, y) = (a + n, b + n);
                    let tick = way * 4.0;
                    Placed { text: text.clone(), label: Rect::from_center_size(x + (y - x) * slide, size), leaders: vec![[a, x], [to, y]], lines: vec![[x, y], [x - tick, x + tick], [y - tick, y + tick]], focus: false }
                };
                'search: for way in [out, -out] {
                    for tier in 0..TIERS {
                        let line = at(way, tier, 0.5).lines[0];
                        if placed.iter().any(|p| runs_along(line, p.lines[0])) {
                            continue;
                        }
                        for slide in SLIDES {
                            let here = at(way, tier, slide);
                            if is_clear(here.label, &placed) {
                                if view.contains_rect(here.label) {
                                    chosen = Some(here);
                                    break 'search;
                                }
                                spare.get_or_insert(here);
                            }
                        }
                    }
                }
                spare.get_or_insert_with(|| at(out, TIERS - 1, 0.5));
            }
            DimKind::Inside => {
                // Out past its end, along itself: on a thin wire the text is all there's room for.
                let way = if a.distance(b) > 3.0 { along } else { vec2(0.7, -0.7) };
                let edge = (size.x * way.x.abs() + size.y * way.y.abs()) / 2.0;
                let at = |tier: usize| {
                    let reach = FIRST + NEXT * tier as f32;
                    Placed { text: text.clone(), label: Rect::from_center_size(b + way * (reach + edge), size), leaders: Vec::new(), lines: vec![[a, b], [b, b + way * reach]], focus: false }
                };
                for tier in 0..TIERS {
                    let here = at(tier);
                    if is_clear(here.label, &placed) {
                        if view.contains_rect(here.label) {
                            chosen = Some(here);
                            break;
                        }
                        spare.get_or_insert(here);
                    }
                }
                spare.get_or_insert_with(|| at(TIERS - 1));
            }
        }
        let focused = is_focus(d);
        placed.extend(chosen.or(spare).map(|p| Placed { focus: focused, ..p }));
    }
    placed
}

/// Over the model, like a port's mark: a dimension is read, not looked at through the metal.
pub(super) fn paint(painter: &egui::Painter, pj: &Projector, dims: &[Dim], focus: Option<&str>, middle: Pos2, view: Rect) {
    let font = FontId::proportional(TEXT_SIZE);
    let mut placed = place(dims, pj, middle, view, |t| painter.layout_no_wrap(t.to_string(), font.clone(), INK).size(), focus);
    // The one being pointed at goes over the rest.
    placed.sort_by_key(|p| p.focus);
    for p in &placed {
        let width = if p.focus { 1.8 } else { 1.0 };
        for l in &p.leaders {
            painter.line_segment(*l, Stroke::new(width * 0.8, INK.gamma_multiply(if p.focus { 0.8 } else { 0.45 })));
        }
        for l in &p.lines {
            painter.line_segment(*l, Stroke::new(width, INK));
        }
    }
    // Text last, so no line runs through a number.
    for p in placed {
        let (ground, ink) = if p.focus { (INK, Color32::WHITE) } else { (Color32::from_white_alpha(240), INK) };
        let g = painter.layout_no_wrap(p.text, font.clone(), ink);
        painter.rect(p.label, 3.0, ground, Stroke::new(0.8, INK.gamma_multiply(0.5)), StrokeKind::Inside);
        painter.galley(pos2(p.label.min.x + 4.0, p.label.min.y + 2.0), g, ink);
    }
}

#[cfg(test)]
mod tests {
    use super::super::view3d::Camera;
    use super::*;

    fn dim(name: &str, a: [f64; 3], b: [f64; 3], kind: DimKind) -> Dim {
        Dim { name: name.into(), value: norm(sub(b, a)), kind, to: b, line: [a, b], part: "part".into(), main: true }
    }

    fn view(yaw: f32, pitch: f32) -> Projector {
        let cam = Camera { yaw, pitch, ..Default::default() };
        Projector::for_test(&cam, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)), [0.0; 3], 0.05)
    }

    fn whole() -> Rect {
        Rect::from_min_size(pos2(-5000.0, -5000.0), vec2(10000.0, 10000.0))
    }

    fn text_size(t: &str) -> Vec2 {
        vec2(t.chars().count() as f32 * 7.0, 14.0)
    }

    #[test]
    fn no_two_labels_are_written_over_each_other() {
        // Stacked on one edge, the way a sketch's a1, a2 and overall length are.
        let dims = [
            dim("L", [-0.035, -0.01, 0.0], [0.035, -0.01, 0.0], DimKind::Between),
            dim("a1", [-0.035, -0.01, 0.0], [-0.0305, -0.01, 0.0], DimKind::Between),
            dim("a2", [-0.0305, -0.01, 0.0], [-0.0245, -0.01, 0.0], DimKind::Between),
            dim("a3", [-0.0245, -0.01, 0.0], [-0.018, -0.01, 0.0], DimKind::Between),
        ];
        let pj = view(0.0, 1.55);
        let placed = place(&dims, &pj, pj.screen([0.0; 3]), whole(), text_size, None);
        assert_eq!(placed.len(), 4);
        for (i, x) in placed.iter().enumerate() {
            for y in &placed[..i] {
                assert!(!x.label.intersects(y.label), "{} over {}", x.text, y.text);
            }
        }
    }

    #[test]
    fn a_line_stands_on_the_outside_of_the_model() {
        let pj = view(0.0, 1.55);
        let d = [dim("L", [-0.03, -0.01, 0.0], [0.03, -0.01, 0.0], DimKind::Between)];
        let middle = pj.screen([0.0; 3]);
        let placed = place(&d, &pj, middle, whole(), text_size, None);
        let edge = pj.screen(d[0].line[0]);
        assert!((placed[0].label.center() - middle).length() > (edge + (pj.screen(d[0].line[1]) - edge) * 0.5 - middle).length(), "{placed:?}");
    }

    #[test]
    fn a_line_seen_end_on_is_left_out() {
        // A height, from straight above.
        let d = [dim("h", [0.01, 0.01, 0.0], [0.01, 0.01, 0.0016], DimKind::Between)];
        let pj = view(0.0, std::f32::consts::FRAC_PI_2);
        assert!(place(&d, &pj, pj.screen([0.0; 3]), whole(), text_size, None).is_empty());
        let side = view(0.6, 0.2);
        assert_eq!(place(&d, &side, side.screen([0.0; 3]), whole(), text_size, None).len(), 1);
    }

    #[test]
    fn a_label_that_would_go_under_a_card_comes_back_across_the_model() {
        // A height at the model's left edge, with the card's edge just beyond it.
        let pj = view(0.0, 1.55);
        let d = [dim("Hp", [-0.03, -0.01, 0.0], [-0.03, 0.01, 0.0], DimKind::Between)];
        let middle = pj.screen([0.0; 3]);
        let left = pj.screen(d[0].line[0]).x;
        let room = Rect::from_min_max(pos2(left - 4.0, 0.0), pos2(800.0, 600.0));
        let placed = place(&d, &pj, middle, room, text_size, None);
        assert!(room.contains_rect(placed[0].label), "{placed:?}");
        assert!(placed[0].label.center().x > left, "{placed:?}");
        // With the room to spare it stays outside.
        let placed = place(&d, &pj, middle, whole(), text_size, None);
        assert!(placed[0].label.center().x < left, "{placed:?}");
    }

    #[test]
    fn no_two_lines_lie_along_each_other() {
        // An overall length and a short part of it, on one edge.
        let dims = [dim("L", [-0.035, -0.01, 0.0], [0.035, -0.01, 0.0], DimKind::Between), dim("a1", [-0.035, -0.01, 0.0], [-0.0305, -0.01, 0.0], DimKind::Between)];
        let pj = view(0.0, 1.55);
        let placed = place(&dims, &pj, pj.screen([0.0; 3]), whole(), text_size, None);
        assert_eq!(placed.len(), 2);
        assert!(!runs_along(placed[0].lines[0], placed[1].lines[0]), "{placed:?}");
    }

    #[test]
    fn a_label_slides_along_its_line_to_find_room() {
        // A wide label left in the middle of the next line's way.
        let pj = view(0.0, 1.55);
        let short = dim("Hm", [-0.03, -0.002, 0.0], [-0.03, 0.002, 0.0], DimKind::Between);
        let tall = dim("Hp", [-0.03, -0.012, 0.0], [-0.03, 0.012, 0.0], DimKind::Between);
        let placed = place(&[short, tall], &pj, pj.screen([0.0; 3]), whole(), text_size, None);
        assert_eq!(placed.len(), 2);
        assert!(!placed[0].label.intersects(placed[1].label), "{placed:?}");
        // Near the model still: not pushed out past the whole width of the other label.
        let edge = pj.screen([-0.03, 0.0, 0.0]).x;
        assert!(edge - placed[1].label.center().x < 60.0, "{placed:?}");
    }

    #[test]
    fn lines_apart_or_crossing_dont_count_as_along() {
        let p = |x: f32, y: f32| pos2(x, y);
        assert!(runs_along([p(0.0, 0.0), p(100.0, 0.0)], [p(10.0, 2.0), p(30.0, 2.0)]));
        assert!(!runs_along([p(0.0, 0.0), p(100.0, 0.0)], [p(10.0, 20.0), p(30.0, 20.0)]));
        assert!(!runs_along([p(0.0, 0.0), p(100.0, 0.0)], [p(50.0, -20.0), p(50.0, 20.0)]));
        assert!(!runs_along([p(0.0, 0.0), p(100.0, 0.0)], [p(110.0, 0.0), p(130.0, 0.0)]));
    }

    #[test]
    fn a_corner_shows_only_while_its_row_is_pointed_at_and_moves_nothing_else() {
        let corner = Dim { main: false, ..dim("x1", [0.01, 0.0, 0.0], [0.03, 0.0, 0.0], DimKind::Between) };
        let dims = [dim("L", [-0.035, -0.01, 0.0], [0.035, -0.01, 0.0], DimKind::Between), dim("W", [-0.035, -0.01, 0.0], [-0.035, 0.01, 0.0], DimKind::Between), corner];
        let pj = view(0.0, 1.55);
        let quiet = place(&dims, &pj, pj.screen([0.0; 3]), whole(), text_size, None);
        assert_eq!(quiet.len(), 2, "{quiet:?}");
        assert!(quiet.iter().all(|p| !p.focus && !p.text.starts_with("x1")), "{quiet:?}");
        let pointed = place(&dims, &pj, pj.screen([0.0; 3]), whole(), text_size, Some("x1"));
        assert_eq!(pointed.len(), 3, "{pointed:?}");
        assert!(pointed[2].focus && pointed[2].text.starts_with("x1"), "{pointed:?}");
        for (a, b) in quiet.iter().zip(&pointed) {
            assert_eq!(a.label, b.label, "{} moved", a.text);
        }
        // A main one that's pointed at is only marked.
        let main = place(&dims, &pj, pj.screen([0.0; 3]), whole(), text_size, Some("W"));
        assert_eq!(main.len(), 2, "{main:?}");
        assert!(main.iter().any(|p| p.focus && p.text.starts_with('W')), "{main:?}");
    }

    #[test]
    fn a_tiny_size_still_gets_its_label() {
        // A wire's radius is less than a point on the screen, and its text is what matters.
        let d = [dim("a", [0.0, 0.0, 0.0], [0.00001, 0.0, 0.0], DimKind::Inside)];
        let pj = view(0.0, 1.55);
        let placed = place(&d, &pj, pj.screen([0.02, 0.0, 0.0]), whole(), text_size, None);
        assert_eq!(placed.len(), 1);
        assert!(placed[0].label.width() > 8.0);
    }
}
