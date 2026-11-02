//! Tests from hand-written IFC snippets. Expected values are worked out by hand
//! from the geometry, not copied from the program's output.

use super::*;
use crate::step::{Step, Value, decode_string};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

const METRE_UNITS: &str = "#9001=IFCUNITASSIGNMENT((#9002));#9002=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);";

/// A frame every test can use: origin `#1`, Z `#2`, X `#3`, identity axis placement
/// `#4`, identity local placement `#5`, and representation context `#6`.
const COMMON: &str = "
#1=IFCCARTESIANPOINT((0.,0.,0.));
#2=IFCDIRECTION((0.,0.,1.));
#3=IFCDIRECTION((1.,0.,0.));
#4=IFCAXIS2PLACEMENT3D(#1,#2,#3);
#5=IFCLOCALPLACEMENT($,#4);
#6=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#4,$);
";

fn file_with(units: &str, body: &str) -> String {
    format!(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
         FILE_NAME('t.ifc','2024-01-01T00:00:00',('a'),('b'),'c','d','e');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
         #9000=IFCPROJECT('proj',$,'Project',$,$,$,$,$,#9001);\n{units}\n{COMMON}\n{body}\nENDSEC;\nEND-ISO-10303-21;\n"
    )
}

fn load_with(units: &str, body: &str) -> Building {
    read(&file_with(units, body)).unwrap()
}

fn load(body: &str) -> Building {
    load_with(METRE_UNITS, body)
}

fn only(building: &Building) -> &Element {
    assert_eq!(building.elements.len(), 1, "skipped: {:?}", building.skipped);
    &building.elements[0]
}

#[track_caller]
fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "got {actual}, expected {expected}");
}

#[track_caller]
fn close3(actual: [f64; 3], expected: [f64; 3]) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 1e-9,
            "axis {axis}: got {actual:?}, expected {expected:?}"
        );
    }
}

/// Item `#13`: an `l` by `t` rectangle from the origin along +X, up by `h`.
fn wall_solid(l: f64, t: f64, h: f64) -> String {
    format!(
        "#10=IFCCARTESIANPOINT(({half},0.));
         #11=IFCAXIS2PLACEMENT2D(#10,$);
         #12=IFCRECTANGLEPROFILEDEF(.AREA.,$,#11,{l:?},{t:?});
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,{h:?});",
        half = format_args!("{:?}", l / 2.0),
    )
}

/// Shape `#20` with a Body holding the given items.
fn body_shape(items: &str) -> String {
    format!(
        "#19=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',({items}));
         #20=IFCPRODUCTDEFINITIONSHAPE($,$,(#19));"
    )
}

/// Wall `#30` using shape `#20`.
fn wall_entity(placement: &str) -> String {
    format!("#30=IFCWALL('wall-guid',$,'Wall 1',$,$,{placement},#20,$,$);")
}

/// Placement `#40` at a point with X rotated `yaw` about Z.
fn placement_at(p: [f64; 3], yaw: f64, parent: &str) -> String {
    let (s, c) = yaw.sin_cos();
    format!(
        "#41=IFCCARTESIANPOINT(({:?},{:?},{:?}));
         #42=IFCDIRECTION(({c:?},{s:?},0.));
         #43=IFCAXIS2PLACEMENT3D(#41,#2,#42);
         #40=IFCLOCALPLACEMENT({parent},#43);",
        p[0], p[1], p[2]
    )
}

fn simple_wall(l: f64, t: f64, h: f64) -> String {
    format!("{}{}{}", wall_solid(l, t, h), body_shape("#13"), wall_entity("#5"))
}

// ---------------------------------------------------------------- units

#[test]
fn metres_pass_through() {
    let b = load(&simple_wall(5.0, 0.2, 3.0));
    close(b.unit_to_m, 1.0);
    let e = only(&b);
    close3(e.min, [0.0, -0.1, 0.0]);
    close3(e.max, [5.0, 0.1, 3.0]);
    assert_eq!(b.skipped, Vec::<(String, String)>::new());
}

#[test]
fn millimetres_scale_to_metres() {
    let units = "#9001=IFCUNITASSIGNMENT((#9002));#9002=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);";
    let b = load_with(units, &simple_wall(5000.0, 200.0, 3000.0));
    close(b.unit_to_m, 0.001);
    let e = only(&b);
    close3(e.size, [5.0, 0.2, 3.0]);
    close3(e.max, [5.0, 0.1, 3.0]);
    close(e.thickness_m.unwrap(), 0.2);
}

#[test]
fn every_si_prefix_is_read() {
    for (prefix, factor) in [("KILO", 1e3), ("DECI", 0.1), ("CENTI", 0.01), ("MICRO", 1e-6), ("HECTO", 100.0)] {
        let units = format!("#9001=IFCUNITASSIGNMENT((#9002));#9002=IFCSIUNIT(*,.LENGTHUNIT.,.{prefix}.,.METRE.);");
        let b = load_with(&units, &simple_wall(5.0, 0.2, 3.0));
        close(b.unit_to_m, factor);
        close(only(&b).size[0], 5.0 * factor);
    }
}

#[test]
fn feet_through_conversion_based_unit() {
    let units = "#9001=IFCUNITASSIGNMENT((#9002));
        #9002=IFCCONVERSIONBASEDUNIT(#9003,.LENGTHUNIT.,'FOOT',#9004);
        #9003=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
        #9004=IFCMEASUREWITHUNIT(IFCRATIOMEASURE(0.3048),#9005);
        #9005=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);";
    let b = load_with(units, &simple_wall(10.0, 1.0, 9.0));
    close(b.unit_to_m, 0.3048);
    close3(only(&b).size, [3.048, 0.3048, 2.7432]);
}

#[test]
fn inch_defined_in_millimetres() {
    let units = "#9001=IFCUNITASSIGNMENT((#9002));
        #9002=IFCCONVERSIONBASEDUNIT(*,.LENGTHUNIT.,'INCH',#9004);
        #9004=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(25.4),#9005);
        #9005=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);";
    close(load_with(units, &simple_wall(100.0, 8.0, 96.0)).unit_to_m, 0.0254);
}

#[test]
fn unusable_conversion_falls_back_to_the_units_name() {
    let units = "#9001=IFCUNITASSIGNMENT((#9002));
        #9002=IFCCONVERSIONBASEDUNIT(*,.LENGTHUNIT.,'FOOT',$);";
    close(load_with(units, &simple_wall(10.0, 1.0, 9.0)).unit_to_m, 0.3048);
}

#[test]
fn other_unit_types_are_not_taken_for_length() {
    let units = "#9001=IFCUNITASSIGNMENT((#9006,#9007,#9002));
        #9006=IFCSIUNIT(*,.AREAUNIT.,.MILLI.,.SQUARE_METRE.);
        #9007=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
        #9002=IFCSIUNIT(*,.LENGTHUNIT.,.CENTI.,.METRE.);";
    close(load_with(units, &simple_wall(10.0, 1.0, 9.0)).unit_to_m, 0.01);
}

#[test]
fn missing_length_unit_assumes_metres_and_says_so() {
    let units = "#9001=IFCUNITASSIGNMENT(());";
    let b = load_with(units, &simple_wall(5.0, 0.2, 3.0));
    close(b.unit_to_m, 1.0);
    assert!(b.skipped.iter().any(|(ty, reason)| ty == "IFCUNITASSIGNMENT" && reason.contains("assuming metres")));
    assert_eq!(b.elements.len(), 1);
}

#[test]
fn degree_trims_use_the_plane_angle_unit() {
    let units = "#9001=IFCUNITASSIGNMENT((#9002,#9006));
        #9002=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
        #9006=IFCCONVERSIONBASEDUNIT(*,.PLANEANGLEUNIT.,'DEGREE',#9007);
        #9007=IFCMEASUREWITHUNIT(IFCPLANEANGLEMEASURE(0.017453292519943295),#9008);
        #9008=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);";
    // A half disc of radius 1 from 0 to 180 degrees: reaches y = 1 but not y = -1.
    let body = format!(
        "#50=IFCAXIS2PLACEMENT2D(#51,$);#51=IFCCARTESIANPOINT((0.,0.));
         #52=IFCCIRCLE(#50,1.);
         #53=IFCTRIMMEDCURVE(#52,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#53);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load_with(units, &body);
    let e = only(&b);
    assert!((e.max[1] - 1.0).abs() < 1e-3 && e.min[1].abs() < 1e-9, "{e:?}");
    assert!(!e.exact, "a sampled arc is not exact");
}

// ------------------------------------------------------------ placement

#[test]
fn placement_offsets_the_element() {
    let body = format!("{}{}{}{}", wall_solid(5.0, 0.2, 3.0), body_shape("#13"), placement_at([10.0, 20.0, 1.0], 0.0, "$"), wall_entity("#40"));
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [10.0, 19.9, 1.0]);
    close3(e.max, [15.0, 20.1, 4.0]);
    close3(e.center, [12.5, 20.0, 2.5]);
    close3(e.size, [5.0, 0.2, 3.0]);
    close(e.yaw_rad, 0.0);
    assert!(e.exact);
}

#[test]
fn placement_chain_composes_rotation_and_translation() {
    // Parent at x=100 turned 90 degrees; child 10 along the parent's X and 3 up.
    // The parent's X is world +Y, so the child origin is (100, 10, 3), facing +Y.
    let body = format!(
        "{}{}
         #60=IFCCARTESIANPOINT((100.,0.,0.));
         #61=IFCDIRECTION((0.,1.,0.));
         #62=IFCAXIS2PLACEMENT3D(#60,#2,#61);
         #63=IFCLOCALPLACEMENT($,#62);
         {}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        placement_at([10.0, 0.0, 3.0], 0.0, "#63"),
        wall_entity("#40"),
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [99.9, 10.0, 3.0]);
    close3(e.max, [100.1, 15.0, 6.0]);
    close(e.yaw_rad, FRAC_PI_2);
    close3(e.size, [5.0, 0.2, 3.0]);
}

#[test]
fn three_level_chain_with_rotations_about_z_adds_yaw() {
    let body = format!(
        "{}{}
         #60=IFCDIRECTION(({c:?},{s:?},0.));
         #62=IFCAXIS2PLACEMENT3D(#1,#2,#60);
         #63=IFCLOCALPLACEMENT($,#62);
         #64=IFCLOCALPLACEMENT(#63,#62);
         {}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        placement_at([0.0; 3], 15f64.to_radians(), "#64"),
        wall_entity("#40"),
        c = 30f64.to_radians().cos(),
        s = 30f64.to_radians().sin(),
    );
    close(only(&load(&body)).yaw_rad, 75f64.to_radians());
}

#[test]
fn non_default_placement_axes_tip_the_extrusion_sideways() {
    // Local Z = world +Y and local X = world +X, so local Y = Z x X = world -Z.
    // A rectangle 1 along X and 2 along local Y, extruded 4 along world +Y.
    let body = format!(
        "#60=IFCDIRECTION((0.,1.,0.));
         #62=IFCAXIS2PLACEMENT3D(#1,#60,#3);
         #63=IFCLOCALPLACEMENT($,#62);
         #12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,2.);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,4.);
         {}{}",
        body_shape("#13"),
        wall_entity("#63")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [-0.5, 0.0, -1.0]);
    close3(e.max, [0.5, 4.0, 1.0]);
    close3(e.size, [4.0, 1.0, 2.0]);
    assert!(e.exact, "all axes are horizontal or vertical: {e:?}");
}

#[test]
fn item_position_with_non_default_axes() {
    // Beam along +X: position Z = world X, position X = world Y, so position
    // Y = Z x X = world Z. Rectangle 0.3 wide along Y and 0.5 deep along Z.
    let body = format!(
        "#60=IFCDIRECTION((0.,1.,0.));
         #62=IFCAXIS2PLACEMENT3D(#1,#3,#60);
         #12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.3,0.5);
         #13=IFCEXTRUDEDAREASOLID(#12,#62,#2,6.);
         {}#30=IFCBEAM('b',$,'Beam',$,$,#5,#20,$,$);",
        body_shape("#13"),
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.kind, ElementKind::Beam);
    close3(e.min, [0.0, -0.15, -0.25]);
    close3(e.max, [6.0, 0.15, 0.25]);
    close3(e.size, [6.0, 0.3, 0.5]);
    close(e.yaw_rad, 0.0);
    assert!(e.exact);
}

#[test]
fn omitted_axes_use_the_ifc_defaults() {
    let body = format!(
        "#62=IFCAXIS2PLACEMENT3D(#1,$,$);#63=IFCLOCALPLACEMENT($,#62);{}{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#63")
    );
    close3(only(&load(&body)).max, [5.0, 0.1, 3.0]);
}

