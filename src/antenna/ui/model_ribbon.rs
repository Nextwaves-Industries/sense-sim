//! The Model tab of the app's ribbon: every modelling tool at once, in captioned groups, small ones
//! stacked in threes so they fit. While a sketch is open it shows what a sketch can use instead.

use bevy_egui::egui::{self, Align2, Color32, FontId, Sense, Stroke, StrokeKind, Ui, vec2};

use super::model3d::{Cmd, Glyph, ModelBar, SkTool, small_glyph};
use super::place::Shape;
use crate::theme::{self, icon};

/// Height of a stacked tool: three of these make a big button's height.
const SMALL_H: f32 = 18.0;

struct Bar<'a> {
    m: &'a ModelBar,
    run: Option<Cmd>,
}

impl Bar<'_> {
    /// A tool's symbol beside its name, one of a stack of three.
    #[allow(clippy::too_many_arguments)]
    fn small(&mut self, ui: &mut Ui, g: Glyph, text: &str, on: bool, selected: bool, tip: &str, c: Cmd) {
        let font = FontId::proportional(11.0);
        let width = ui.painter().layout_no_wrap(text.to_string(), font.clone(), theme::TEXT).size().x + 22.0;
        let (rect, r) = ui.allocate_exact_size(vec2(width, SMALL_H), if on { Sense::click() } else { Sense::hover() });
        let p = ui.painter();
        if selected {
            p.rect(rect, 3.0, theme::ACCENT_SOFT, Stroke::new(1.0, Color32::from_rgb(190, 212, 250)), StrokeKind::Inside);
        } else if on && r.hovered() {
            p.rect_filled(rect, 3.0, theme::HOVER);
        }
        small_glyph(ui, rect.left_center() + vec2(9.5, 0.0), 15.0, g, on);
        ui.painter().text(rect.left_center() + vec2(19.0, 0.0), Align2::LEFT_CENTER, text, font, if on { theme::TEXT } else { theme::FAINT });
        let r = if on { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
        let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
        if r.clicked() {
            self.run = Some(c);
        }
    }

    /// A tool's symbol over its name, the height of a stack.
    #[allow(clippy::too_many_arguments)]
    fn big(&mut self, ui: &mut Ui, g: Glyph, text: &str, on: bool, selected: bool, tip: &str, c: Cmd) {
        if super::model3d::cmd_button(ui, g, text, on, selected, tip) {
            self.run = Some(c);
        }
    }

    fn lit(&self, on: bool) -> bool {
        on && self.m.showing
    }

    fn placing(&self, s: Shape) -> bool {
        self.m.placing == Some(s)
    }
}

fn stack(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        add(ui);
    });
}

/// The line between two groups, closer in than the other tabs': this one has a lot to fit.
fn gap(ui: &mut Ui) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 58.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::STROKE);
    ui.add_space(2.0);
}