#[test]
fn ref_direction_parallel_to_axis_falls_back_instead_of_breaking() {
    let body = format!(
        "#62=IFCAXIS2PLACEMENT3D(#1,#2,#2);#63=IFCLOCALPLACEMENT($,#62);{}{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#63")
    );
    close3(only(&load(&body)).max, [5.0, 0.1, 3.0]);
}

#[test]
fn ref_direction_is_projected_perpendicular_to_axis() {
    // (1,1,5) with its Z removed points at 45 degrees.
    let body = format!(
        "#60=IFCDIRECTION((1.,1.,5.));#62=IFCAXIS2PLACEMENT3D(#1,#2,#60);#63=IFCLOCALPLACEMENT($,#62);{}{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#63")
    );
    close(only(&load(&body)).yaw_rad, FRAC_PI_4);
}

#[test]
fn absent_object_placement_is_the_identity() {
    let body = format!("{}{}{}", wall_solid(5.0, 0.2, 3.0), body_shape("#13"), wall_entity("$"));
    close3(only(&load(&body)).max, [5.0, 0.1, 3.0]);
}

#[test]
fn two_dimensional_relative_placement() {
    let body = format!(
        "#60=IFCCARTESIANPOINT((7.,8.));#61=IFCDIRECTION((0.,1.));#62=IFCAXIS2PLACEMENT2D(#60,#61);
         #63=IFCLOCALPLACEMENT($,#62);{}{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#63")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [6.9, 8.0, 0.0]);
    close3(e.max, [7.1, 13.0, 3.0]);
}

// ------------------------------------------------------------- rotation

/// Corners of the oriented box in world coordinates.
fn box_corners(e: &Element) -> Vec<[f64; 3]> {
    let (s, c) = e.yaw_rad.sin_cos();
    let mut out = Vec::new();
    for sx in [-0.5, 0.5] {
        for sy in [-0.5, 0.5] {
            for sz in [-0.5, 0.5] {
                let (x, y, z) = (e.size[0] * sx, e.size[1] * sy, e.size[2] * sz);
                out.push([e.center[0] + x * c - y * s, e.center[1] + x * s + y * c, e.center[2] + z]);
            }
        }
    }
    out
}

#[test]
fn rotated_wall_reports_yaw_and_a_true_world_box() {
    let (l, t) = (5.0, 0.2);
    let yaw = 30f64.to_radians();
    let body = format!("{}{}{}{}", wall_solid(l, t, 3.0), body_shape("#13"), placement_at([0.0; 3], yaw, "$"), wall_entity("#40"));
    let b = load(&body);
    let e = only(&b);
    let (s, c) = yaw.sin_cos();
    // Corners are (0|l, +-t/2) rotated by yaw.
    close(e.max[0], l * c + t / 2.0 * s);
    close(e.min[0], -(t / 2.0) * s);
    close(e.max[1], l * s + t / 2.0 * c);
    close(e.min[1], -(t / 2.0) * c);
    close(e.yaw_rad, yaw);
    close3(e.size, [l, t, 3.0]);
    close3(e.center, [l / 2.0 * c, l / 2.0 * s, 1.5]);
}

#[test]
fn oriented_box_reproduces_the_world_box_at_any_yaw() {
    for degrees in [-170.0, -135.0, -90.0, -30.0, 0.0, 12.5, 45.0, 90.0, 135.0, 179.0, 180.0] {
        let body = format!(
            "{}{}{}{}",
            wall_solid(5.0, 0.3, 3.0),
            body_shape("#13"),
            placement_at([3.0, -4.0, 2.0], f64::to_radians(degrees), "$"),
            wall_entity("#40")
        );
        let b = load(&body);
        let e = only(&b);
        let corners = box_corners(e);
        for axis in 0..3 {
            let lo = corners.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min);
            let hi = corners.iter().map(|p| p[axis]).fold(f64::NEG_INFINITY, f64::max);
            assert!((lo - e.min[axis]).abs() < 1e-9 && (hi - e.max[axis]).abs() < 1e-9, "{degrees} deg: {e:?}");
        }
        assert!(e.yaw_rad > -PI - 1e-12 && e.yaw_rad <= PI + 1e-12);
        close3(e.size, [5.0, 0.3, 3.0]);
    }
}

#[test]
fn short_local_x_is_turned_so_the_long_side_comes_first() {
    // Local X across the wall: XDim 0.2, YDim 5. The long side is local Y.
    let body = format!(
        "#12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.2,5.);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,3.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.size, [5.0, 0.2, 3.0]);
    close(e.yaw_rad, FRAC_PI_2);
    close(e.thickness_m.unwrap(), 0.2);
}

#[test]
fn outline_drawn_in_world_coordinates_gets_its_yaw_from_the_outline() {
    // Identity placement, but a rectangle turned 30 degrees written out as a polyline.
    let (s, c) = 30f64.to_radians().sin_cos();
    let (l, t) = (6.0, 0.25);
    let corner = |u: f64, v: f64| format!("({:?},{:?})", u * c - v * s, u * s + v * c);
    let body = format!(
        "#50=IFCCARTESIANPOINT({});#51=IFCCARTESIANPOINT({});#52=IFCCARTESIANPOINT({});#53=IFCCARTESIANPOINT({});
         #54=IFCPOLYLINE((#50,#51,#52,#53,#50));
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#54);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,3.);{}{}",
        corner(0.0, 0.0),
        corner(l, 0.0),
        corner(l, t),
        corner(0.0, t),
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    assert!((e.yaw_rad - 30f64.to_radians()).abs() < 1e-9, "{e:?}");
    close3(e.size, [l, t, 3.0]);
    assert!(e.exact);
}

/// A polygon with `n` corners of radius `r`, the first on +X.
fn regular_polygon_profile(n: usize, r: f64, yaw: f64) -> String {
    let rows: Vec<String> = (0..n)
        .map(|k| {
            let (s, c) = (yaw + k as f64 / n as f64 * std::f64::consts::TAU).sin_cos();
            format!("({:?},{:?})", r * c, r * s)
        })
        .collect();
    format!(
        "#50=IFCCARTESIANPOINTLIST2D(({}),$);
         #51=IFCINDEXEDPOLYCURVE(#50,$,.F.);
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#51);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,3.);",
        rows.join(",")
    )
}

#[test]
fn faceted_round_column_keeps_its_frame() {
    // Revit writes a pipe pile this way. An edge-aligned box would be cos(pi/20),
    // 1.2 %, narrower per side, but that is no reason to turn it by an arbitrary angle.
    let body = format!(
        "{}{}{}#30=IFCCOLUMN('c',$,'Pile',$,$,#40,#20,$,$);",
        regular_polygon_profile(20, 0.15, 0.0),
        body_shape("#13"),
        placement_at([1.0, 2.0, 0.0], 0.0, "$"),
    );
    let b = load(&body);
    let e = only(&b);
    close(e.yaw_rad, 0.0);
    close3(e.size, [0.3, 0.3, 3.0]);
    close3(e.center, [1.0, 2.0, 1.5]);
    close3(e.min, [0.85, 1.85, 0.0]);
}

#[test]
fn clearly_better_box_replaces_the_frame_even_when_not_rectangular() {
    // A long thin triangle at 30 degrees under an identity placement. Its own frame
    // is far tighter than the world axes, though a triangle fills only half a box.
    let (s, c) = 30f64.to_radians().sin_cos();
    let corner = |u: f64, v: f64| format!("({:?},{:?})", u * c - v * s, u * s + v * c);
    let body = format!(
        "#50=IFCCARTESIANPOINTLIST2D(({},{},{}),$);
         #51=IFCINDEXEDPOLYCURVE(#50,$,.F.);
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#51);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        corner(0.0, 0.0),
        corner(10.0, 0.0),
        corner(5.0, 1.0),
        body_shape("#13"),
        wall_entity("#5"),
    );
    let b = load(&body);
    let e = only(&b);
    assert!((e.yaw_rad - 30f64.to_radians()).abs() < 1e-9, "{e:?}");
    close3(e.size, [10.0, 1.0, 1.0]);
}

#[test]
fn pitched_roof_is_marked_inexact_but_keeps_its_true_thickness() {
    // Slab tilted 30 degrees about X, with a 0.25 extrusion normal to its surface.
    let (s, c) = 30f64.to_radians().sin_cos();
    let body = format!(
        "#60=IFCDIRECTION((0.,{:?},{:?}));
         #62=IFCAXIS2PLACEMENT3D(#1,#60,#3);
         #12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,8.,4.);
         #13=IFCEXTRUDEDAREASOLID(#12,#62,#2,0.25);{}
         #30=IFCSLAB('s',$,'Roof',$,$,#5,#20,$,.ROOF.);",
        -s,
        c,
        body_shape("#13")
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.kind, ElementKind::Roof);
    assert!(!e.exact, "a tilted slab does not fit an upright box");
    close(e.thickness_m.unwrap(), 0.25);
    // The height is 4 sin(30) plus 0.25 cos(30), much more than the thickness.
    close(e.size[2], 4.0 * s + 0.25 * c);
}

// ------------------------------------------------------- profile kinds

#[test]
fn circle_profile_keeps_an_exact_box_in_a_rotated_frame() {
    let body = format!(
        "#10=IFCCARTESIANPOINT((1.,2.));#11=IFCAXIS2PLACEMENT2D(#10,$);
         #12=IFCCIRCLEPROFILEDEF(.AREA.,$,#11,0.25);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,3.);{}{}
         #30=IFCCOLUMN('c',$,'Column',$,$,#40,#20,$,$);",
        body_shape("#13"),
        placement_at([10.0, 0.0, 0.0], 0.3, "$"),
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.kind, ElementKind::Column);
    let (s, c) = 0.3f64.sin_cos();
    let center = [10.0 + c - 2.0 * s, s + 2.0 * c];
    close3(e.min, [center[0] - 0.25, center[1] - 0.25, 0.0]);
    close3(e.max, [center[0] + 0.25, center[1] + 0.25, 3.0]);
    close3(e.size, [0.5, 0.5, 3.0]);
    close(e.yaw_rad, 0.3);
    assert_eq!(e.thickness_m, None);
    assert!(e.exact);
}

#[test]
fn hollow_and_structural_sections_use_their_outer_envelope() {
    let cases = [
        ("IFCRECTANGLEHOLLOWPROFILEDEF(.AREA.,$,$,0.2,0.4,0.01,$,$)", [0.2, 0.4], true),
        ("IFCROUNDEDRECTANGLEPROFILEDEF(.AREA.,$,$,0.2,0.4,0.01)", [0.2, 0.4], true),
        ("IFCCIRCLEHOLLOWPROFILEDEF(.AREA.,$,$,0.1,0.01)", [0.2, 0.2], true),
        ("IFCELLIPSEPROFILEDEF(.AREA.,$,$,0.1,0.3)", [0.2, 0.6], true),
        ("IFCISHAPEPROFILEDEF(.AREA.,$,$,0.2,0.4,0.01,0.02,$)", [0.2, 0.4], true),
        ("IFCASYMMETRICISHAPEPROFILEDEF(.AREA.,$,$,0.2,0.4,0.01,0.02,$,0.3,0.02,$,$)", [0.3, 0.4], true),
        ("IFCUSHAPEPROFILEDEF(.AREA.,$,$,0.4,0.2,0.01,0.02,$,$,$)", [0.2, 0.4], true),
        ("IFCCSHAPEPROFILEDEF(.AREA.,$,$,0.4,0.2,0.01,0.05,$)", [0.2, 0.4], true),
        ("IFCTSHAPEPROFILEDEF(.AREA.,$,$,0.4,0.2,0.01,0.02,$,$,$,$,$)", [0.2, 0.4], false),
        ("IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.4,0.2,0.01,$,$,$)", [0.2, 0.4], false),
        ("IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.4,$,0.01,$,$,$)", [0.4, 0.4], false),
        ("IFCZSHAPEPROFILEDEF(.AREA.,$,$,0.4,0.2,0.04,0.02,$,$)", [0.36, 0.4], false),
    ];
    for (profile, dims, exact) in cases {
        let body = format!(
            "#12={profile};#13=IFCEXTRUDEDAREASOLID(#12,#4,#2,3.);{}#30=IFCBEAM('b',$,'B',$,$,#5,#20,$,$);",
            body_shape("#13")
        );
        let b = load(&body);
        let e = only(&b);
        assert!((e.max[0] - e.min[0] - dims[0]).abs() < 1e-9, "{profile}: {e:?}");
        assert!((e.max[1] - e.min[1] - dims[1]).abs() < 1e-9, "{profile}: {e:?}");
        close(e.size[2], 3.0);
        assert_eq!(e.exact, exact, "{profile}");
    }
}

#[test]
fn arbitrary_profile_over_a_polyline() {
    // An L: bounds are the outer corners. A box cannot show the notch, and the
    // extents are still exact, so it stays exact.
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.));#51=IFCCARTESIANPOINT((4.,0.));#52=IFCCARTESIANPOINT((4.,1.));
         #53=IFCCARTESIANPOINT((1.,1.));#54=IFCCARTESIANPOINT((1.,3.));#55=IFCCARTESIANPOINT((0.,3.));
         #56=IFCPOLYLINE((#50,#51,#52,#53,#54,#55,#50));
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#56);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,0.3);{}
         #30=IFCSLAB('s',$,'Floor',$,$,#5,#20,$,.FLOOR.);",
        body_shape("#13")
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.kind, ElementKind::Slab);
    close3(e.min, [0.0, 0.0, 0.0]);
    close3(e.max, [4.0, 3.0, 0.3]);
    close(e.thickness_m.unwrap(), 0.3);
    assert!(e.exact);
}

#[test]
fn profile_with_voids_uses_the_outer_curve() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.));#51=IFCCARTESIANPOINT((4.,0.));#52=IFCCARTESIANPOINT((4.,2.));#53=IFCCARTESIANPOINT((0.,2.));
         #54=IFCPOLYLINE((#50,#51,#52,#53,#50));
         #60=IFCCARTESIANPOINT((1.,1.));#61=IFCCARTESIANPOINT((2.,1.));#62=IFCCARTESIANPOINT((2.,1.5));
         #63=IFCPOLYLINE((#60,#61,#62,#60));
         #12=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#54,(#63));
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    close3(only(&load(&body)).max, [4.0, 2.0, 1.0]);
}

#[test]
fn indexed_polycurve_profile() {
    let body = format!(
        "#50=IFCCARTESIANPOINTLIST2D(((0.,0.),(2.,0.),(2.,1.),(0.,1.)),$);
         #51=IFCINDEXEDPOLYCURVE(#50,(IFCLINEINDEX((1,2,3,4,1))),$);
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#51);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    close3(only(&b).max, [2.0, 1.0, 1.0]);
    assert!(only(&b).exact);
}

#[test]
fn composite_curve_with_polyline_and_arc_segments() {
    // A stadium end: a polyline side, then a half circle bulging past x = 2.
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.));#51=IFCCARTESIANPOINT((2.,0.));#52=IFCCARTESIANPOINT((2.,2.));#53=IFCCARTESIANPOINT((0.,2.));
         #54=IFCPOLYLINE((#53,#50,#51));
         #55=IFCCARTESIANPOINT((2.,1.));#56=IFCAXIS2PLACEMENT2D(#55,$);#57=IFCCIRCLE(#56,1.);
         #58=IFCTRIMMEDCURVE(#57,(#51),(#52),.T.,.CARTESIAN.);
         #59=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#54);
         #60=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#58);
         #61=IFCCOMPOSITECURVE((#59,#60),.F.);
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#61);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    assert!((e.max[0] - 3.0).abs() < 1e-3, "{e:?}");
    close(e.max[1], 2.0);
    close(e.min[0], 0.0);
    assert!(!e.exact);
}

#[test]
fn trimmed_line_segment_is_read_from_parameters() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((1.,1.));#51=IFCDIRECTION((1.,0.));#52=IFCVECTOR(#51,2.);#53=IFCLINE(#50,#52);
         #54=IFCTRIMMEDCURVE(#53,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(3.)),.T.,.PARAMETER.);
         #55=IFCCARTESIANPOINT((1.,4.));#56=IFCPOLYLINE((#55,#50));
         #57=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#54);#58=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#56);
         #59=IFCCOMPOSITECURVE((#57,#58),.F.);
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#59);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    // Parameter 3 along a vector of magnitude 2 is 6 from (1,1).
    let b = load(&body);
    close3(only(&b).max, [7.0, 4.0, 1.0]);
}

#[test]
fn composite_profile_spans_all_parts() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((10.,0.));#51=IFCAXIS2PLACEMENT2D(#50,$);
         #52=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);#53=IFCCIRCLEPROFILEDEF(.AREA.,$,#51,1.);
         #12=IFCCOMPOSITEPROFILEDEF(.AREA.,$,(#52,#53),$);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    close3(only(&b).min, [-0.5, -1.0, 0.0]);
    close3(only(&b).max, [11.0, 1.0, 1.0]);
}

#[test]
fn derived_profile_applies_its_operator() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((10.,0.));#51=IFCCARTESIANTRANSFORMATIONOPERATOR2D($,$,#50,2.);
         #52=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);
         #12=IFCDERIVEDPROFILEDEF(.AREA.,$,#52,#51,$);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#2,1.);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    close3(only(&b).min, [9.0, -1.0, 0.0]);
    close3(only(&b).max, [11.0, 1.0, 1.0]);
}

#[test]
fn oblique_extrusion_shears_the_box_and_is_not_exact() {
    let body = format!(
        "#60=IFCDIRECTION((1.,0.,1.));
         #12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);
         #13=IFCEXTRUDEDAREASOLID(#12,#4,#60,{:?});{}{}",
        2f64.sqrt() * 3.0,
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.max, [3.5, 0.5, 3.0]);
    assert!(!e.exact);
}

#[test]
fn tapered_extrusion_bounds_both_ends() {
    let body = format!(
        "#50=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);#51=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,3.,1.);
         #13=IFCEXTRUDEDAREASOLIDTAPERED(#50,#4,#2,2.,#51);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    close3(only(&b).max, [1.5, 0.5, 2.0]);
    assert!(!only(&b).exact);
}

#[test]
fn revolved_solid_sweeps_around_its_axis() {
    // A small square 1..2 out from the Y axis, turned a half circle about Y: reaches z = 2.
    let units = "#9001=IFCUNITASSIGNMENT((#9002));#9002=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);";
    let body = format!(
        "#50=IFCCARTESIANPOINT((1.5,0.5));#51=IFCAXIS2PLACEMENT2D(#50,$);
         #52=IFCRECTANGLEPROFILEDEF(.AREA.,$,#51,1.,1.);
         #53=IFCDIRECTION((0.,1.,0.));#54=IFCAXIS1PLACEMENT(#1,#53);
         #13=IFCREVOLVEDAREASOLID(#52,#4,#54,{:?});{}{}",
        PI,
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load_with(units, &body);
    let e = only(&b);
    assert!((e.max[0] - 2.0).abs() < 1e-9 && (e.min[0] + 2.0).abs() < 1e-9, "{e:?}");
    close(e.max[1], 1.0);
    assert!(!e.exact);
}

#[test]
fn csg_block_and_cylinder() {
    let body = format!(
        "#50=IFCBLOCK(#4,1.,2.,3.);#51=IFCCSGSOLID(#50);{}{}",
        body_shape("#51"),
        wall_entity("#5")
    );
    close3(only(&load(&body)).max, [1.0, 2.0, 3.0]);
    let body = format!(
        "#50=IFCRIGHTCIRCULARCYLINDER(#4,3.,0.5);{}{}",
        body_shape("#50"),
        wall_entity("#5")
    );
    close3(only(&load(&body)).max, [0.5, 0.5, 3.0]);
}

#[test]
fn swept_disk_bounds_the_directrix() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.,0.));#51=IFCCARTESIANPOINT((2.,0.,1.));#52=IFCPOLYLINE((#50,#51));
         #53=IFCSWEPTDISKSOLID(#52,0.1,$,$,$);{}#30=IFCRAILING('r',$,'R',$,$,#5,#20,$,$);",
        body_shape("#53"),
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.kind, ElementKind::Other);
    close3(e.min, [-0.1, -0.1, -0.1]);
    close3(e.max, [2.1, 0.1, 1.1]);
}

// ---------------------------------------------------------- mapped items

/// Representation map `#70` of an 1 by 1 by 1 box at the origin.
const UNIT_BOX_MAP: &str = "
#71=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);
#72=IFCEXTRUDEDAREASOLID(#71,#4,#2,1.);
#73=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#72));
#70=IFCREPRESENTATIONMAP(#4,#73);
";

#[test]
fn mapped_item_applies_target_scale_and_placement() {
    let body = format!(
        "{UNIT_BOX_MAP}
         #74=IFCCARTESIANPOINT((10.,0.,0.));
         #75=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#74,2.,$);
         #13=IFCMAPPEDITEM(#70,#75);
         {}{}{}",
        body_shape("#13"),
        placement_at([0.0, 5.0, 0.0], 0.0, "$"),
        wall_entity("#40")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [9.0, 4.0, 0.0]);
    close3(e.max, [11.0, 6.0, 2.0]);
    assert!(e.exact);
}

#[test]
fn mapped_item_with_non_uniform_scale_and_rotation() {
    // Axis1 = world Y, so the map's X runs along Y and its Y along -X.
    let body = format!(
        "{UNIT_BOX_MAP}
         #74=IFCDIRECTION((0.,1.,0.));
         #75=IFCDIRECTION((-1.,0.,0.));
         #76=IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM(#74,#75,#1,4.,$,2.,3.);
         #13=IFCMAPPEDITEM(#70,#76);
         {}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [-1.0, -2.0, 0.0]);
    close3(e.max, [1.0, 2.0, 3.0]);
    close(e.yaw_rad, FRAC_PI_2);
    close3(e.size, [4.0, 2.0, 3.0]);
}

#[test]
fn mapping_origin_is_applied_before_the_target() {
    // IfcOpenShell's order: target * origin with no inverse, so a map origin of
    // (1,0,0) moves the geometry by +1 and a target at (0,5,0) by +5 in y.
    let body = format!(
        "#71=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);
         #72=IFCEXTRUDEDAREASOLID(#71,#4,#2,1.);
         #73=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#72));
         #77=IFCCARTESIANPOINT((1.,0.,0.));#78=IFCAXIS2PLACEMENT3D(#77,$,$);
         #70=IFCREPRESENTATIONMAP(#78,#73);
         #79=IFCCARTESIANPOINT((0.,5.,0.));
         #80=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#79,$,$);
         #13=IFCMAPPEDITEM(#70,#80);
         {}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    close3(only(&b).min, [0.5, 4.5, 0.0]);
}

#[test]
fn two_instances_of_a_map_land_in_different_places() {
    let body = format!(
        "{UNIT_BOX_MAP}
         #75=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#1,$,$);
         #13=IFCMAPPEDITEM(#70,#75);
         {}{}
         #30=IFCWINDOW('a',$,'A',$,$,#5,#20,$,$,$,$);
         #31=IFCWINDOW('b',$,'B',$,$,#40,#20,$,$,$,$);",
        body_shape("#13"),
        placement_at([100.0, 0.0, 0.0], 0.0, "$"),
    );
    let b = load(&body);
    assert_eq!(b.elements.len(), 2, "{:?}", b.skipped);
    close3(b.elements[0].max, [0.5, 0.5, 1.0]);
    close3(b.elements[1].max, [100.5, 0.5, 1.0]);
}

#[test]
fn nested_mapped_items_compose() {
    let body = format!(
        "{UNIT_BOX_MAP}
         #74=IFCCARTESIANPOINT((10.,0.,0.));
         #75=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#74,$,$);
         #76=IFCMAPPEDITEM(#70,#75);
         #77=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#76));
         #78=IFCREPRESENTATIONMAP(#4,#77);
         #79=IFCCARTESIANPOINT((0.,20.,0.));
         #80=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#79,$,$);
         #13=IFCMAPPEDITEM(#78,#80);
         {}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    close3(only(&load(&body)).min, [9.5, 19.5, 0.0]);
}

// ------------------------------------------------------------- fallbacks

#[test]
fn bounding_box_only_is_not_exact() {
    let body = format!(
        "#13=IFCBOUNDINGBOX(#1,5.,0.2,3.);
         #19=IFCSHAPEREPRESENTATION(#6,'Box','BoundingBox',(#13));
         #20=IFCPRODUCTDEFINITIONSHAPE($,$,(#19));
         {}",
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.max, [5.0, 0.2, 3.0]);
    close(e.thickness_m.unwrap(), 0.2);
    assert!(!e.exact);
}

#[test]
fn unsupported_body_falls_back_to_box_and_notes_it() {
    let body = format!(
        "#13=IFCBOUNDINGBOX(#1,5.,0.2,3.);
         #14=IFCSECTIONEDSPINE($,$,$);
         #18=IFCSHAPEREPRESENTATION(#6,'Body','SectionedSpine',(#14));
         #19=IFCSHAPEREPRESENTATION(#6,'Box','BoundingBox',(#13));
         #20=IFCPRODUCTDEFINITIONSHAPE($,$,(#19,#18));
         {}",
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.max, [5.0, 0.2, 3.0]);
    assert!(!e.exact);
    assert_eq!(b.skipped.len(), 1, "{:?}", b.skipped);
    assert!(b.skipped[0].1.contains("partial: unsupported representation item IFCSECTIONEDSPINE"), "{:?}", b.skipped);
}

#[test]
fn product_representation_supertype_is_accepted() {
    // AutoCAD Architecture 2010 writes this instead of IFCPRODUCTDEFINITIONSHAPE.
    let body = format!(
        "{}#19=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#13));
         #20=IFCPRODUCTREPRESENTATION($,$,(#19));{}",
        wall_solid(5.0, 0.2, 3.0),
        wall_entity("#5")
    );
    close3(only(&load(&body)).max, [5.0, 0.1, 3.0]);
}

#[test]
fn body_is_preferred_over_box_regardless_of_order() {
    let body = format!(
        "{}#14=IFCBOUNDINGBOX(#1,50.,50.,50.);
         #18=IFCSHAPEREPRESENTATION(#6,'Box','BoundingBox',(#14));
         #19=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#13));
         #20=IFCPRODUCTDEFINITIONSHAPE($,$,(#18,#19));
         {}",
        wall_solid(5.0, 0.2, 3.0),
        wall_entity("#5")
    );
    let b = load(&body);
    close3(only(&b).max, [5.0, 0.1, 3.0]);
    assert!(only(&b).exact);
}

#[test]
fn faceted_brep_gives_a_true_box_but_is_not_exact() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((1.,2.,3.));#51=IFCCARTESIANPOINT((4.,2.,3.));#52=IFCCARTESIANPOINT((4.,5.,3.));#53=IFCCARTESIANPOINT((1.,5.,9.));
         #54=IFCPOLYLOOP((#50,#51,#52,#53));
         #55=IFCFACEOUTERBOUND(#54,.T.);
         #56=IFCFACE((#55));
         #57=IFCCLOSEDSHELL((#56));
         #13=IFCFACETEDBREP(#57);{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.min, [1.0, 2.0, 3.0]);
    close3(e.max, [4.0, 5.0, 9.0]);
    assert!(!e.exact);
}

#[test]
fn surface_models_and_edge_loops_read_their_vertices() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((1.,2.,3.));#51=IFCCARTESIANPOINT((4.,5.,6.));
         #52=IFCVERTEXPOINT(#50);#53=IFCVERTEXPOINT(#51);
         #54=IFCEDGECURVE(#52,#53,$,.T.);
         #55=IFCORIENTEDEDGE(*,*,#54,.T.);
         #56=IFCEDGELOOP((#55));
         #57=IFCFACEBOUND(#56,.T.);
         #58=IFCADVANCEDFACE((#57),$,.T.);
         #59=IFCOPENSHELL((#58));
         #13=IFCSHELLBASEDSURFACEMODEL((#59));{}{}",
        body_shape("#13"),
        wall_entity("#5")
    );
    close3(only(&load(&body)).max, [4.0, 5.0, 6.0]);
}