fn sketch_tools(ui: &mut Ui, b: &mut Bar) {
    let t = |b: &Bar, tool: SkTool| b.lit(b.m.tool == tool);
    theme::ribbon_group(ui, "Done", |ui| {
        b.big(ui, Glyph::Check, "Finish sketch", true, false, "Back to the model (Esc)", Cmd::ExitSketch);
    });
    theme::ribbon_divider(ui);
    theme::ribbon_group(ui, "Pick", |ui| {
        let on = t(b, SkTool::Select);
        b.big(ui, Glyph::Arrow, "Select", true, on, "Select and drag points and curves (S)", Cmd::Tool(SkTool::Select));
    });
    theme::ribbon_divider(ui);
    theme::ribbon_group(ui, "Draw", |ui| {
        for (g, text, tool, tip) in [
            (Glyph::Line, "Line", SkTool::Line, "A chain of lines (L). Enter or Esc ends it"),
            (Glyph::Rect, "Rectangle", SkTool::Rect, "Two corners (R)"),
            (Glyph::CentreRect, "Centre rect", SkTool::CentreRect, "From its centre to a corner"),
            (Glyph::Circle, "Circle", SkTool::Circle, "Centre, then the rim (C)"),
            (Glyph::Arc, "Arc", SkTool::Arc, "Centre, start, then end, counter-clockwise (A)"),
            (Glyph::Spline, "Spline", SkTool::Spline, "A smooth curve through points. Enter ends it"),
            (Glyph::Point, "Point", SkTool::Point, "A point, such as where a feed goes (P)"),
        ] {
            let on = t(b, tool);
            b.big(ui, g, text, true, on, tip, Cmd::Tool(tool));
        }
    });
    theme::ribbon_divider(ui);
    theme::ribbon_group(ui, "Size and shape", |ui| {
        let on = t(b, SkTool::Dimension);
        b.big(ui, Glyph::Dim, "Dimension", true, on, "Click a line, a circle, two points or two lines (D). Type a number or a variable", Cmd::Tool(SkTool::Dimension));
        let on = t(b, SkTool::Round);
        b.big(ui, Glyph::Round, "Round corner", true, on, "Click where two lines meet", Cmd::Tool(SkTool::Round));
        b.big(ui, Glyph::Dash, "Construction", true, false, "Make the selected curve a reference line", Cmd::Construction);
        b.big(ui, Glyph::Lock, "Fully define", true, false, "Add the dimensions it still needs, from where things are", Cmd::FullyDefine);
    });
    theme::ribbon_divider(ui);
    theme::ribbon_group(ui, "Make from it", |ui| {
        b.big(ui, Glyph::Sheet, "Make face", true, false, "The flat metal this closed sketch fills", Cmd::Sheet);
        b.big(ui, Glyph::Extrude, "Extrude", true, false, "Push this sketch out into a solid", Cmd::Extrude);
        b.big(ui, Glyph::Wire, "Wire", true, false, "A thin round conductor along this sketch", Cmd::Wire);
    });
}