#[test]
fn triangulated_and_polygonal_face_sets_read_their_points() {
    for face_set in [
        "IFCTRIANGULATEDFACESET(#50,$,.T.,((1,2,3)),$)",
        "IFCPOLYGONALFACESET(#50,.T.,(#51),$)",
    ] {
        let body = format!(
            "#50=IFCCARTESIANPOINTLIST3D(((0.,1.,2.),(3.,1.,2.),(3.,4.,7.)),$);
             #51=IFCINDEXEDPOLYGONALFACE((1,2,3));
             #13={face_set};{}{}",
            body_shape("#13"),
            wall_entity("#5")
        );
        let b = load(&body);
        let e = only(&b);
        close3(e.min, [0.0, 1.0, 2.0]);
        close3(e.max, [3.0, 4.0, 7.0]);
        assert!(!e.exact, "{face_set}");
    }
}

#[test]
fn boolean_clipping_uses_the_first_operand_and_is_not_exact() {
    let body = format!(
        "{}#50=IFCPLANE(#4);#51=IFCHALFSPACESOLID(#50,.F.);
         #52=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#13,#51);
         #53=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#52,#51);{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#53"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.max, [5.0, 0.1, 3.0]);
    close(e.thickness_m.unwrap(), 0.2);
    assert!(!e.exact);
    assert!(b.skipped.is_empty());
}

#[test]
fn boolean_union_uses_both_operands() {
    let body = format!(
        "{}#50=IFCBLOCK(#4,9.,1.,1.);#53=IFCBOOLEANRESULT(.UNION.,#13,#50);{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#53"),
        wall_entity("#5")
    );
    close3(only(&load(&body)).max, [9.0, 1.0, 3.0]);
}

#[test]
fn representation_with_only_an_axis_is_skipped_with_a_reason() {
    let body = format!(
        "#50=IFCPOLYLINE((#1,#1));
         #19=IFCSHAPEREPRESENTATION(#6,'Axis','Curve2D',(#50));
         #20=IFCPRODUCTDEFINITIONSHAPE($,$,(#19));
         {}",
        wall_entity("#5")
    );
    let b = load(&body);
    assert!(b.elements.is_empty());
    assert_eq!(b.skipped.len(), 1);
    assert_eq!(b.skipped[0].0, "IFCWALL");
    assert!(b.skipped[0].1.contains("no 3D body, only Axis/Curve2D"), "{:?}", b.skipped);
}

#[test]
fn partly_readable_body_keeps_what_it_can_and_notes_the_rest() {
    let body = format!(
        "{}#14=IFCSOMETHINGNEW($);{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13,#14"),
        wall_entity("#5")
    );
    let b = load(&body);
    let e = only(&b);
    assert!(!e.exact);
    close3(e.max, [5.0, 0.1, 3.0]);
    assert!(b.skipped[0].1.contains("partial: unsupported representation item IFCSOMETHINGNEW"));
    assert_eq!(b.skipped[0].0, "IFCWALL");
}

// -------------------------------------------------------------- entities

#[test]
fn entity_types_map_to_kinds() {
    use ElementKind::*;
    let cases = [
        ("IFCWALL", Wall),
        ("IFCWALLSTANDARDCASE", Wall),
        ("IFCWALLELEMENTEDCASE", Wall),
        ("IFCSLAB", Slab),
        ("IFCSLABSTANDARDCASE", Slab),
        ("IFCROOF", Roof),
        ("IFCCOLUMN", Column),
        ("IFCCOLUMNSTANDARDCASE", Column),
        ("IFCBEAM", Beam),
        ("IFCBEAMSTANDARDCASE", Beam),
        ("IFCDOOR", Door),
        ("IFCDOORSTANDARDCASE", Door),
        ("IFCWINDOW", Window),
        ("IFCWINDOWSTANDARDCASE", Window),
        ("IFCCOVERING", Covering),
        ("IFCCURTAINWALL", Window),
        ("IFCPLATE", Wall),
        ("IFCMEMBER", Other),
        ("IFCBUILDINGELEMENTPROXY", Other),
        ("IFCSTAIRFLIGHT", Other),
        ("IFCFOOTING", Other),
    ];
    for (entity, kind) in cases {
        let body = format!(
            "{}{}#30={entity}('g',$,'N',$,$,#5,#20,$,$);",
            wall_solid(5.0, 0.2, 3.0),
            body_shape("#13")
        );
        assert_eq!(only(&load(&body)).kind, kind, "{entity}");
    }
}

#[test]
fn furniture_and_spaces_are_ignored_without_comment() {
    let body = format!(
        "{}{}#30=IFCFURNISHINGELEMENT('g',$,'Chair',$,$,#5,#20,$);#31=IFCSPACE('s',$,'Room',$,$,#5,#20,$,$,$,$);",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    assert!(b.elements.is_empty());
    assert!(b.skipped.is_empty());
}

#[test]
fn plate_is_a_window_when_it_is_glazing() {
    let panel = |extra: &str, plate: &str| {
        format!("{}{}{extra}{plate}", wall_solid(1.0, 0.02, 2.0), body_shape("#13"))
    };
    // By predefined type.
    let b = load(&panel("", "#30=IFCPLATE('g',$,'Panel',$,$,#5,#20,$,.CURTAIN_PANEL.);"));
    assert_eq!(only(&b).kind, ElementKind::Window);
    // By name.
    let b = load(&panel("", "#30=IFCPLATE('g',$,'Glazed Panel',$,$,#5,#20,$,$);"));
    assert_eq!(only(&b).kind, ElementKind::Window);
    // By material.
    let b = load(&panel(
        "#60=IFCMATERIAL('Clear Glass');#61=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#60);",
        "#30=IFCPLATE('g',$,'Panel',$,$,#5,#20,$,$);",
    ));
    assert_eq!(only(&b).kind, ElementKind::Window);
    // By its parent, a curtain wall.
    let b = load(&panel(
        "#60=IFCCURTAINWALL('c',$,'CW',$,$,#5,$,$,$);#61=IFCRELAGGREGATES('r',$,$,$,#60,(#30));",
        "#30=IFCPLATE('g',$,'Panel',$,$,#5,#20,$,$);",
    ));
    assert_eq!(b.elements.len(), 1);
    assert_eq!(b.elements[0].kind, ElementKind::Window);
    // A steel plate is none of those.
    let b = load(&panel("", "#30=IFCPLATE('g',$,'Steel Sheet',$,$,#5,#20,$,$);"));
    assert_eq!(only(&b).kind, ElementKind::Wall);
}

#[test]
fn roof_slabs_load_as_roof() {
    let shape = format!("{}{}", wall_solid(5.0, 2.0, 0.2), body_shape("#13"));
    let b = load(&format!("{shape}#30=IFCSLAB('g',$,'S',$,$,#5,#20,$,.ROOF.);"));
    assert_eq!(only(&b).kind, ElementKind::Roof);
    let b = load(&format!(
        "{shape}#30=IFCSLAB('g',$,'S',$,$,#5,#20,$,.FLOOR.);
         #31=IFCROOF('r',$,'R',$,$,#5,$,$,$);
         #32=IFCRELAGGREGATES('x',$,$,$,#31,(#30));"
    ));
    assert_eq!(b.elements.len(), 1);
    assert_eq!(b.elements[0].kind, ElementKind::Roof);
    // The empty IFCROOF is reported, with its reason.
    assert!(b.skipped.iter().any(|(ty, reason)| ty == "IFCROOF" && reason.contains("aggregated parts")), "{:?}", b.skipped);
}

#[test]
fn parent_covered_by_its_parts_is_replaced_by_them() {
    let body = format!(
        "{}{}
         #31=IFCWALL('whole',$,'Whole',$,$,#5,#20,$,$);
         #32=IFCBUILDINGELEMENTPART('part',$,'Part',$,$,#5,#20,$,$);
         #33=IFCRELAGGREGATES('x',$,$,$,#31,(#32));",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.id, "part");
    assert_eq!(e.kind, ElementKind::Wall, "a part takes its parent's kind");
    assert!(b.skipped.iter().any(|(ty, reason)| ty == "IFCWALL" && reason.contains("replaced by")), "{:?}", b.skipped);
}

#[test]
fn parent_not_covered_by_its_parts_stays() {
    let body = format!(
        "{}{}
         #50=IFCBLOCK(#4,1.,0.2,1.);
         #51=IFCSHAPEREPRESENTATION(#6,'Body','CSG',(#50));
         #52=IFCPRODUCTDEFINITIONSHAPE($,$,(#51));
         #31=IFCWALL('whole',$,'Whole',$,$,#5,#20,$,$);
         #32=IFCBUILDINGELEMENTPART('part',$,'Part',$,$,#5,#52,$,$);
         #33=IFCRELAGGREGATES('x',$,$,$,#31,(#32));",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    assert_eq!(load(&body).elements.len(), 2);
}

#[test]
fn element_without_representation_is_skipped_with_a_reason() {
    let b = load("#30=IFCWALL('g',$,'Empty',$,$,#5,$,$,$);");
    assert!(b.elements.is_empty());
    assert_eq!(b.skipped, vec![("IFCWALL".to_string(), "#30 'Empty': no representation".to_string())]);
}

#[test]
fn slab_thickness_comes_from_its_extrusion() {
    let body = format!(
        "#12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,8.,6.);#13=IFCEXTRUDEDAREASOLID(#12,#4,#2,0.25);{}
         #30=IFCSLAB('g',$,'S',$,$,#5,#20,$,.FLOOR.);",
        body_shape("#13")
    );
    let b = load(&body);
    close(only(&b).thickness_m.unwrap(), 0.25);
    close3(only(&b).size, [8.0, 6.0, 0.25]);
}

#[test]
fn faceted_pitched_roof_has_no_thickness_rather_than_its_rise() {
    // A ridge roof as facets: 20 by 10 in plan, rising 5. The rise is not a thickness.
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.,0.));#51=IFCCARTESIANPOINT((20.,0.,0.));
         #52=IFCCARTESIANPOINT((20.,10.,0.));#53=IFCCARTESIANPOINT((0.,10.,0.));#54=IFCCARTESIANPOINT((10.,5.,5.));
         #55=IFCPOLYLOOP((#50,#51,#54));#56=IFCPOLYLOOP((#51,#52,#54));#57=IFCPOLYLOOP((#52,#53,#54));
         #58=IFCFACEOUTERBOUND(#55,.T.);#59=IFCFACEOUTERBOUND(#56,.T.);#60=IFCFACEOUTERBOUND(#57,.T.);
         #61=IFCFACE((#58));#62=IFCFACE((#59));#63=IFCFACE((#60));
         #64=IFCCLOSEDSHELL((#61,#62,#63));#13=IFCFACETEDBREP(#64);{}
         #30=IFCROOF('r',$,'Roof',$,$,#5,#20,$,$);",
        body_shape("#13")
    );
    let b = load(&body);
    let e = only(&b);
    assert_eq!(e.thickness_m, None);
    close3(e.size, [20.0, 10.0, 5.0]);
}

#[test]
fn faceted_flat_slab_uses_its_vertical_extent() {
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.,0.));#51=IFCCARTESIANPOINT((8.,0.,0.));#52=IFCCARTESIANPOINT((8.,5.,0.));#53=IFCCARTESIANPOINT((0.,0.,0.3));
         #55=IFCPOLYLOOP((#50,#51,#52,#53));#58=IFCFACEOUTERBOUND(#55,.T.);#61=IFCFACE((#58));
         #64=IFCCLOSEDSHELL((#61));#13=IFCFACETEDBREP(#64);{}
         #30=IFCSLAB('s',$,'Floor',$,$,#5,#20,$,.FLOOR.);",
        body_shape("#13")
    );
    close(only(&load(&body)).thickness_m.unwrap(), 0.3);
}

#[test]
fn roof_swept_along_its_ridge_does_not_report_the_ridge_as_thickness() {
    // A pitched cross-section in the YZ plane, extruded 19 along X. Position Z = world X,
    // position X = world Y, position Y = world Z.
    let body = format!(
        "#50=IFCCARTESIANPOINT((0.,0.));#51=IFCCARTESIANPOINT((6.,3.));#52=IFCCARTESIANPOINT((6.,3.2));#53=IFCCARTESIANPOINT((0.,0.2));
         #54=IFCPOLYLINE((#50,#51,#52,#53,#50));
         #12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#54);
         #60=IFCDIRECTION((0.,1.,0.));
         #62=IFCAXIS2PLACEMENT3D(#1,#3,#60);
         #13=IFCEXTRUDEDAREASOLID(#12,#62,#2,19.);{}
         #30=IFCSLAB('s',$,'Roof',$,$,#5,#20,$,.ROOF.);",
        body_shape("#13")
    );
    let b = load(&body);
    let e = only(&b);
    close3(e.size, [19.0, 6.0, 3.2]);
    // The rise, 3.2, is over 15 % of 19.
    assert_eq!(e.thickness_m, None, "{e:?}");
}

#[test]
fn covering_thickness_is_its_smallest_extent() {
    let ceiling = format!(
        "#12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,8.,6.);#13=IFCEXTRUDEDAREASOLID(#12,#4,#2,0.02);{}
         #30=IFCCOVERING('g',$,'C',$,$,#5,#20,$,.CEILING.);",
        body_shape("#13")
    );
    close(only(&load(&ceiling)).thickness_m.unwrap(), 0.02);
    let cladding = format!(
        "#12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,8.,0.03);#13=IFCEXTRUDEDAREASOLID(#12,#4,#2,3.);{}
         #30=IFCCOVERING('g',$,'C',$,$,#5,#20,$,.CLADDING.);",
        body_shape("#13")
    );
    close(only(&load(&cladding)).thickness_m.unwrap(), 0.03);
}

// ------------------------------------------------------------- materials

fn wall_with(material: &str) -> String {
    format!("{}{}{}{material}", wall_solid(5.0, 0.2, 3.0), body_shape("#13"), wall_entity("#5"))
}

#[test]
fn single_material() {
    let b = load(&wall_with("#60=IFCMATERIAL('Brick');#61=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#60);"));
    assert_eq!(only(&b).material.as_deref(), Some("Brick"));
}

#[test]
fn layer_set_usage_gives_names_and_thickness() {
    let b = load(&wall_with(
        "#60=IFCMATERIAL('Brick');#61=IFCMATERIAL('Insulation');
         #62=IFCMATERIALLAYER(#60,0.11,$);#63=IFCMATERIALLAYER(#61,0.05,$);#64=IFCMATERIALLAYER(#60,0.02,$);
         #65=IFCMATERIALLAYER($,0.04,.T.);
         #66=IFCMATERIALLAYERSET((#62,#63,#64,#65),'Cavity');
         #67=IFCMATERIALLAYERSETUSAGE(#66,.AXIS2.,.POSITIVE.,0.);
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#67);",
    ));
    let e = only(&b);
    assert_eq!(e.material.as_deref(), Some("Brick; Insulation"), "distinct names; the air gap has none");
    close(e.thickness_m.unwrap(), 0.22);
    // The box still reports what the geometry says.
    close(e.size[1], 0.2);
}

#[test]
fn layer_set_directly() {
    let b = load(&wall_with(
        "#60=IFCMATERIAL('Concrete');#62=IFCMATERIALLAYER(#60,0.15,$);#66=IFCMATERIALLAYERSET((#62),$);
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#66);",
    ));
    assert_eq!(only(&b).material.as_deref(), Some("Concrete"));
    close(only(&b).thickness_m.unwrap(), 0.15);
}

#[test]
fn material_list_joins_names() {
    let b = load(&wall_with(
        "#60=IFCMATERIAL('Steel');#61=IFCMATERIAL('Concrete, Cast-in-Place');#62=IFCMATERIALLIST((#60,#61));
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#62);",
    ));
    assert_eq!(only(&b).material.as_deref(), Some("Steel; Concrete, Cast-in-Place"));
    close(only(&b).thickness_m.unwrap(), 0.2);
}

#[test]
fn ifc4_constituent_and_profile_sets() {
    let b = load(&wall_with(
        "#60=IFCMATERIAL('Glass');#61=IFCMATERIAL('Aluminium');
         #62=IFCMATERIALCONSTITUENT('Pane',$,#60,$,$);#63=IFCMATERIALCONSTITUENT('Frame',$,#61,$,$);
         #64=IFCMATERIALCONSTITUENTSET('Set',$,(#62,#63));
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#64);",
    ));
    assert_eq!(only(&b).material.as_deref(), Some("Glass; Aluminium"));
    let b = load(&wall_with(
        "#60=IFCMATERIAL('S355');#62=IFCMATERIALPROFILE($,$,#60,$,$,$);#63=IFCMATERIALPROFILESET($,$,(#62),$);
         #64=IFCMATERIALPROFILESETUSAGE(#63,$,$);
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#64);",
    ));
    assert_eq!(only(&b).material.as_deref(), Some("S355"));
}

#[test]
fn material_on_the_type_is_inherited() {
    let b = load(&wall_with(
        "#60=IFCMATERIAL('Timber');#62=IFCMATERIALLAYER(#60,0.1,$);#63=IFCMATERIALLAYERSET((#62),$);
         #64=IFCWALLTYPE('t',$,'T',$,$,$,$,$,$,.STANDARD.);
         #65=IFCRELDEFINESBYTYPE('d',$,$,$,(#30),#64);
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#64),#63);",
    ));
    assert_eq!(only(&b).material.as_deref(), Some("Timber"));
    close(only(&b).thickness_m.unwrap(), 0.1);
}