fn model_tools(ui: &mut Ui, b: &mut Bar) {
    let m = *b.m;
    theme::ribbon_group(ui, "Tasks", |ui| {
        stack(ui, |ui| {
            b.small(ui, Glyph::Icon(icon::PLAY_CIRCLE), "Simulation", !m.busy, false, "Set up and run the solver", Cmd::Page("setup"));
            b.small(ui, Glyph::Icon(icon::ROCKET_LAUNCH), "Optimization", true, false, "Let it change the variables you ticked until the goals are met", Cmd::Page("optimize"));
            b.small(ui, Glyph::Icon(icon::STOP), "Stop", m.busy, false, "Stop what's running", Cmd::Stop);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Clipboard", |ui| {
        b.big(ui, Glyph::Paste, "Paste", m.can_paste, false, "A separate copy of what was copied, with its own dimensions (Ctrl+V)", Cmd::Paste);
        stack(ui, |ui| {
            b.small(ui, Glyph::Scissors, "Cut", m.has_selection, false, "Copy it, then remove it (Ctrl+X)", Cmd::ClipCut);
            b.small(ui, Glyph::Icon(icon::COPY), "Copy", m.has_selection, false, "Copy what's selected, and what it's made from (Ctrl+C)", Cmd::Copy);
            b.small(ui, Glyph::Icon(icon::TRASH), "Delete", m.has_selection, false, "Remove what's selected (Del)", Cmd::Delete);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Drawing 3D", |ui| {
        stack(ui, |ui| {
            let on = b.placing(Shape::Box_);
            b.small(ui, Glyph::Box_, "Box", true, on, "Click a corner, the size and the height, or type them", Cmd::Place(Shape::Box_));
            let on = b.placing(Shape::Cylinder);
            b.small(ui, Glyph::Cylinder, "Cylinder", true, on, "Click the centre, the radius and the height, or type them", Cmd::Place(Shape::Cylinder));
            let on = b.placing(Shape::Cone);
            b.small(ui, Glyph::Cone, "Cone", true, on, "Click the centre, the radius and the height, or type them", Cmd::Place(Shape::Cone));
        });
        stack(ui, |ui| {
            let on = b.placing(Shape::Sphere);
            b.small(ui, Glyph::Sphere, "Sphere", true, on, "Click the centre and the radius, or type them", Cmd::Place(Shape::Sphere));
            let on = b.placing(Shape::Torus);
            b.small(ui, Glyph::Torus, "Torus", true, on, "Click the centre, the ring and the tube, or type them", Cmd::Place(Shape::Torus));
            b.small(ui, Glyph::Helix, "Helix", true, false, "A wire wound along an axis. Its radius, pitch and turns are in Parameters", Cmd::Helix);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Drawing 2D", |ui| {
        stack(ui, |ui| {
            let on = b.placing(Shape::Rect);
            b.small(ui, Glyph::Rect, "Rectangle", true, on, "Flat metal: click a corner and the size, or type them", Cmd::Place(Shape::Rect));
            let on = b.placing(Shape::Circle);
            b.small(ui, Glyph::Circle, "Circle", true, on, "Flat metal: click the centre and the radius, or type them", Cmd::Place(Shape::Circle));
            let on = b.placing(Shape::Ellipse);
            b.small(ui, Glyph::Ellipse, "Ellipse", true, on, "Flat metal: click the centre and its size, or type them", Cmd::Place(Shape::Ellipse));
        });
        stack(ui, |ui| {
            let on = b.placing(Shape::Polygon);
            b.small(ui, Glyph::Polygon, "Polygon", true, on, "Flat metal, a regular polygon. Its number of sides is in Parameters", Cmd::Place(Shape::Polygon));
            b.small(ui, Glyph::Plane(2), "Sketch", true, false, "Start a sketch on the work plane, for an outline with dimensions and constraints", Cmd::SketchHere);
            let on = b.lit(m.picking_plane);
            b.small(ui, Glyph::Face, "On a face", m.has_body, on, "Click a flat face to sketch on it", Cmd::OnFace);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Drawing 1D", |ui| {
        stack(ui, |ui| {
            let on = b.placing(Shape::Line);
            b.small(ui, Glyph::Line, "Line", true, on, "A straight wire: click both ends, or type them", Cmd::Place(Shape::Line));
            let on = b.placing(Shape::Polyline);
            b.small(ui, Glyph::Polyline, "Polyline", true, on, "A wire through points: click each, Enter to end", Cmd::Place(Shape::Polyline));
            b.small(ui, Glyph::Spline, "B-spline", true, false, "A smooth curve through points, in a sketch on the work plane", Cmd::Tool(SkTool::Spline));
        });
        stack(ui, |ui| {
            b.small(ui, Glyph::Arc, "Arc", true, false, "An arc of a circle, in a sketch on the work plane", Cmd::Tool(SkTool::Arc));
            b.small(ui, Glyph::Wire, "Wire", m.has_sketch, false, "A thin round conductor along a sketch", Cmd::Wire);
            b.small(ui, Glyph::Sheet, "Make face", m.has_sketch, false, "The flat metal a closed sketch fills", Cmd::Sheet);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "CAD files", |ui| {
        stack(ui, |ui| {
            b.small(ui, Glyph::Import, "Import", true, false, "An STL, DXF or Gerber file", Cmd::ImportAny);
            b.small(ui, Glyph::Icon(icon::EXPORT), "Export", true, false, "STL, DXF, Gerber, and the results", Cmd::Page("export"));
            b.small(ui, Glyph::Icon(icon::TREE_STRUCTURE), "Bring in", m.loose_parts > 0, false, "Make a template's parts into objects with their own dimensions, so these tools can work on them. They stop following the template's formulas", Cmd::BringIn);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Boolean", |ui| {
        use antenna_core::modeler::CombineOp::{Add, Common, Cut, Exclude};
        stack(ui, |ui| {
            for (op, g, text, tip) in [
                (Add, Glyph::Join, "Unite", "Join the bodies selected into one. Ctrl+click to select several, solids or flat sheets"),
                (Cut, Glyph::Subtract, "Subtract", "Take the others from the first one selected"),
                (Common, Glyph::Intersect, "Intersect", "Keep only what they share"),
            ] {
                b.small(ui, g, text, m.can_combine, false, tip, Cmd::Boolean(op));
            }
        });
        stack(ui, |ui| {
            b.small(ui, Glyph::Exclude, "Exclude", m.can_combine, false, "Keep what's in one or the other, not both", Cmd::Boolean(Exclude));
            b.small(ui, Glyph::Cut, "Cut", m.has_sketch && m.has_body, false, "Cut a sketch's shape out of a body", Cmd::Cut);
            b.small(ui, Glyph::Shell, "Shell", m.has_body, false, "Hollow a solid out", Cmd::Shell);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Modifications", |ui| {
        stack(ui, |ui| {
            b.small(ui, Glyph::Move, "Move", m.has_body, false, "Move the body selected. How far is in Parameters", Cmd::Move);
            b.small(ui, Glyph::Icon(icon::ARROW_CLOCKWISE), "Rotate", m.has_body, false, "Turn the body selected. About what, and how far, is in Parameters", Cmd::Rotate);
            b.small(ui, Glyph::Mirror, "Reflect", m.has_body, false, "A mirror image of the body selected", Cmd::Mirror);
        });
        stack(ui, |ui| {
            b.small(ui, Glyph::Pattern, "Clone line", m.has_body, false, "Copies along a step: an array", Cmd::Pattern);
            b.small(ui, Glyph::Circular, "Clone axis", m.has_body, false, "Copies round an axis", Cmd::Circular);
            b.small(ui, Glyph::Thicken, "Thicken", m.has_body, false, "Give a curved surface a thickness. A flat sheet has its own, in Parameters", Cmd::Thicken);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Fillet and extrude", |ui| {
        stack(ui, |ui| {
            b.small(ui, Glyph::Fillet, "Fillet", m.has_body, false, "Round the edges you click", Cmd::Fillet);
            b.small(ui, Glyph::Chamfer, "Chamfer", m.has_body, false, "Bevel the edges you click", Cmd::Chamfer);
            b.small(ui, Glyph::Revolve, "Revolve", m.has_sketch, false, "Make a revolution: turn a sketch about an axis", Cmd::Revolve);
        });
        stack(ui, |ui| {
            b.small(ui, Glyph::Extrude, "Extrude", m.has_sketch || m.flat_selected, false, "Extrude along a vector: push a sketch out into a solid. On a flat shape selected, gives it a thickness", Cmd::Extrude);
            b.small(ui, Glyph::Sweep, "Sweep", m.has_sketch, false, "Extrude along a path: carry a section along it", Cmd::Sweep);
            b.small(ui, Glyph::Loft, "Loft", m.has_sketch, false, "Extrude ruled: blend between two or more sketches", Cmd::Loft);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Surfaces", |ui| {
        stack(ui, |ui| {
            b.small(ui, Glyph::SurfaceExtrude, "Bent sheet", m.has_sketch, false, "Push an open sketch out into a surface: a PIFA's plate and short", Cmd::SurfaceExtrude);
            b.small(ui, Glyph::Revolve, "Revolved", m.has_sketch, false, "A surface of revolution: a cone, a dish", Cmd::SurfaceRevolve);
            b.small(ui, Glyph::Loft, "Lofted", m.has_sketch, false, "Walls between sketches, open-ended: a horn", Cmd::SurfaceLoft);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Ports", |ui| {
        stack(ui, |ui| {
            let on = b.lit(m.picking_feed);
            b.small(ui, Glyph::Feed, "Feed", true, on, "A port between two points you click", Cmd::Feed);
            let on = b.lit(m.picking_probe);
            b.small(ui, Glyph::Probe, "Probe", m.has_sketch, on, "A port through a sketch point, between the metal it meets", Cmd::Probe);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Coordinates", |ui| {
        stack(ui, |ui| {
            for (axis, text, tip) in [(2, "XY", "Shapes and sketches go on the x-y plane"), (1, "XZ", "Shapes and sketches go on the x-z plane"), (0, "YZ", "Shapes and sketches go on the y-z plane")] {
                b.small(ui, Glyph::Plane(axis), text, true, m.work_axis == axis, tip, Cmd::WorkPlane(axis));
            }
        });
        stack(ui, |ui| {
            b.small(ui, Glyph::Icon(icon::PLUS_CIRCLE), "Add", true, false, "A new coordinate system to draw in. Its origin and turn are in Parameters", Cmd::CoordAdd);
            b.small(ui, Glyph::Icon(icon::ARROWS_OUT_CARDINAL), "Move", m.custom_coord, false, "Change where the coordinate system in use is", Cmd::CoordEdit);
            b.small(ui, Glyph::Icon(icon::ARROW_CLOCKWISE), "Rotate", m.custom_coord, false, "Change how the coordinate system in use is turned", Cmd::CoordEdit);
        });
    });
    gap(ui);

    theme::ribbon_group(ui, "Measure", |ui| {
        stack(ui, |ui| {
            let on = b.lit(m.picking_measure);
            b.small(ui, Glyph::Measure, "Length", m.has_parts, on, "The distance between two points you click", Cmd::Measure);
            let on = b.lit(m.picking_angle);
            b.small(ui, Glyph::Angle, "Angle", m.has_parts, on, "The angle at a point: click one end, the corner, then the other end", Cmd::MeasureAngle);
        });
        stack(ui, |ui| {
            let on = b.lit(m.section);
            b.small(ui, Glyph::Section, "Section", true, on, "Cut the view open to see inside", Cmd::Section);
            let on = b.lit(m.mesh_view);
            b.small(ui, Glyph::Mesh, "Mesh", true, on, "The mesh the solver will really use", Cmd::MeshView);
        });
    });
}

/// The whole tab. Gives the tool clicked.
pub fn tools(ui: &mut Ui, m: &ModelBar) -> Option<Cmd> {
    let mut b = Bar { m, run: None };
    let id = ui.make_persistent_id("studio_model_ribbon_at");
    let wanted: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    // On a narrow window the rest is a wheel-turn away, and an arrow says so. Nothing's cut off.
    let out = egui::ScrollArea::horizontal().id_salt("studio_model_ribbon").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).horizontal_scroll_offset(wanted).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
            if m.sketching {
                sketch_tools(ui, &mut b);
            } else {
                model_tools(ui, &mut b);
            }
        });
    });
    let (view, content, at) = (out.inner_rect, out.content_size.x, out.state.offset.x);
    let mut next = at;
    let mut arrow = |ui: &mut Ui, x: f32, glyph: &str, step: f32, tip: &str| {
        let r = egui::Rect::from_center_size(egui::pos2(x, view.center().y - 8.0), vec2(18.0, 46.0));
        let hit = ui.interact(r, ui.id().with(("ribbon_arrow", glyph)), Sense::click()).on_hover_text(tip);
        ui.painter().rect(r, 3.0, if hit.hovered() { theme::HOVER } else { theme::PANEL }, Stroke::new(1.0, theme::STROKE), StrokeKind::Inside);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(13.0), theme::TEXT);
        if hit.clicked() {
            next = (at + step).clamp(0.0, (content - view.width()).max(0.0));
        }
    };
    if at > 1.0 {
        arrow(ui, view.left() + 9.0, icon::CARET_LEFT, -view.width() * 0.7, "More tools to the left");
    }
    if at + view.width() < content - 1.0 {
        arrow(ui, view.right() - 9.0, icon::CARET_RIGHT, view.width() * 0.7, "More tools to the right");
    }
    ui.data_mut(|d| d.insert_temp(id, next));
    b.run
}