#[test]
fn material_from_a_parent_gives_the_name_but_not_its_thickness() {
    let body = format!(
        "{}{}
         #30=IFCPLATE('p',$,'Panel',$,$,#5,#20,$,$);
         #31=IFCWALL('w',$,'Whole',$,$,#5,$,$,$);
         #32=IFCRELAGGREGATES('x',$,$,$,#31,(#30));
         #60=IFCMATERIAL('Aluminium');#62=IFCMATERIALLAYER(#60,0.9,$);#63=IFCMATERIALLAYERSET((#62),$);
         #68=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#31),#63);",
        wall_solid(5.0, 0.02, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    let e = b.elements.iter().find(|e| e.id == "p").unwrap();
    assert_eq!(e.material.as_deref(), Some("Aluminium"));
    close(e.thickness_m.unwrap(), 0.02);
}

#[test]
fn no_material_is_none() {
    assert_eq!(only(&load(&wall_with(""))).material, None);
}

// --------------------------------------------------------------- storeys

fn storey_body(extra: &str) -> String {
    format!(
        "{}{}{}
         #80=IFCBUILDINGSTOREY('st',$,'Level 2',$,$,#5,$,$,.ELEMENT.,3.);{extra}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#5"),
    )
}

#[test]
fn storey_through_containment() {
    let b = load(&storey_body("#81=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(#30),#80);"));
    assert_eq!(only(&b).storey.as_deref(), Some("Level 2"));
}

#[test]
fn storey_through_a_space() {
    let b = load(&storey_body(
        "#82=IFCSPACE('sp',$,'Room',$,$,#5,$,$,$,$,$);
         #83=IFCRELAGGREGATES('a',$,$,$,#80,(#82));
         #81=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(#30),#82);",
    ));
    assert_eq!(only(&b).storey.as_deref(), Some("Level 2"));
}

#[test]
fn storey_inherited_from_an_aggregate_parent() {
    let body = format!(
        "{}{}
         #30=IFCPLATE('p',$,'Panel',$,$,#5,#20,$,$);
         #31=IFCCURTAINWALL('cw',$,'CW',$,$,#5,$,$,$);
         #32=IFCRELAGGREGATES('x',$,$,$,#31,(#30));
         #80=IFCBUILDINGSTOREY('st',$,'Ground',$,$,#5,$,$,.ELEMENT.,0.);
         #81=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(#31),#80);",
        wall_solid(1.0, 0.02, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    assert_eq!(b.elements[0].storey.as_deref(), Some("Ground"));
}

#[test]
fn door_finds_its_storey_through_the_wall_it_fills() {
    let body = format!(
        "{}{}{}
         #31=IFCDOOR('d',$,'Door',$,$,#5,#20,$,2.,1.);
         #32=IFCOPENINGELEMENT('o',$,$,$,$,#5,$,$);
         #33=IFCRELVOIDSELEMENT('v',$,$,$,#30,#32);
         #34=IFCRELFILLSELEMENT('f',$,$,$,#32,#31);
         #80=IFCBUILDINGSTOREY('st',$,'Basement',$,$,#5,$,$,.ELEMENT.,-3.);
         #81=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(#30),#80);",
        wall_solid(1.0, 0.1, 2.0),
        body_shape("#13"),
        wall_entity("#5"),
    );
    let b = load(&body);
    let door = b.elements.iter().find(|e| e.kind == ElementKind::Door).unwrap();
    assert_eq!(door.storey.as_deref(), Some("Basement"));
}

#[test]
fn storey_name_falls_back_to_long_name_then_id() {
    let body = |storey: &str| {
        format!(
            "{}{}{}{storey}#81=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(#30),#80);",
            wall_solid(5.0, 0.2, 3.0),
            body_shape("#13"),
            wall_entity("#5"),
        )
    };
    let b = load(&body("#80=IFCBUILDINGSTOREY('st',$,$,$,$,#5,$,'First Floor',.ELEMENT.,3.);"));
    assert_eq!(only(&b).storey.as_deref(), Some("First Floor"));
    let b = load(&body("#80=IFCBUILDINGSTOREY('st',$,$,$,$,#5,$,$,.ELEMENT.,3.);"));
    assert_eq!(only(&b).storey.as_deref(), Some("#80"));
}

#[test]
fn element_on_a_site_has_no_storey() {
    let b = load(&storey_body(
        "#82=IFCSITE('s',$,'Site',$,$,#5,$,$,.ELEMENT.,$,$,$,$,$);
         #81=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(#30),#82);",
    ));
    assert_eq!(only(&b).storey, None);
}

// ----------------------------------------------------------- the building

#[test]
fn building_name_prefers_the_building_then_the_project() {
    let b = load("#70=IFCBUILDING('b',$,'Tower A',$,$,#5,$,$,.ELEMENT.,$,$,$);");
    assert_eq!(b.name, "Tower A");
    let b = load("#70=IFCBUILDING('b',$,$,$,$,#5,$,$,.ELEMENT.,$,$,$);");
    assert_eq!(b.name, "Project");
}

#[test]
fn bounds_and_counts() {
    let body = format!(
        "{}{}{}
         #31=IFCSLAB('s',$,'S',$,$,#40,#20,$,.FLOOR.);
         #32=IFCWALL('w2',$,'W',$,$,#40,#20,$,$);",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#5"),
    ) + &placement_at([10.0, 10.0, 10.0], 0.0, "$");
    let b = load(&body);
    assert_eq!(b.counts(), vec![(ElementKind::Wall, 2), (ElementKind::Slab, 1)]);
    let (min, max) = b.bounds().unwrap();
    close3(min, [0.0, -0.1, 0.0]);
    close3(max, [15.0, 10.1, 13.0]);
    assert_eq!(load("").bounds(), None);
    assert!(load("").counts().is_empty());
}

#[test]
fn building_round_trips_through_json() {
    let b = load(&wall_with("#60=IFCMATERIAL('Brick');#61=IFCRELASSOCIATESMATERIAL('r',$,$,$,(#30),#60);"));
    let json = serde_json::to_string(&b).unwrap();
    assert_eq!(serde_json::from_str::<Building>(&json).unwrap(), b);
}

// ------------------------------------------------- strings and tokenising

#[test]
fn decodes_escaped_strings() {
    let cases = [
        ("plain", "plain"),
        ("it''s", "it's"),
        ("''", "'"),
        (r"a\\b", r"a\b"),
        (r"\X2\00E9\X0\", "\u{e9}"),
        (r"caf\X2\00E9\X0\ noir", "caf\u{e9} noir"),
        (r"\X2\00C400D6\X0\x", "\u{c4}\u{d6}x"),
        (r"\X2\65E5672C8A9E\X0\", "\u{65e5}\u{672c}\u{8a9e}"),
        (r"\X2\D83DDE00\X0\", "\u{1f600}"),
        (r"\X4\0001F600\X0\", "\u{1f600}"),
        (r"\X\E9t\X\E9", "\u{e9}t\u{e9}"),
        (r"\S\i", "\u{e9}"),
        (r"Stra\S\_e", "Stra\u{df}e"),
        (r"\PA\\S\i", "\u{e9}"),
        ("line\r\nwrap", "linewrap"),
        // Malformed escapes stay as written.
        (r"C:\Users\x", r"C:\Users\x"),
        (r"\X2\00E", r"\X2\00E"),
        (r"\X2\ZZZZ\X0\", r"\X2\ZZZZ\X0\"),
        ("trailing\\", "trailing\\"),
        ("\\", "\\"),
        // UTF-8 in the file passes through.
        ("caf\u{e9}", "caf\u{e9}"),
    ];
    for (raw, want) in cases {
        assert_eq!(decode_string(raw), want, "raw: {raw}");
    }
}

#[test]
fn escaped_names_reach_the_element() {
    let body = format!(
        "{}{}#30=IFCWALL('g',$,'Bj\\X2\\00F6\\X0\\rk''s \\S\\i wall; /* no comment */',$,$,#5,#20,$,$);",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    assert_eq!(only(&load(&body)).name, "Bj\u{f6}rk's \u{e9} wall; /* no comment */");
}

fn tokens(arguments: &str) -> Result<Vec<Value<'_>>, String> {
    // Leaked so that the returned values can borrow from the file.
    let text: &'static str = Box::leak(format!("DATA;#1=X{arguments};ENDSEC;").into_boxed_str());
    let step = Box::leak(Box::new(Step::parse(text).unwrap()));
    // A statement that never closes (a lost quote, say) is never indexed.
    step.args(step.rec(1).ok_or("statement was not indexed")?)
}

#[test]
fn tokenises_all_value_kinds() {
    let values = tokens("($,*,1,-2.5E1,.5,'s',.T.,#7,(1,(2,3)),IFCLENGTHMEASURE(0.2),\"0F\",1.,1.E3,())").unwrap();
    assert_eq!(values[0], Value::Null);
    assert_eq!(values[1], Value::Derived);
    assert_eq!(values[2], Value::Int(1));
    assert_eq!(values[3], Value::Real(-25.0));
    assert_eq!(values[4], Value::Real(0.5));
    assert_eq!(values[5], Value::Str("s"));
    assert_eq!(values[6], Value::Enum("T"));
    assert_eq!(values[7], Value::Ref(7));
    assert_eq!(
        values[8],
        Value::List(vec![Value::Int(1), Value::List(vec![Value::Int(2), Value::Int(3)])])
    );
    assert_eq!(values[9], Value::Typed("IFCLENGTHMEASURE", vec![Value::Real(0.2)]));
    assert_eq!(values[9].as_f64(), Some(0.2));
    assert_eq!(values[10], Value::Bin("0F"));
    assert_eq!(values[11], Value::Real(1.0));
    assert_eq!(values[12], Value::Real(1000.0));
    assert_eq!(values[13], Value::List(vec![]));
}

#[test]
fn tokeniser_rejects_malformed_lists_without_panicking() {
    for bad in ["(1 2)", "(1,", "(,)", "(1,,2)", "(#)", "(.T)", "('abc)", "(IFCX)", "(@)", "(-)", "(\"0F)", "(1,)"] {
        assert!(tokens(bad).is_err(), "{bad} should fail");
    }
}

#[test]
fn huge_numbers_are_absent_not_infinite() {
    let values = tokens("(1E999,-1E999,99999999999999999999)").unwrap();
    assert_eq!(values[0].as_f64(), None);
    assert_eq!(values[1].as_f64(), None);
    assert_eq!(values[2].as_f64(), Some(1e20));
}

#[test]
fn comments_whitespace_and_multi_line_entities() {
    let body = "
        /* a comment; with a semicolon and a 'quote */
        #10 = IFCRECTANGLEPROFILEDEF ( .AREA. , $ , $ ,
              5. , /* x */ 0.2
        ) ;
        #13
           =
           ifcextrudedareasolid(#10,#4,#2,
           3.);
        #19=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#13));
        #20=IFCPRODUCTDEFINITIONSHAPE($,$,(#19)); /* trailing */
        #30=IFCWALL('wall',$,'A name
 wrapped over lines',$,$,#5,#20,$,$);
    ";
    let b = load(body);
    let e = only(&b);
    close3(e.size, [5.0, 0.2, 3.0]);
    assert_eq!(e.name, "A name wrapped over lines");
}

#[test]
fn semicolons_and_quotes_inside_strings_do_not_end_statements() {
    let body = format!(
        "{}{}#30=IFCWALL('a;b''c;',$,'d;#99=IFCWALL();',$,$,#5,#20,$,$);",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    assert_eq!(b.elements.len(), 1);
    assert_eq!(b.elements[0].id, "a;b'c;");
}

#[test]
fn entity_ids_may_be_in_any_order() {
    let body = format!("{}{}{}", wall_entity("#5"), body_shape("#13"), wall_solid(5.0, 0.2, 3.0));
    close3(only(&load(&body)).max, [5.0, 0.1, 3.0]);
}

#[test]
fn duplicate_ids_keep_the_first_and_report_it() {
    let body = format!("{}{}{}#30=IFCSLAB('dup',$,'x',$,$,#5,#20,$,$);", wall_solid(5.0, 0.2, 3.0), body_shape("#13"), wall_entity("#5"));
    let b = load(&body);
    assert_eq!(only(&b).id, "wall-guid");
    assert!(b.skipped.iter().any(|(ty, reason)| ty == "STEP" && reason.contains("duplicate")));
}

#[test]
fn utf8_bom_is_accepted() {
    let text = format!("\u{feff}{}", file_with(METRE_UNITS, &simple_wall(5.0, 0.2, 3.0)));
    assert_eq!(read(&text).unwrap().elements.len(), 1);
}

#[test]
fn non_utf8_bytes_read_as_latin1() {
    let text = text_from_bytes(b"caf\xE9".to_vec()).unwrap();
    assert_eq!(text, "caf\u{e9}");
    assert_eq!(text_from_bytes(b"caf\xC3\xA9".to_vec()).unwrap(), "caf\u{e9}");
}

#[test]
fn zip_xml_and_utf16_are_refused_clearly() {
    assert!(text_from_bytes(b"PK\x03\x04rest".to_vec()).unwrap_err().to_string().contains("ifczip"));
    assert!(text_from_bytes(b"  <?xml version".to_vec()).unwrap_err().to_string().contains("ifcxml"));
    assert!(text_from_bytes(vec![0xFF, 0xFE, b'I', 0]).unwrap_err().to_string().contains("UTF-16"));
}

#[test]
fn read_file_reports_a_missing_path() {
    let error = read_file(Path::new("definitely/not/here.ifc")).unwrap_err();
    assert!(format!("{error:#}").contains("definitely"));
}

// ---------------------------------------------------------- malformed input

#[test]
fn text_that_is_not_ifc_is_an_error() {
    assert!(read("").is_err());
    assert!(read("hello world").is_err());
    assert!(read("ISO-10303-21;\nHEADER;\nENDSEC;\nEND-ISO-10303-21;").is_err());
    let cad = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\nENDSEC;";
    assert!(read(cad).unwrap_err().to_string().contains("AUTOMOTIVE_DESIGN"));
}

#[test]
fn an_empty_data_section_is_an_empty_building() {
    let b = read("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;").unwrap();
    assert!(b.elements.is_empty());
    close(b.unit_to_m, 1.0);
}

#[test]
fn file_schema_is_case_insensitive_and_future_schemas_load() {
    for schema in ["ifc4", "IFC4X3_ADD2", "IFC2X3"] {
        let text = format!("HEADER;FILE_SCHEMA(('{schema}'));ENDSEC;DATA;ENDSEC;");
        assert!(read(&text).is_ok(), "{schema}");
    }
}

/// One good wall `#31`, with `bad` alongside; the good wall must always survive.
#[track_caller]
fn good_wall_survives(bad: &str) -> Building {
    let body = format!(
        "{}{}#31=IFCWALL('good',$,'Good',$,$,#5,#20,$,$);\n{bad}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    assert!(b.elements.iter().any(|e| e.id == "good"), "lost the good wall to {bad}: {:?}", b.skipped);
    b
}

#[track_caller]
fn skipped_with(b: &Building, needle: &str) {
    assert!(b.skipped.iter().any(|(_, reason)| reason.contains(needle)), "no '{needle}' in {:?}", b.skipped);
}

#[test]
fn unterminated_string_costs_one_entity() {
    let b = good_wall_survives("#30=IFCWALL('oops,$,'x',$,$,#5,#20,$,$);\n#32=IFCWALL('next',$,'Next',$,$,#5,#20,$,$);");
    assert!(b.elements.iter().any(|e| e.id == "next"), "{:?}", b.skipped);
}

#[test]
fn unterminated_string_at_line_end_resynchronises() {
    let b = good_wall_survives("#30=IFCWALL('oops,$,$,$,$,#5,#20,$,$);\n#32=IFCWALL('next',$,'Next',$,$,#5,#20,$,$);");
    assert!(b.elements.iter().any(|e| e.id == "next"));
    skipped_with(&b, "unterminated string");
}

#[test]
fn missing_semicolon_costs_one_entity() {
    let b = good_wall_survives("#30=IFCWALL('oops',$,$,$,$,#5,#20,$,$)\n#32=IFCWALL('next',$,'Next',$,$,#5,#20,$,$);");
    assert!(b.elements.iter().any(|e| e.id == "next"));
    skipped_with(&b, "missing ';'");
}

#[test]
fn entity_with_bad_syntax_is_skipped() {
    let b = good_wall_survives("#30=IFCWALL('bad',$,$,$,$,#5,#20 #21,$,$);");
    skipped_with(&b, "#30 '': malformed entity");
}

#[test]
fn entity_without_arguments_is_noted() {
    let b = good_wall_survives("#30=IFCWALL;");
    skipped_with(&b, "expected '('");
}

#[test]
fn garbage_statements_are_noted() {
    let b = good_wall_survives("#abc=IFCWALL();\n@@@ what;\n");
    skipped_with(&b, "not a valid entity");
}

#[test]
fn oversized_entity_id_is_noted() {
    let b = good_wall_survives("#99999999999999999999999999=IFCWALL('x',$,$,$,$,#5,#20,$,$);");
    skipped_with(&b, "not a valid entity");
}

#[test]
fn stray_comment_opener_does_not_swallow_the_rest_of_the_file() {
    // No `*/` anywhere after it, so the good wall below it must still load.
    let body = format!(
        "#30=IFCWALL('bad',$,$,$,$,#5,#20,$,$);\n/* lost\n{}{}#31=IFCWALL('good',$,'Good',$,$,#5,#20,$,$);\n",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13")
    );
    let b = load(&body);
    assert!(b.elements.iter().any(|e| e.id == "good"), "{:?}", b.skipped);
    skipped_with(&b, "missing ';'");
    // Inside an entity it costs that entity only.
    let b = good_wall_survives("#30=IFCWALL('oops',$,/* lost ,$,$,#5,#20,$,$);\n#32=IFCWALL('next',$,'Next',$,$,#5,#20,$,$);");
    assert!(b.elements.iter().any(|e| e.id == "next"), "{:?}", b.skipped);
    assert!(b.elements.iter().all(|e| e.id != "oops"));
}

#[test]
fn closed_comments_can_contain_anything() {
    let body = format!(
        "/* #30=IFCWALL('ghost',$,$,$,$,#5,#20,$,$); 'quote */\n{}{}{}",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#5")
    );
    assert_eq!(only(&load(&body)).id, "wall-guid");
}

#[test]
fn dangling_references_are_skipped_with_reasons() {
    skipped_with(&good_wall_survives("#30=IFCWALL('x',$,'x',$,$,#777,#20,$,$);"), "dangling reference #777");
    skipped_with(&good_wall_survives("#30=IFCWALL('x',$,'x',$,$,#5,#777,$,$);"), "dangling reference #777");
    let b = good_wall_survives(
        "#21=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#777));
         #22=IFCPRODUCTDEFINITIONSHAPE($,$,(#21));
         #30=IFCWALL('x',$,'x',$,$,#5,#22,$,$);",
    );
    skipped_with(&b, "dangling reference #777");
    let b = good_wall_survives(
        "#21=IFCPRODUCTDEFINITIONSHAPE($,$,(#777));
         #30=IFCWALL('x',$,'x',$,$,#5,#21,$,$);",
    );
    skipped_with(&b, "dangling reference #777");
}

#[test]
fn references_to_the_wrong_kind_of_entity_are_skipped() {
    skipped_with(&good_wall_survives("#30=IFCWALL('x',$,'x',$,$,#1,#20,$,$);"), "unsupported object placement IFCCARTESIANPOINT");
    skipped_with(&good_wall_survives("#30=IFCWALL('x',$,'x',$,$,#5,#1,$,$);"), "representation #1 is IFCCARTESIANPOINT");
    let b = good_wall_survives(
        "#21=IFCEXTRUDEDAREASOLID(#1,#4,#2,1.);
         #22=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#21));
         #23=IFCPRODUCTDEFINITIONSHAPE($,$,(#22));
         #30=IFCWALL('x',$,'x',$,$,#5,#23,$,$);",
    );
    skipped_with(&b, "unsupported profile IFCCARTESIANPOINT");
    skipped_with(&good_wall_survives("#30=IFCWALL('x',$,'x',$,$,'string',#20,$,$);"), "object placement is not an entity reference");
    skipped_with(&good_wall_survives("#30=IFCWALL('x',$,'x',$,$,#5,'string',$,$);"), "representation is not an entity reference");
}

#[test]
fn unsupported_placement_and_geometry_are_named() {
    let b = good_wall_survives(
        "#21=IFCGRIDPLACEMENT(#4,$);
         #30=IFCWALL('x',$,'x',$,$,#21,#20,$,$);",
    );
    skipped_with(&b, "unsupported object placement IFCGRIDPLACEMENT");
    let b = good_wall_survives(
        "#21=IFCEXTRUDEDAREASOLID(#22,#4,#2,1.);#22=IFCCENTERLINEPROFILEDEF(.AREA.,$,#1,0.1);
         #23=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#21));
         #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
         #30=IFCWALL('x',$,'x',$,$,#5,#24,$,$);",
    );
    skipped_with(&b, "unsupported profile IFCCENTERLINEPROFILEDEF");
}

#[test]
fn bad_numbers_are_rejected_not_propagated() {
    for dims in ["-1.,1.", "1E999,1.", "$,1.", "'a',1."] {
        let b = good_wall_survives(&format!(
            "#21=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,{dims});
             #22=IFCEXTRUDEDAREASOLID(#21,#4,#2,1.);
             #23=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#22));
             #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
             #30=IFCWALL('x',$,'x',$,$,#5,#24,$,$);"
        ));
        assert!(b.elements.iter().all(|e| e.id != "x"), "{dims}");
        skipped_with(&b, "XDim");
    }
    let b = good_wall_survives(
        "#21=IFCCARTESIANPOINT((1E999,0.,0.));
         #22=IFCAXIS2PLACEMENT3D(#21,$,$);
         #23=IFCLOCALPLACEMENT($,#22);
         #30=IFCWALL('x',$,'x',$,$,#23,#20,$,$);",
    );
    skipped_with(&b, "bad coordinates");
    // Finite in the file, but overflowing once multiplied out.
    let b = good_wall_survives(
        "#21=IFCCARTESIANPOINT((1E308,0.,0.));
         #22=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#21,1E308,$);
         #23=IFCMAPPEDITEM(#24,#22);
         #24=IFCREPRESENTATIONMAP(#4,#19);
         #25=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#23));
         #26=IFCPRODUCTDEFINITIONSHAPE($,$,(#25));
         #30=IFCWALL('x',$,'x',$,$,#5,#26,$,$);",
    );
    skipped_with(&b, "not finite");
}

#[test]
fn zero_length_directions_fall_back_or_are_rejected() {
    let b = good_wall_survives(
        "#21=IFCDIRECTION((0.,0.,0.));
         #22=IFCAXIS2PLACEMENT3D(#1,#21,#21);
         #23=IFCLOCALPLACEMENT($,#22);
         #30=IFCWALL('x',$,'x',$,$,#23,#20,$,$);",
    );
    assert!(b.elements.iter().any(|e| e.id == "x"), "a zero axis uses the default: {:?}", b.skipped);
    let b = good_wall_survives(
        "#21=IFCDIRECTION((0.,0.,0.));
         #22=IFCEXTRUDEDAREASOLID(#12,#4,#21,1.);
         #23=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#22));
         #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
         #30=IFCWALL('x',$,'x',$,$,#5,#24,$,$);",
    );
    skipped_with(&b, "extrusion direction");
}

#[test]
fn cycles_terminate_with_a_reason() {
    let b = good_wall_survives(
        "#21=IFCLOCALPLACEMENT(#22,#4);#22=IFCLOCALPLACEMENT(#21,#4);
         #30=IFCWALL('x',$,'x',$,$,#21,#20,$,$);",
    );
    skipped_with(&b, "cyclic");

    let b = good_wall_survives(
        "#21=IFCLOCALPLACEMENT(#21,#4);
         #30=IFCWALL('x',$,'x',$,$,#21,#20,$,$);",
    );
    skipped_with(&b, "cyclic");

    let b = good_wall_survives(
        "#21=IFCMAPPEDITEM(#22,$);
         #22=IFCREPRESENTATIONMAP(#4,#23);
         #23=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#21));
         #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
         #30=IFCWALL('x',$,'x',$,$,#5,#24,$,$);",
    );
    skipped_with(&b, "contains itself");

    let b = good_wall_survives(
        "#21=IFCBOOLEANRESULT(.DIFFERENCE.,#21,#21);
         #22=IFCSHAPEREPRESENTATION(#6,'Body','Clipping',(#21));
         #23=IFCPRODUCTDEFINITIONSHAPE($,$,(#22));
         #30=IFCWALL('x',$,'x',$,$,#5,#23,$,$);",
    );
    skipped_with(&b, "boolean chain");

    let b = good_wall_survives(
        "#21=IFCBOOLEANRESULT(.UNION.,#22,#22);#22=IFCBOOLEANRESULT(.UNION.,#21,#21);
         #23=IFCSHAPEREPRESENTATION(#6,'Body','Clipping',(#21));
         #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
         #30=IFCWALL('x',$,'x',$,$,#5,#24,$,$);",
    );
    assert!(b.elements.iter().all(|e| e.id != "x"));

    let b = good_wall_survives(
        "#21=IFCCSGSOLID(#21);
         #22=IFCSHAPEREPRESENTATION(#6,'Body','CSG',(#21));
         #23=IFCPRODUCTDEFINITIONSHAPE($,$,(#22));
         #30=IFCWALL('x',$,'x',$,$,#5,#23,$,$);",
    );
    skipped_with(&b, "nested deeper");

    let b = good_wall_survives(
        "#21=IFCDERIVEDPROFILEDEF(.AREA.,$,#21,$,$);
         #22=IFCEXTRUDEDAREASOLID(#21,#4,#2,1.);
         #23=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#22));
         #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
         #30=IFCWALL('x',$,'x',$,$,#5,#24,$,$);",
    );
    skipped_with(&b, "nested deeper");

    let b = good_wall_survives(
        "#21=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#22);#22=IFCCOMPOSITECURVE((#21),.F.);
         #23=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#22);
         #24=IFCEXTRUDEDAREASOLID(#23,#4,#2,1.);
         #25=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#24));
         #26=IFCPRODUCTDEFINITIONSHAPE($,$,(#25));
         #30=IFCWALL('x',$,'x',$,$,#5,#26,$,$);",
    );
    skipped_with(&b, "nested deeper");
}

#[test]
fn relationship_cycles_terminate() {
    // Spatial parents, materials and a unit that all refer to themselves.
    let units = "#9001=IFCUNITASSIGNMENT((#9002));
        #9002=IFCCONVERSIONBASEDUNIT(*,.LENGTHUNIT.,'BOGUS',#9004);
        #9004=IFCMEASUREWITHUNIT(IFCRATIOMEASURE(2.),#9002);";
    let body = format!(
        "{}{}{}
         #60=IFCSPACE('a',$,$,$,$,$,$,$,$,$,$);
         #61=IFCSPACE('b',$,$,$,$,$,$,$,$,$,$);
         #62=IFCRELAGGREGATES('x',$,$,$,#60,(#61));
         #63=IFCRELAGGREGATES('y',$,$,$,#61,(#60));
         #64=IFCRELCONTAINEDINSPATIALSTRUCTURE('z',$,$,$,(#30),#60);
         #65=IFCMATERIALLIST((#65));
         #66=IFCRELASSOCIATESMATERIAL('m',$,$,$,(#30),#65);
         #67=IFCRELAGGREGATES('p',$,$,$,#30,(#31));
         #68=IFCRELAGGREGATES('q',$,$,$,#31,(#30));
         #31=IFCWALL('w2',$,$,$,$,#5,#20,$,$);",
        wall_solid(5.0, 0.2, 3.0),
        body_shape("#13"),
        wall_entity("#5"),
    );
    let b = load_with(units, &body);
    assert!(b.skipped.iter().any(|(ty, _)| ty == "IFCUNITASSIGNMENT" || ty == "IFCWALL"), "{:?}", b.skipped);
}

#[test]
fn deep_nesting_cannot_overflow_the_stack() {
    let opening = "(".repeat(1_000_000);
    let closing = ")".repeat(1_000_000);
    let b = good_wall_survives(&format!("#30=IFCWALL({opening}{closing});"));
    skipped_with(&b, "nested deeper");

    let typed = "A(".repeat(200_000);
    let b = good_wall_survives(&format!("#30=IFCWALL({typed}{});", ")".repeat(200_000)));
    skipped_with(&b, "nested deeper");
}

#[test]
fn doubling_mapped_items_stop_at_the_point_budget() {
    // 30 levels that each instance the next level twice. Every level's second copy
    // moves by a new power of two and a slightly different turn, so that neither
    // points nor frame directions merge. Fully expanded this is 2^30 boxes.
    let mut levels = String::from(
        "#2001=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#1,$,$);
         #2003=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#13));
         #3000=IFCREPRESENTATIONMAP(#4,#2003);",
    );
    for level in 1..30u64 {
        let (id, previous) = (3000 + level * 10, 3000 + (level - 1) * 10);
        let (s, c) = (level as f64 * 0.01).sin_cos();
        levels.push_str(&format!(
            "#{p}=IFCCARTESIANPOINT(({offset:?},0.,0.));
             #{d}=IFCDIRECTION(({c:?},{s:?},0.));
             #{t}=IFCCARTESIANTRANSFORMATIONOPERATOR3D(#{d},$,#{p},$,$);
             #{a}=IFCMAPPEDITEM(#{previous},#2001);#{b}=IFCMAPPEDITEM(#{previous},#{t});
             #{r}=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#{a},#{b}));
             #{id}=IFCREPRESENTATIONMAP(#4,#{r});",
            offset = 2f64.powi(level as i32),
            p = id + 1,
            d = id + 2,
            t = id + 3,
            a = id + 4,
            b = id + 5,
            r = id + 6,
        ));
    }
    levels.push_str(
        "#2004=IFCMAPPEDITEM(#3290,#2001);
         #2005=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#2004));
         #2006=IFCPRODUCTDEFINITIONSHAPE($,$,(#2005));
         #30=IFCWALL('bomb',$,'bomb',$,$,#5,#2006,$,$);",
    );
    let started = std::time::Instant::now();
    let b = good_wall_survives(&levels);
    assert!(started.elapsed().as_secs() < 60, "took {:?}", started.elapsed());
    assert!(b.elements.iter().all(|e| e.id != "bomb"), "it should exceed the budget");
    skipped_with(&b, "points");
    let longest = b.skipped.iter().map(|(_, reason)| reason.len()).max().unwrap();
    assert!(longest < 2000, "reasons must stay short, got {longest} bytes");
}

#[test]
fn failure_reasons_do_not_grow_through_nested_maps() {
    // Each level fails with two different reasons, then reports them both upward.
    let mut levels = String::from(
        "#2001=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#1,$,$);
         #2003=IFCSHAPEREPRESENTATION(#6,'Body','Unknown',(#2008,#2009));
         #2008=IFCSOMETHINGNEW($);#2009=IFCSOMETHINGELSE($);
         #3000=IFCREPRESENTATIONMAP(#4,#2003);",
    );
    for level in 1..28u64 {
        let (id, previous) = (3000 + level * 10, 3000 + (level - 1) * 10);
        levels.push_str(&format!(
            "#{a}=IFCMAPPEDITEM(#{previous},#2001);#{b}=IFCMAPPEDITEM(#{previous},#{a});
             #{r}=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#{a},#{b}));
             #{id}=IFCREPRESENTATIONMAP(#4,#{r});",
            a = id + 1,
            b = id + 2,
            r = id + 3,
        ));
    }
    levels.push_str(
        "#2004=IFCMAPPEDITEM(#3270,#2001);
         #2005=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#2004));
         #2006=IFCPRODUCTDEFINITIONSHAPE($,$,(#2005));
         #30=IFCWALL('x',$,'x',$,$,#5,#2006,$,$);",
    );
    let b = good_wall_survives(&levels);
    let longest = b.skipped.iter().map(|(_, reason)| reason.len()).max().unwrap();
    assert!(longest < 2000, "reasons must stay short, got {longest} bytes");
}

/// A file touching most of the reader, for the corruption tests.
fn rich_file() -> String {
    let body = format!(
        "{UNIT_BOX_MAP}{wall}{shape}{wall_entity}{place}
         #100=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#1,$,$);
         #101=IFCMAPPEDITEM(#70,#100);
         #102=IFCSHAPEREPRESENTATION(#6,'Body','MappedRepresentation',(#101));
         #103=IFCPRODUCTDEFINITIONSHAPE($,$,(#102));
         #104=IFCWINDOW('win',$,'Fen\\X2\\00EA\\X0\\tre',$,$,#40,#103,$,1.,1.);
         #105=IFCCIRCLEPROFILEDEF(.AREA.,$,$,0.2);
         #106=IFCEXTRUDEDAREASOLID(#105,#4,#2,3.);
         #107=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#106,#106);
         #108=IFCSHAPEREPRESENTATION(#6,'Body','Clipping',(#107));
         #109=IFCPRODUCTDEFINITIONSHAPE($,$,(#108));
         #110=IFCCOLUMN('col',$,'Col',$,$,#40,#109,$,$);
         #111=IFCMATERIAL('Brick');#112=IFCMATERIALLAYER(#111,0.2,$);#113=IFCMATERIALLAYERSET((#112),'Set');
         #114=IFCMATERIALLAYERSETUSAGE(#113,.AXIS2.,.POSITIVE.,0.);
         #115=IFCRELASSOCIATESMATERIAL('m',$,$,$,(#30),#114);
         #116=IFCBUILDINGSTOREY('st',$,'L1',$,$,#5,$,$,.ELEMENT.,0.);
         #117=IFCRELCONTAINEDINSPATIALSTRUCTURE('c',$,$,$,(#30,#104,#110),#116);
         #118=IFCPOLYLINE((#1,#41,#1));
         #119=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#118);
         #120=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,1.,1.)),$);
         #121=IFCTRIANGULATEDFACESET(#120,$,.T.,((1,2,1)),$);
         #122=IFCSHAPEREPRESENTATION(#6,'Body','Tessellation',(#121));
         #123=IFCPRODUCTDEFINITIONSHAPE($,$,(#122));
         #124=IFCSLAB('sl',$,'Slab',$,$,#5,#123,$,.FLOOR.);",
        wall = wall_solid(5.0, 0.2, 3.0),
        shape = body_shape("#13"),
        wall_entity = wall_entity("#5"),
        place = placement_at([1.0, 2.0, 3.0], 0.5, "#5"),
    );
    file_with(METRE_UNITS, &body)
}

#[test]
fn rich_file_loads_cleanly() {
    let b = read(&rich_file()).unwrap();
    assert_eq!(b.elements.len(), 4, "{:?}", b.skipped);
    assert!(b.skipped.is_empty(), "{:?}", b.skipped);
    assert_eq!(b.elements[0].storey.as_deref(), Some("L1"));
    assert_eq!(b.elements[1].name, "Fen\u{ea}tre");
}

#[test]
fn crlf_line_endings_read_the_same() {
    let unix = rich_file();
    let windows = unix.replace('\n', "\r\n");
    assert_eq!(read(&windows).unwrap(), read(&unix).unwrap());
}

#[test]
fn project_units_win_over_an_earlier_assignment() {
    let units = "#9001=IFCUNITASSIGNMENT((#9002));#9002=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
        #8000=IFCUNITASSIGNMENT((#8001));#8001=IFCSIUNIT(*,.LENGTHUNIT.,.KILO.,.METRE.);";
    close(load_with(units, "").unit_to_m, 0.001);
}

#[test]
fn truncation_at_any_byte_never_panics() {
    let text = rich_file();
    for end in 0..=text.len() {
        if let Some(prefix) = text.get(..end) {
            let _ = read(prefix);
        }
    }
}

/// Minimal xorshift, so failures reproduce from the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn random_corruption_never_panics() {
    let original = rich_file();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let replacements: &[u8] = b"#$*'\"().,;=/\\ \n0123456789EeXT-+ABC";
    for _ in 0..20_000 {
        let mut bytes = original.clone().into_bytes();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(bytes.len());
            match rng.below(3) {
                0 => bytes[at] = replacements[rng.below(replacements.len())],
                1 => {
                    bytes.remove(at);
                }
                _ => bytes.insert(at, replacements[rng.below(replacements.len())]),
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        let building = match read(&text) {
            Ok(building) => building,
            Err(_) => continue,
        };
        // Whatever loaded must still be finite and non-inverted.
        for element in &building.elements {
            for axis in 0..3 {
                assert!(element.min[axis] <= element.max[axis], "{element:?}");
                assert!(element.center[axis].is_finite() && element.size[axis].is_finite(), "{element:?}");
            }
            assert!(element.yaw_rad.is_finite());
        }
    }
}

/// The corruption test over a real export, which reaches paths hand-written files
/// do not. Run with the file's path in `IFC_TEST_FILE`:
/// `cargo test -p ifc --release -- --ignored --nocapture real_file_corruption`
#[test]
#[ignore = "needs IFC_TEST_FILE to name a real .ifc file"]
fn real_file_corruption_never_panics() {
    let path = std::env::var("IFC_TEST_FILE").expect("set IFC_TEST_FILE");
    let original = text_from_bytes(std::fs::read(&path).unwrap()).unwrap().into_bytes();
    let rounds: usize = std::env::var("IFC_TEST_ROUNDS").map_or(300, |r| r.parse().unwrap());
    let baseline = read(std::str::from_utf8(&original).unwrap()).unwrap().elements.len();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let replacements: &[u8] = b"#$*'\"().,;=/\\ \n0123456789EeXT-+ABC";
    let mut errors = 0;
    let mut counts: Vec<usize> = Vec::new();
    let mut worst: Option<(usize, Vec<(String, String)>)> = None;
    for _ in 0..rounds {
        let mut bytes = original.clone();
        // Many small hits, so several entities break at once in every round.
        for _ in 0..1 + rng.below(200) {
            let at = rng.below(bytes.len());
            bytes[at] = replacements[rng.below(replacements.len())];
        }
        match read(&String::from_utf8_lossy(&bytes)) {
            Ok(building) => {
                let loaded = building.elements.len();
                counts.push(loaded);
                assert!(building.elements.iter().all(|e| {
                    (0..3).all(|axis| e.min[axis] <= e.max[axis] && e.size[axis].is_finite())
                }));
                if worst.as_ref().is_none_or(|(fewest, _)| loaded < *fewest) {
                    worst = Some((loaded, building.skipped));
                }
            }
            Err(_) => errors += 1,
        }
    }
    counts.sort_unstable();
    let at = |q: f64| counts[((counts.len() - 1) as f64 * q) as usize];
    println!(
        "{rounds} rounds, {errors} rejected, baseline {baseline}; loaded min {} p10 {} median {} max {}",
        at(0.0),
        at(0.1),
        at(0.5),
        at(1.0)
    );
    if let Some((loaded, skipped)) = worst {
        println!("worst round loaded {loaded}; sample of its reasons:");
        for (ty, reason) in skipped.iter().take(6) {
            println!("  {ty}: {}", reason.chars().take(110).collect::<String>());
        }
    }
    // Damage to an entity many others share (the origin point of every placement,
    // say) rightly loses those elements, but that must be the exception.
    assert!(at(0.5) * 10 >= baseline * 9, "most rounds should keep nearly everything");
}

/// Reads an in-memory file of several hundred megabytes and prints the time per
/// size, to show that reading scales linearly. Run with:
/// `cargo test -p ifc --release -- --ignored --nocapture large_file_scales_linearly`
#[test]
#[ignore = "builds several hundred megabytes in memory"]
fn large_file_scales_linearly() {
    let mut last: Option<(usize, f64)> = None;
    for walls in [150_000usize, 600_000, 1_500_000] {
        let mut body = String::with_capacity(walls * 420);
        body.push_str("#20=IFCBUILDINGSTOREY('st',$,'L1',$,$,#5,$,$,.ELEMENT.,0.);");
        let mut related = String::new();
        for i in 0..walls {
            let n = 100_000 + i * 10;
            let x = (i % 1000) as f64;
            let y = (i / 1000) as f64;
            body.push_str(&format!(
                "#{a}=IFCCARTESIANPOINT(({x:.3},{y:.3},0.));\n\
                 #{b}=IFCAXIS2PLACEMENT3D(#{a},#2,#3);\n\
                 #{c}=IFCLOCALPLACEMENT(#5,#{b});\n\
                 #{d}=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,0.2);\n\
                 #{e}=IFCEXTRUDEDAREASOLID(#{d},#4,#2,3.);\n\
                 #{f}=IFCSHAPEREPRESENTATION(#6,'Body','SweptSolid',(#{e}));\n\
                 #{g}=IFCPRODUCTDEFINITIONSHAPE($,$,(#{f}));\n\
                 #{n}=IFCWALL('{i:022}',$,'Wall {i}',$,$,#{c},#{g},$,$);\n",
                a = n + 1,
                b = n + 2,
                c = n + 3,
                d = n + 4,
                e = n + 5,
                f = n + 6,
                g = n + 7,
            ));
            if i > 0 {
                related.push(',');
            }
            related.push_str(&format!("#{n}"));
        }
        body.push_str(&format!("#21=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,({related}),#20);\n"));
        let text = file_with(METRE_UNITS, &body);
        let started = std::time::Instant::now();
        let building = read(&text).unwrap();
        let seconds = started.elapsed().as_secs_f64();
        assert_eq!(building.elements.len(), walls);
        assert!(building.elements.iter().all(|e| e.storey.as_deref() == Some("L1")));
        let megabytes = text.len() / 1_000_000;
        let rate = seconds / megabytes as f64;
        println!("{megabytes} MB, {walls} walls: {seconds:.2} s, {:.1} ms/MB", rate * 1e3);
        if let Some((_, previous)) = last {
            // Lookups are logarithmic, so allow some growth, but not quadratic.
            assert!(rate < previous * 2.5, "time per MB went from {previous} to {rate}");
        }
        last = Some((megabytes, rate));
    }
}
