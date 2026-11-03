//! Assistant and MCP tools for the outcome recipes.

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde_json::{Value, json};

use super::hardware::{self, JobState, Jobs};
use super::{Predictions, acceptance, anchors, battery, compare, drift, erces, esl, proximity, route, spec, stress, survey};
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;
use crate::rtls::antenna::AntennaChoice;
use crate::rtls::device::Device;
use crate::rtls::planning::Tech;
use crate::rtls::radio;
use crate::rtls::{RtlsState, sim};
use crate::scene::PlacedObject;

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn is_tool(name: &str) -> bool {
    name.starts_with("design_") || name.starts_with("site_") || name.starts_with("outcomes_")
}

pub fn defs() -> Vec<ToolDef> {
    let project = || json!({ "template": { "type": "string", "description": "Antenna template id (antenna_list_templates or: dipole, monopole, strip_dipole, bowtie, patch_air, pifa, slot)" }, "f0": { "type": "string", "description": "Design frequency, e.g. 2.45GHz" }, "vars": { "type": "object", "description": "Template variables to change, e.g. {\"L\":\"31mm\"}" } });
    let with = |mut base: Value, extra: Value| {
        if let (Some(a), Some(b)) = (base.as_object_mut(), extra.as_object()) {
            a.extend(b.clone());
        }
        json!({ "type": "object", "properties": base })
    };
    vec![
        def("outcomes_ledger", "Every prediction made so far: the range, what it rests on (estimate, simulated, calibrated, measured), the verdict against its target, assumptions, and how measurements compared. Read this before telling anyone a number.", json!({ "type": "object", "properties": { "markdown": { "type": "boolean", "description": "Return the readable report rather than JSON" } } })),
        def("outcomes_check", "Record a real measurement against a prediction by its id. The prediction stays as made, so the ledger can score it.", json!({ "type": "object", "properties": { "id": { "type": "string" }, "measured": { "type": "number" }, "note": { "type": "string" }, "source": { "type": "string", "description": "Where it came from: the instrument, file or person" } }, "required": ["id", "measured"] })),
        def("design_job", "Status and result of a design_ job by id. With no id, lists all jobs.", json!({ "type": "object", "properties": { "id": { "type": "integer" } } })),
        def("design_uwb_bias", "How much an antenna's shape shifts UWB ranges with direction, after a normal antenna-delay calibration, per direction. Can search dimensions for the lowest shift, and save the pattern so devices in the scene use it (site_set_antenna), which carries the shift into position error. Metal-only antennas (method of moments). Runs in the background: poll design_job.", with(project(), json!({ "channel": { "type": "integer", "enum": [5, 9] }, "region": { "type": "string", "enum": ["horizon", "all"] }, "theta_min": { "type": "number", "description": "Degrees from the antenna axis: the cone a site really uses (from rtls_smart_plan brief). Overrides region." }, "theta_max": { "type": "number" }, "optimize": { "type": "array", "description": "[{name,min,max}], SI values" }, "max_evals": { "type": "integer" }, "save_as": { "type": "string" }, "target_cm": { "type": "number" }, "frequencies": { "type": "integer" }, "angle_step": { "type": "number" }, "edges_per_lambda": { "type": "number" } }))),
        def("design_shelf", "An RFID reader antenna on a real metal shelf, solved together, judged by read rate on this level and stray reads on the levels around it, with a range and the reader power that fits best. With optimize=true also searches its size, height, position and a metal rim. Background job: poll design_job.", json!({ "type": "object", "properties": { "width_m": { "type": "number" }, "depth_m": { "type": "number" }, "gap_m": { "type": "number", "description": "Between levels" }, "levels_above": { "type": "integer" }, "levels_below": { "type": "integer" }, "f_mhz": { "type": "number" }, "product_loss_db": { "type": "number" }, "product_loss_spread_db": { "type": "number" }, "min_read": { "type": "number" }, "max_stray": { "type": "number" }, "optimize": { "type": "boolean" }, "max_evals": { "type": "integer" }, "edges_per_lambda": { "type": "number" } } })),
        def("design_aoa", "Angle-of-arrival error of a coupled array against the textbook phase model, and a calibration table made from the solved array. Reports what to use for the locator's angle noise. Background job: poll design_job.", json!({ "type": "object", "properties": { "nx": { "type": "integer" }, "ny": { "type": "integer" }, "spacing_m": { "type": "number" }, "f_ghz": { "type": "number" }, "theta_max": { "type": "number" }, "phase_sigma_deg": { "type": "number" }, "amp_sigma_db": { "type": "number" }, "snr_db": { "type": "number" }, "band_share": { "type": "number", "description": "How far the channels run either side of f_ghz, as a share. Default 0.016, BLE" }, "save_as": { "type": "string" } } })),
        def("design_integrate", "What an enclosure, battery or nearby metal does to an antenna, and fixes: a matching network, and retuning a dimension if you name it. Parts are boxes; a shell wraps the antenna in plastic. Plastic needs a build with Antenna Studio (FDTD). Background job: poll design_job.", with(project(), json!({ "parts": { "type": "array", "description": "[{name, material, min:[x,y,z], max:[x,y,z]}], expressions allowed" }, "shell": { "type": "object", "description": "{material, gap_m, wall_m}" }, "f_lo_ghz": { "type": "number" }, "f_hi_ghz": { "type": "number" }, "retune": { "type": "string", "description": "Variable to retune, e.g. L" }, "max_loss_db": { "type": "number", "description": "Most power to lose to mismatch at the band's worst point. Default 1" } }))),
        def("design_tag", "Which RFID tag kind and position for a product, ranked by the chance of reaching a read range. A quick physical estimate for ranking: confirm on the real product.", json!({ "type": "object", "properties": { "contents": { "type": "string", "enum": ["dry_goods", "cardboard", "plastic", "water_based", "oily", "frozen", "metal_can", "foil_lined"] }, "target_range_m": { "type": "number" }, "region": { "type": "string", "enum": ["fcc", "etsi"] } }, "required": ["contents"] })),
        def("design_coexist", "How radios in one device hurt each other: sensitivity lost, range left while the other transmits, isolation needed, harmonics. Radios by preset name (BLE, WiFi 2.4 GHz, WiFi 5 GHz, WiFi 6E, UWB channel 5, UWB channel 9, UHF RFID reader, LTE-M band 13) or full objects with datasheet numbers. Isolation from a Touchstone file, or a flat figure.", json!({ "type": "object", "properties": { "radios": { "type": "array" }, "isolation_db": { "type": "number" }, "touchstone": { "type": "string" } }, "required": ["radios"] })),
        def("design_fit_measured", "Compare a real antenna's VNA sweep (Touchstone) with the simulation: frequency scale, extra series resistance (loss or radiation: S11 alone cannot say, and it reports when two values fit equally well), what substrate εr would explain it, and the error before and after. Background job: poll design_job.", with(project(), json!({ "measured": { "type": "string" }, "simulated": { "type": "string", "description": "Touchstone file; or give a template to solve" }, "er": { "type": "number", "description": "Substrate εr the model used" } }))),
        def("design_field_yield", "Share of manufactured units that keep their link range, with dimensions spread as the factory makes them, rather than whether S11 stays under a line. Background job: poll design_job.", with(project(), json!({ "tolerances": { "type": "array", "description": "[{name, sigma_m}]" }, "min_range_share": { "type": "number" }, "path_loss_exp": { "type": "number" }, "f_lo_ghz": { "type": "number" }, "f_hi_ghz": { "type": "number" } }))),
        def("site_survey_plan", "Where to stand for a short walk with a real UWB tag: the fewest spots that best pin down what this site's materials cost and each anchor's delay.", json!({ "type": "object", "properties": { "spots": { "type": "integer" }, "height": { "type": "number" }, "step_m": { "type": "number" } } })),
        def("site_survey_fit", "Fit the site's radio model to a walk (a file of device,x,y,z,kind,value or JSON lines; kind is range or rssi). Reports what each material really costs and the error at spots held out of the fit, with the built-in values and with only what would be applied. apply=true writes the material scales that pass its checks into this tab, for this band: it needs origin. A fitted path-loss exponent, device delays and level offsets are reported, never applied. Only origin=field can count as calibration; a practice walk is refused as field however it was converted.", json!({ "type": "object", "properties": { "path": { "type": "string" }, "readings": { "type": "array" }, "origin": { "type": "string", "enum": ["field", "practice"], "description": "field: you measured these on site with a real tag. practice: made up, for trying the loop" }, "apply": { "type": "boolean" }, "prior_sigma": { "type": "number", "description": "How unsure the built-in material costs are (default 0.5, as a share)" } } })),
        def("site_trace", "What a radio ray crosses between two points, in order, and what the model takes each thing to be. For checking the model against the real building.", json!({ "type": "object", "properties": { "from": { "type": "array" }, "to": { "type": "array" } }, "required": ["from", "to"] })),
        def("site_guess_materials", "Look at a photo and guess what things are made of (kind site: thin, wood, glass, concrete, metal, liquid; kind product: dry_goods, cardboard, plastic, water_based, oily, frozen, metal_can, foil_lined). Only a suggestion, never applied by itself: site guesses were right about 60 % of the time on test photos and product guesses about 30 %, so check them with the user. use_sam outlines each object first (a 947 MB download, under Meta's SAM licence); it did not improve the guess in testing. Background job: poll design_job.", json!({ "type": "object", "properties": { "path": { "type": "string" }, "kind": { "type": "string", "enum": ["site", "product"] }, "labels": { "type": "array", "items": { "type": "string" } }, "boxes": { "type": "array", "description": "[[x0,y0,x1,y1]] from 0 to 1" }, "use_sam": { "type": "boolean" }, "threshold": { "type": "number" } }, "required": ["path"] })),
        def("site_survey_clear", "Forget a survey's material fit, back to the built-in values.", json!({ "type": "object", "properties": {} })),
        def("site_survey_practice", "Write a SYNTHETIC walk to a file, as if the site's materials had the scales in truth (e.g. {\"metal\":0.5}), to try the survey loop or test it against a known answer. Never present its output as a measurement.", json!({ "type": "object", "properties": { "path": { "type": "string" }, "truth": { "type": "object" }, "spots": { "type": "array" }, "seed": { "type": "integer" } }, "required": ["path"] })),
        def("site_stress", "Run the positioning system through full and empty racks, crowds, worse materials and a dead anchor, with a range across random combinations. A combination where any tag has no position counts as a miss. The verdict counts only the random combinations; the named conditions are on the claim beside it. Pauses nothing else but takes a while: each scenario runs the whole simulation.", json!({ "type": "object", "properties": { "seconds": { "type": "number" }, "target_cm": { "type": "number" }, "samples": { "type": "integer", "description": "Random combinations, default 30. Below 29 it can't say Likely even if all pass" }, "anchors_down": { "type": "integer" } } })),
        def("site_drift_baseline", "Record the devices' positions as designed, to compare live ranges against.", json!({ "type": "object", "properties": {} })),
        def("site_drift_check", "Has the site stopped matching its model? Compares ranges with the baseline positions: finds devices that moved (how far, which way), changed delay, went silent, and new obstructions (where). Reads a JSON-lines file {device,tag:[x,y,z],range_m,expected_excess_m}, where expected_excess_m is what known walls add on that link (leave it out and any wall reads as drift). With no path it's a drill on the running simulation: it tries the check and never says healthy. Tags must move: ranges from fewer than 5 spots say nothing, since a fixed reflection looks the same as a change.", json!({ "type": "object", "properties": { "path": { "type": "string" }, "seconds": { "type": "number" }, "noise_m": { "type": "number", "description": "Range noise when all is well on this system, 1σ. Default 0.12" } } })),
        def("site_material_ranges", "For a building not built yet: coverage across undecided materials, as loss ranges in dB, and which decision matters most.", json!({ "type": "object", "properties": { "tech": { "type": "string", "enum": ["uwb", "ble", "rfid", "wifi"] }, "metric": { "type": "string", "enum": ["reach", "fix"], "description": "reach (default): a usable signal arrives at all. fix: enough radios for a position" }, "min_dbm": { "type": "number" }, "ranges": { "type": "array", "description": "[{material, low_db, high_db, note}], default: glass, concrete and internal walls" }, "min_coverage": { "type": "number" }, "samples": { "type": "integer" }, "height": { "type": "number" }, "cell": { "type": "number" } } })),
        def("site_set_material", "Say what one placed object is made of, replacing the guess from its name, in every radio prediction. Materials: thin, wood, glass, object, concrete, metal, liquid. Omit material to clear it.", json!({ "type": "object", "properties": { "object": { "type": "string" }, "material": { "type": "string" }, "weight": { "type": "number", "description": "How many typical walls one crossing is worth (default: the material's). Metal-coated low-E glass is glass with weight 4: about 24 dB against 3 dB for plain glass at 6 dB a wall, 32 against 4 at RFID's 8" } }, "required": ["object"] })),
        def("site_conditions", "Set how the scene behaves now: rack_fill (0..1, share of bays with goods) and people_per_100m2. No arguments reports the current values.", json!({ "type": "object", "properties": { "rack_fill": { "type": "number" }, "people_per_100m2": { "type": "number" } } })),
        def("site_erces", "Will firefighters' radios work inside a building not built yet? Coverage per floor from a signal arriving outside, across undecided glazing and walls, against the fire code's shares (commonly 95 % generally and 99 % in critical areas, at -95 dBm: check your local code and its edition). Says which material decides it, and how strong the outside signal has to be. This predicts; only the required test on the finished building can pass it.", json!({ "type": "object", "properties": { "floors": { "type": "array", "description": "Heights in metres; default: found from the model" }, "outside_dbm": { "type": "number", "description": "Signal arriving outside: use a drive test. The default is a guess" }, "outside_sigma_db": { "type": "number" }, "threshold_dbm": { "type": "number" }, "general_share": { "type": "number" }, "critical_share": { "type": "number" }, "critical_zones": { "type": "array", "items": { "type": "string" }, "description": "Zone names that count as critical: fire pump room, stairs, command centre" }, "ranges": { "type": "array", "description": "[{material, low_db, high_db, note}]" }, "cell": { "type": "number" }, "samples": { "type": "integer" } } })),
        def("site_compare_tech", "Which technology for this site? Deploys each one on the real layout, simulates it and costs it over the years you'll own it, including changing tag batteries. Give your own prices: without them it uses placeholders and says so. Slow: each one runs a full deployment. Everything auto deploy had placed is put back.", json!({ "type": "object", "properties": { "techs": { "type": "array", "items": { "type": "string", "enum": ["uwb", "ble", "ble_aoa", "wifi", "rfid"] } }, "target_cm": { "type": "number" }, "tags": { "type": "integer" }, "years": { "type": "number" }, "moving_share": { "type": "number", "description": "Share of time tags are moving" }, "update_hz": { "type": "number", "description": "Updates per second while moving, for battery life. Default 1" }, "test_s": { "type": "number" }, "points": { "type": "integer", "description": "Spots a tag stands at, the same for each technology. Default 30" }, "settle_s": { "type": "number" }, "prices": { "type": "object", "description": "uwb_anchor, uwb_tag, ble_node, ble_locator, ble_tag, wifi_ap, wifi_tag, rfid_reader, rfid_tag, install_per_device, tag_service, annual_per_device" } } })),
        def("site_acceptance_plan", "Fix a handover test before anyone tests: how many points it takes to show a share at a confidence (59 points all passing shows 95 % at 95 %), and where: drawn at random from equal patches of the free floor, as the statistics need. Writes the plan to a file, with a fingerprint printed on the report, so a changed plan shows.", json!({ "type": "object", "properties": { "target_cm": { "type": "number", "description": "The accuracy both sides agreed" }, "pass_share": { "type": "number" }, "confidence": { "type": "number" }, "tech": { "type": "string", "enum": ["uwb", "ble", "wifi"] }, "points": { "type": "integer", "description": "Default: as many as the statistics need" }, "height": { "type": "number" }, "step_m": { "type": "number" }, "seed": { "type": "integer", "description": "The draw; default the time now. Recorded in the plan" }, "path": { "type": "string" } }, "required": ["target_cm"] })),
        def("site_acceptance_preview", "Run the fixed test plan in the simulator: a tag stands at each point. A forecast of the real test, always labelled simulated, and never counted as passing anything.", json!({ "type": "object", "properties": { "settle_s": { "type": "number" }, "path": { "type": "string", "description": "Also write the HTML report here" } } })),
        def("site_acceptance_score", "Score real test results against the fixed plan: what share it shows at the agreed confidence, pass or fail, and an HTML handover report. With source saying who measured them and how, they become a measured entry in the ledger and are recorded against the site's stress prediction. Without it they count for nothing: never fill it in yourself.", json!({ "type": "object", "properties": { "source": { "type": "string", "description": "Who measured these and how, from the person who did" }, "path": { "type": "string", "description": "Results: id,x,y,z or id,error_m per line, or JSON lines" }, "results": { "type": "array" }, "plan_path": { "type": "string", "description": "Default: the plan in force" }, "report_path": { "type": "string" } } })),
        def("site_anchor_fit", "Where the UWB anchors really are, from a log of the ranges between them (a,b,range_m per line, or JSON lines). Unlike a plain survey it knows which links cross a rack or wall and trusts those less, and reports what ignoring that would have said. apply=true moves the anchors in the scene; a practice log can never be applied.", json!({ "type": "object", "properties": { "path": { "type": "string" }, "ranges": { "type": "array" }, "prior_m": { "type": "number", "description": "How far the design positions could be off, 1σ. Default 2" }, "surveyed": { "type": "array", "items": { "type": "string" }, "description": "Anchors whose scene position was surveyed. Two or more tie the set to the building; ranges alone only fix its shape" }, "source": { "type": "string", "description": "Who logged these ranges and how, from the person who did. Needed to apply; never fill it in yourself" }, "apply": { "type": "boolean" } } })),
        def("site_anchor_practice", "Write a SYNTHETIC log of ranges between the anchors, as if some had been fitted elsewhere than designed, to try site_anchor_fit or test it against a known answer. Never present it as a measurement.", json!({ "type": "object", "properties": { "path": { "type": "string" }, "moves": { "type": "object", "description": "Anchor id to [dx, dz] metres" }, "rounds": { "type": "integer" }, "seed": { "type": "integer" } }, "required": ["path"] })),
        def("site_battery", "How fast can a tag report and still last? Simulates the site at each update rate for accuracy, works out battery life from typical energy figures, and picks the slowest rate that holds the accuracy target with a tag moving, saying whether it lasts. Battery figures are typical values: give your tag's own if you have them.", json!({ "type": "object", "properties": { "profile": { "type": "string" }, "rates": { "type": "array", "items": { "type": "number" } }, "moving_share": { "type": "number" }, "still_hz": { "type": "number" }, "speed_mps": { "type": "number", "description": "How fast tags move when they do, which is what a slow rate costs. Default 1.3, walking" }, "target_cm": { "type": "number" }, "target_years": { "type": "number" }, "seconds": { "type": "number" }, "capacity_mah": { "type": "number" }, "update_uc": { "type": "number", "description": "Charge per update, microcoulombs" } } })),
        def("site_esl", "Electronic shelf labels: labels are sampled along the faces of the shelves, then it reports the share reached, how long a store-wide price change takes with retries and shared airtime, the channel plan, and the worst fixtures. Gateways are esl-gateway devices; place=N finds where N more do the most, and apply=true adds them. Radio figures are placeholders: use your vendor's.", json!({ "type": "object", "properties": { "place": { "type": "integer" }, "apply": { "type": "boolean" }, "labels": { "type": "integer", "description": "How many labels the store really has, for the update time. Default one per spot on the shelves" }, "updates_per_s": { "type": "number", "description": "Label updates per second per gateway. Default 2, use your vendor's" }, "channels": { "type": "integer" }, "min_reach": { "type": "number" }, "samples": { "type": "integer" } } })),
        def("design_tunnel", "An RFID tunnel on a conveyor: tags read per pass for each tag kind, whole cartons read, the fastest belt that holds the target, and whether time or the link is what limits it. Give conveyor to read the belt speed from a running plant model. A quick estimate: test the real carton mix.", json!({ "type": "object", "properties": { "contents": { "type": "string", "enum": ["dry_goods", "cardboard", "plastic", "water_based", "oily", "frozen", "metal_can", "foil_lined"] }, "conveyor": { "type": "string" }, "belt_mps": { "type": "number" }, "carton_length_m": { "type": "number" }, "gap_m": { "type": "number" }, "zone_m": { "type": "number" }, "tags_per_carton": { "type": "integer" }, "reads_per_s": { "type": "number" }, "antennas": { "type": "integer" }, "antenna_distance_m": { "type": "number" }, "eirp_dbm": { "type": "number" }, "region": { "type": "string", "enum": ["fcc", "etsi"] }, "min_read": { "type": "number" } }, "required": ["contents"] })),
        def("site_route_coverage", "The radio a robot sees along its route: longest stretch with no link and where it starts, handoffs, time with no link per pass, with racks full and nearly empty, and a range across rack fill and materials. Route from points, or a device's path. Signal strength only: congestion and how the robot's radio really roams aren't modelled.", json!({ "type": "object", "properties": { "points": { "type": "array", "description": "[[x,y,z], ...]" }, "device": { "type": "string", "description": "A device with a path; default the first one" }, "tech": { "type": "string", "enum": ["wifi", "ble"] }, "threshold_dbm": { "type": "number" }, "hysteresis_db": { "type": "number" }, "speed_mps": { "type": "number" }, "handoff_ms": { "type": "number" }, "need_mbps": { "type": "number" }, "max_gap_m": { "type": "number" }, "samples": { "type": "integer" } } })),
        def("design_medical_coexist", "Which of a hospital's radios to test a wireless medical device against first, and how many times an hour its function would go too long without data. Uses typical ward radios, your list, or with position the devices in the scene with the walls between. For planning tests only: it doesn't show coexistence and doesn't replace ANSI C63.27 or a risk analysis under AAMI TIR69.", json!({ "type": "object", "properties": { "radio": { "type": "string", "description": "The device's radio, by preset name. Default BLE" }, "margin_db": { "type": "number" }, "packets_per_s": { "type": "number" }, "packet_ms": { "type": "number" }, "max_gap_s": { "type": "number" }, "neighbours": { "type": "array", "description": "[{name, radio, distance_m, count, extra_loss_db, burst_ms}]" }, "position": { "type": "array", "description": "[x, y, z] of the device in the scene" }, "typical": { "type": "boolean", "description": "Also include typical ward radios" }, "max_gaps_per_hour": { "type": "number", "description": "How often it may go too long without data. Default 0.1: set it from your risk analysis" }, "samples": { "type": "integer" } } })),
        def("site_proximity", "Forklift and pedestrian alerts tested at the real corners: a vehicle and a person head for the same spot in the layout, and the alert has to fire in time to stop. Reports the share in time overall and at blind corners, how far out the alarm must be, and how often someone safely to the side sets it off. Supports a design only: it can't show a system is safe.", json!({ "type": "object", "properties": { "speed_mps": { "type": "number" }, "decel_mps2": { "type": "number" }, "reaction_s": { "type": "number" }, "walker_mps": { "type": "number" }, "workers": { "type": "integer", "description": "People sharing the ranging airtime" }, "update_hz": { "type": "number" }, "slot_ms": { "type": "number" }, "margin_m": { "type": "number" }, "encounters": { "type": "integer" }, "min_detect": { "type": "number" } } })),
        def("design_sar", "A SAR pre-check for a body-worn antenna: real FDTD with a slab of tissue beside it, averaged over 1 g and 10 g, the most power each limit allows, and what the body does to the match. Needs the FDTD solver; minutes to tens of minutes. A design-stage check: only an accredited lab's measurement can show compliance. Background job: poll design_job.", with(project(), json!({ "gap_mm": { "type": "number", "description": "Antenna to tissue. Default 5" }, "power_mw": { "type": "number", "description": "Average power reaching the antenna. Default 100" }, "axis": { "type": "string", "enum": ["x", "y", "z"], "description": "Which side the body is on" }, "tissue": { "type": "string", "description": "Default: SAR tissue 900 MHz or SAR tissue 2450 MHz" } }))),
        def("site_set_antenna", "Give a device an antenna from the library (a design_uwb_bias result carries its range shift into the simulation), and/or mark a tag as worn on a person so their body shadows it.", json!({ "type": "object", "properties": { "device": { "type": "string" }, "antenna": { "type": "string" }, "worn": { "type": "boolean" } }, "required": ["device"] })),
    ]
}

fn result_of(r: anyhow::Result<u64>) -> ToolOutput {
    match r {
        Ok(id) => ToolOutput::ok(json!({ "started": id, "note": "Running in the background. Poll design_job with this id." })),
        Err(e) => ToolOutput::err(format!("{e:#}")),
    }
}

fn start_job<F>(world: &mut World, title: &str, work: anyhow::Result<F>) -> ToolOutput
where
    F: FnOnce() -> anyhow::Result<hardware::JobResult> + Send + 'static,
{
    result_of(work.map(|w| world.resource_mut::<Jobs>().start(title, w)))
}

/// A recipe quick enough to answer straight away.
fn instant(world: &mut World, r: anyhow::Result<hardware::JobResult>) -> ToolOutput {
    match r {
        Ok(res) => {
            let mut ledger = world.resource_mut::<Predictions>();
            for c in res.claims {
                ledger.put(c);
            }
            ToolOutput::ok(res.value)
        }
        Err(e) => ToolOutput::err(format!("{e:#}")),
    }
}

fn job_json(j: &hardware::Job) -> Value {
    let (state, result, error) = match &j.state {
        JobState::Running => ("running", Value::Null, Value::Null),
        JobState::Done(v) => ("done", v.clone(), Value::Null),
        JobState::Failed(e) => ("failed", Value::Null, json!(e)),
    };
    json!({ "id": j.id, "title": j.title, "state": state, "seconds": j.seconds.unwrap_or_else(|| j.started.elapsed().as_secs_f64()), "result": result, "error": error })
}

/// The scene's name, which ties site predictions and fits to it.
fn scene_of(world: &World) -> String {
    world.resource::<crate::ui::UiState>().scene_title().to_string()
}

/// Runs the simulation and reads every live range, for a drift check.
fn live_samples(world: &mut World, seconds: f64) -> Vec<drift::Sample> {
    let mut out = Vec::new();
    // Each device's id, and what one typical wall costs on its links.
    let ids: BTreeMap<Entity, (String, f32)> = {
        let list: Vec<(Entity, Device)> = world.query::<(Entity, &Device)>().iter(world).map(|(e, d)| (e, d.clone())).collect();
        let profiles = world.resource::<crate::rtls::device::Profiles>();
        list.into_iter()
            .map(|(e, d)| {
                let p = profiles.for_device(&d);
                (e, (d.id, p.nlos_bias_m + p.nlos_sigma_m))
            })
            .collect()
    };
    let steps = (seconds / 0.5).ceil().max(1.0) as usize;
    for _ in 0..steps {
        sim::run_for(world, 0.5);
        let st = world.resource::<RtlsState>();
        for t in st.tracks.values().filter(|t| t.tech == "uwb") {
            let Some(est) = t.est else { continue };
            // Ranges the solver threw out too: a moved anchor's are just the ones it would.
            for l in t.links.iter().filter(|l| l.kind == crate::rtls::LinkKind::Range) {
                let (Some(range), Some((device, per_wall))) = (l.value, ids.get(&l.other)) else { continue };
                // The walls on record are in the model; they aren't drift.
                out.push(drift::Sample { device: device.clone(), tag: est.to_array(), range_m: range, expected_excess_m: l.bias_eq * per_wall });
            }
        }
    }
    out
}

/// A list of names. One name on its own means the same as a list of one.
fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        _ => v.as_array().map_or(Vec::new(), |a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()),
    }
}

/// The radios in the scene, as a device standing at `at` would meet them, with the walls between.
#[allow(clippy::type_complexity)]
fn medical_neighbours(In(at): In<Vec3>, parts: sim::OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>, devices: Query<(&Device, &Transform)>, profiles: Res<crate::rtls::device::Profiles>) -> Vec<antenna_core::recipes::medical::Neighbour> {
    use crate::rtls::device::DeviceKind;
    let occ = sim::Occluders::build(&parts, &objects);
    let mut out = Vec::new();
    for (d, t) in devices.iter().filter(|(d, _)| d.enabled) {
        let (preset, burst_s) = match d.kind {
            DeviceKind::WifiAp if profiles.for_device(d).freq_mhz > 4000.0 => ("WiFi 5 GHz", 0.002),
            DeviceKind::WifiAp => ("WiFi 2.4 GHz", 0.002),
            DeviceKind::BleNode => ("BLE", 0.0004),
            DeviceKind::RfidReader => ("UHF RFID reader", 0.05),
            DeviceKind::UwbAnchor => ("UWB channel 5", 0.0002),
            _ => continue,
        };
        let Some(radio) = antenna_core::recipes::coexist::Radio::preset(preset) else { continue };
        let path = occ.path(at, t.translation, None).in_band(profiles.for_device(d).freq_mhz);
        out.push(antenna_core::recipes::medical::Neighbour { name: format!("{} {}", d.kind.label(), d.id), radio, distance_m: path.distance as f64, count: 1, extra_loss_db: 6.0 * path.wall_eq as f64, burst_s });
    }
    out
}

/// What a tool's numbers rest on, for when its result doesn't say for itself. `None`
/// reports no number of its own.
fn evidence_of(name: &str) -> Option<&'static str> {
    Some(match name {
        "outcomes_ledger" | "design_job" | "site_set_material" | "site_conditions" | "site_set_antenna" | "site_survey_clear" | "site_drift_baseline" => return None,
        "design_uwb_bias" | "design_shelf" | "design_aoa" | "design_integrate" | "design_fit_measured" | "design_field_yield" | "design_sar" | "site_guess_materials" => "Said with the result: poll design_job",
        "outcomes_check" => "Measured, as entered: recording it changes no label",
        "site_survey_practice" | "site_anchor_practice" => "Synthetic: practice data, not a measurement",
        "site_survey_plan" => "Estimate: where a walk would teach the model most. Not validated on hardware",
        "site_survey_fit" => "Depends on the walk: see origin",
        "site_trace" => "Simulated: what the model takes the building to be. Not validated on hardware",
        "site_acceptance_plan" => "A test plan: statistics of the test, no prediction of its result",
        "site_acceptance_preview" => "Simulated: a forecast of the test, never a pass",
        "site_acceptance_score" => "Measured, if the results are real: simulated ones are marked and can't pass",
        "site_anchor_fit" => "Measured ranges fitted to the model, if the log is real: a practice log says so",
        "site_compare_tech" => "Simulated, at placeholder prices unless you gave your own. Not validated on hardware",
        "design_tag" | "design_coexist" | "design_tunnel" | "design_medical_coexist" | "site_material_ranges" | "site_erces" | "site_esl" | "site_battery" => "Estimate: not validated on hardware",
        "site_stress" | "site_drift_check" | "site_route_coverage" | "site_proximity" => "Simulated: not validated on hardware",
        _ => "Not stated: treat it as an estimate, not validated on hardware",
    })
}

/// Adds what the numbers rest on to a result that doesn't say. A claim in the result
/// knows best; failing that, the tool's own label.
fn stamped(name: &str, out: ToolOutput) -> ToolOutput {
    let Some(fallback) = evidence_of(name).filter(|_| !out.is_error) else { return out };
    let [crate::agent::llm::Block::Text(text)] = out.content.as_slice() else { return out };
    let Ok(Value::Object(mut v)) = serde_json::from_str::<Value>(text) else { return out };
    if !v.contains_key("evidence") {
        let from_claim = v.get("claim").and_then(|c| c.pointer("/prediction/evidence")).and_then(|e| serde_json::from_value::<proof::Evidence>(e.clone()).ok());
        let label = from_claim.map_or_else(|| fallback.to_string(), |e| format!("{}: not validated on hardware", e.label()));
        v.insert("evidence".into(), json!(label));
    }
    ToolOutput::ok(Value::Object(v))
}

/// A drift check's result. Against the simulator it's a drill: the ranges come from the
/// model they're compared with, whose solver knows where the anchors truly are, so it can
/// catch a scene edit but can't show a real site is healthy, and doesn't say so.
fn drift_json(r: &drift::DriftReport, file: Option<&str>, o: &drift::DriftOptions) -> Value {
    let mut v = json!({ "too_few_samples": r.too_few_samples, "samples": r.samples, "findings": r.findings, "summary": r.findings.iter().map(drift::Finding::summary).collect::<Vec<_>>(), "residual_rms_m": r.residual_rms, "noise_assumed_m": o.noise_m, "false_alarms": drift::FALSE_ALARMS });
    match file {
        Some(path) => {
            v["source"] = json!("file");
            v["path"] = json!(path);
            v["healthy"] = json!(r.healthy);
            v["evidence"] = json!("Measured, if this file holds live ranges: the check can't tell. Its limits were set on simulated data, not validated on hardware");
        }
        None => {
            v["source"] = json!("simulation");
            v["mode"] = json!("drill");
            v["note"] = json!("A drill: these ranges come from the simulator, so this tries the check and can only catch edits to the scene. To judge a real site, give a file of its live ranges.");
            v["evidence"] = json!("Simulated: not validated on hardware");
        }
    }
    v
}

/// Handles one of the tools; `None` if the name isn't ours. Every result says what its
/// numbers rest on.
pub fn start(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    run_tool(world, name, i).map(|out| stamped(name, out))
}

fn run_tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    Some(match name {
        "outcomes_ledger" => {
            let p = world.resource::<Predictions>();
            if i["markdown"].as_bool().unwrap_or(false) {
                ToolOutput::text(p.ledger.to_markdown())
            } else {
                let (held, measured) = p.ledger.hit_rate().unwrap_or((0, 0));
                ToolOutput::ok(json!({ "claims": p.ledger.claims, "taken_off_the_list": p.ledger.removed, "predictions_measured": measured, "predictions_that_held": held, "score": p.ledger.score_line(), "validated_on_hardware": false, "note": proof::NOT_VALIDATED }))
            }
        }
        "outcomes_check" => {
            let Some(id) = i["id"].as_str() else { return Some(ToolOutput::err("give the id of the prediction")) };
            let Some(v) = i["measured"].as_f64().filter(|v| v.is_finite()) else { return Some(ToolOutput::err("give the measured value as a number, in the prediction's unit")) };
            let mut p = world.resource_mut::<Predictions>();
            let Some(c) = p.ledger.get_mut(id) else { return Some(ToolOutput::err(format!("no prediction '{id}'"))) };
            c.check_from(v, i["note"].as_str().unwrap_or(""), i["source"].as_str().unwrap_or("outcomes_check tool"));
            let k = c.checks.last().cloned();
            p.save();
            ToolOutput::ok(json!(k))
        }
        "design_job" => {
            let jobs = world.resource::<Jobs>();
            match i["id"].as_u64() {
                Some(id) => match jobs.get(id) {
                    Some(j) => ToolOutput::ok(job_json(j)),
                    None => ToolOutput::err(format!("no job {id}")),
                },
                None => ToolOutput::ok(json!({ "jobs": jobs.list.iter().map(job_json).collect::<Vec<_>>() })),
            }
        }
        "site_guess_materials" => start_job(world, "What things are made of", hardware::guess_materials_job(i)),
        "design_uwb_bias" => start_job(world, "UWB antenna bias", hardware::uwb_bias_job(i)),
        "design_shelf" => start_job(world, "RFID shelf antenna", hardware::shelf_job(i)),
        "design_aoa" => start_job(world, "Angle-of-arrival array", hardware::aoa_job(i)),
        "design_integrate" => start_job(world, "Antenna in the product", hardware::integrate_job(i)),
        "design_fit_measured" => start_job(world, "Fit to VNA sweep", hardware::fit_measured_job(i)),
        "design_field_yield" => start_job(world, "Field yield", hardware::field_yield_job(i)),
        "design_tag" => {
            let r = hardware::tag_advice(i);
            instant(world, r)
        }
        "design_coexist" => {
            let r = hardware::coexistence(i);
            instant(world, r)
        }
        "site_survey_plan" => {
            let (spots, height, step) = (i["spots"].as_u64().unwrap_or(20) as usize, i["height"].as_f64().unwrap_or(1.2) as f32, i["step_m"].as_f64().unwrap_or(2.0) as f32);
            match world.run_system_cached_with(survey::plan_system, (spots, height, step.max(0.5))) {
                Ok(p) => {
                    world.resource_mut::<survey::SurveyState>().plan = Some(p.clone());
                    ToolOutput::ok(json!(p))
                }
                Err(e) => ToolOutput::err(format!("{e}")),
            }
        }
        "site_survey_fit" => {
            let readings = if let Some(path) = i["path"].as_str() {
                match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| survey::parse_readings(&t)) {
                    Ok(r) => r,
                    Err(e) => return Some(ToolOutput::err(format!("{path}: {e:#}"))),
                }
            } else {
                match serde_json::from_value::<Vec<survey::Reading>>(i["readings"].clone()) {
                    Ok(r) => r,
                    Err(_) => return Some(ToolOutput::err("give path (a file) or readings ([{device, tag:[x,y,z], kind, value, synthetic}], synthetic true for a practice walk), and origin (field or practice)")),
                }
            };
            let declared = match i["origin"].as_str() {
                None => None,
                Some("field") => Some(survey::WalkOrigin::Field),
                Some("practice") => Some(survey::WalkOrigin::Practice),
                Some(o) => return Some(ToolOutput::err(format!("origin '{o}' should be field or practice"))),
            };
            let origin = match survey::resolve_origin(declared, &readings, &survey::PracticeWalks::load()) {
                Ok(o) => o,
                Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
            };
            let (links, unknown) = match world.run_system_cached_with(survey::links_system, readings) {
                Ok(v) => v,
                Err(e) => return Some(ToolOutput::err(format!("{e}"))),
            };
            let mut fit = match survey::fit(&links, i["prior_sigma"].as_f64().unwrap_or(0.5)) {
                Ok(f) => f,
                Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
            };
            fit.unknown_devices = unknown;
            fit.origin = origin;
            let mut written = 0;
            if i["apply"].as_bool().unwrap_or(false) {
                let scene = survey::scene_binding(world);
                written = survey::apply(&fit, i["path"].as_str().unwrap_or("readings"), scene);
            }
            let mut v = json!(fit);
            v["applied_to_model"] = json!(written > 0);
            if origin == survey::WalkOrigin::Undeclared {
                v["note"] = json!("Nothing applied: say what this walk is. origin=field if you measured it on site with a real tag, origin=practice if it's made up.");
            } else if origin == survey::WalkOrigin::Practice {
                v["note"] = json!("A practice walk: made-up readings. It can change numbers to show the loop working, but it never counts as calibration.");
            }
            v["evidence"] = json!(match origin {
                survey::WalkOrigin::Field => "A model fitted to a walk declared as measured on site. Not validated on hardware beyond that walk",
                survey::WalkOrigin::Practice => "Synthetic: a practice walk, not a measurement",
                survey::WalkOrigin::Undeclared => "Nothing yet: the walk's origin wasn't declared",
            });
            world.resource_mut::<survey::SurveyState>().last = Some(fit);
            ToolOutput::ok(v)
        }
        "site_survey_practice" => {
            let truth: BTreeMap<String, f32> = match i.get("truth").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(t) => t,
                    Err(e) => return Some(ToolOutput::err(format!("truth: {e}"))),
                },
                None => BTreeMap::new(),
            };
            let spots: Vec<[f32; 3]> = match i.get("spots").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(s) => s,
                    Err(e) => return Some(ToolOutput::err(format!("spots: {e}"))),
                },
                None => world.resource::<survey::SurveyState>().plan.as_ref().map_or(Vec::new(), |p| p.points.iter().map(|x| x.at).collect()),
            };
            if spots.is_empty() {
                return Some(ToolOutput::err("run site_survey_plan first, or give spots"));
            }
            let readings = match world.run_system_cached_with(survey::practice_system, (truth, spots, i["seed"].as_u64().unwrap_or(1))) {
                Ok(r) => r,
                Err(e) => return Some(ToolOutput::err(format!("{e}"))),
            };
            let Some(path) = i["path"].as_str() else { return Some(ToolOutput::err("give path, where to write the practice walk")) };
            let lines: Vec<String> = readings.iter().map(|r| serde_json::to_string(r).unwrap_or_default()).collect();
            if let Err(e) = std::fs::write(path, lines.join("\n")) {
                return Some(ToolOutput::err(format!("{path}: {e}")));
            }
            // So it's known for what it is when it comes back, whatever format it's in by then.
            let mut known = survey::PracticeWalks::load();
            known.record(&readings);
            if let Err(e) = known.save() {
                let _ = std::fs::remove_file(path);
                return Some(ToolOutput::err(format!("couldn't record this practice walk, so it wasn't written: {e:#}")));
            }
            ToolOutput::ok(json!({ "written": path, "readings": readings.len(), "note": "SYNTHETIC readings for practice, from a truth you chose. Not a measurement of any site." }))
        }
        "site_trace" => {
            let p = |k: &str| -> Option<Vec3> {
                let v = i[k].as_array()?;
                Some(Vec3::new(v.first()?.as_f64()? as f32, v.get(1)?.as_f64()? as f32, v.get(2)?.as_f64()? as f32))
            };
            let (Some(a), Some(b)) = (p("from"), p("to")) else { return Some(ToolOutput::err("give from and to, as [x, y, z]")) };
            match world.run_system_cached_with(survey::trace_system, (a, b)) {
                Ok(list) => ToolOutput::ok(json!({ "distance_m": a.distance(b), "crossed": list.iter().map(|(n, m, w, t)| json!({ "object": n, "material": m.name(), "walls_worth": w, "at_m": t })).collect::<Vec<_>>() })),
                Err(e) => ToolOutput::err(format!("{e}")),
            }
        }
        "site_survey_clear" => {
            survey::clear_applied();
            ToolOutput::text("Back to the built-in material values.")
        }
        "site_stress" => {
            let d = stress::StressOptions::default();
            let o = stress::StressOptions {
                seconds: i["seconds"].as_f64().unwrap_or(d.seconds).clamp(1.0, 300.0),
                target_m: i["target_cm"].as_f64().map_or(d.target_m, |c| c as f32 / 100.0),
                samples: i["samples"].as_u64().map_or(d.samples, |v| v as usize),
                anchors_down: i["anchors_down"].as_u64().map_or(d.anchors_down, |v| v as usize),
            };
            let scene = scene_of(world);
            let r = stress::run(world, o, &scene);
            if let Some(c) = r.claim.clone() {
                world.resource_mut::<Predictions>().put(c);
            }
            ToolOutput::ok(json!(r))
        }
        "site_drift_baseline" => {
            let mut q = world.query::<(&Device, &Transform)>();
            let map: BTreeMap<String, [f32; 3]> = q.iter(world).filter(|(d, _)| d.enabled && !d.kind.is_mobile() && d.kind.tech() == "uwb").map(|(d, t)| (d.id.clone(), t.translation.to_array())).collect();
            let n = map.len();
            world.resource_mut::<drift::DriftState>().baseline = map;
            ToolOutput::text(format!("Baseline recorded: {n} UWB anchors."))
        }
        "site_drift_check" => {
            let baseline = world.resource::<drift::DriftState>().baseline.clone();
            if baseline.is_empty() {
                return Some(ToolOutput::err("record a baseline first (site_drift_baseline) while the site matches its design"));
            }
            let samples = match i["path"].as_str() {
                Some(path) => match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| drift::parse_samples(&t)) {
                    Ok(s) => s,
                    Err(e) => return Some(ToolOutput::err(format!("{path}: {e:#}"))),
                },
                None => live_samples(world, i["seconds"].as_f64().unwrap_or(30.0).clamp(1.0, 600.0)),
            };
            if samples.is_empty() {
                return Some(ToolOutput::err("no two-way ranges came in, so nothing could be checked. This needs UWB tags using two-way ranging; TDoA tags report no ranges."));
            }
            let options = drift::DriftOptions { noise_m: i["noise_m"].as_f64().map_or(drift::DriftOptions::default().noise_m, |v| (v as f32).max(0.001)), ..Default::default() };
            let r = drift::analyse(&samples, &baseline, options);
            let v = drift_json(&r, i["path"].as_str(), &options);
            world.resource_mut::<drift::DriftState>().last = Some(r);
            ToolOutput::ok(v)
        }
        "site_material_ranges" => {
            let tech = i["tech"].as_str().and_then(Tech::parse).unwrap_or(Tech::Ble);
            let ranges: Vec<spec::MaterialRange> = match i.get("ranges").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(r) => r,
                    Err(e) => return Some(ToolOutput::err(format!("ranges: {e}"))),
                },
                None => spec::defaults(),
            };
            // For RFID the map holds the power reaching the tag, which wakes at its own threshold.
            let (wall_db, sensitivity) = world.resource::<crate::rtls::device::Profiles>().default_for(tech.infra()).map_or((6.0, -95.0), |p| (p.wall_loss_db, if tech == Tech::Rfid { p.tag_sensitivity_dbm } else { p.sensitivity_dbm }));
            // 6 dB over the receiver's limit: a signal somebody could actually use.
            let metric = if i["metric"].as_str() == Some("fix") { spec::SpecMetric::Fix } else { spec::SpecMetric::Reach { min_dbm: i["min_dbm"].as_f64().map_or(sensitivity + 6.0, |v| v as f32) } };
            let scene = scene_of(world);
            match spec::run(world, tech, &ranges, i["height"].as_f64().unwrap_or(1.2) as f32, i["cell"].as_f64().unwrap_or(1.0) as f32, i["min_coverage"].as_f64().unwrap_or(0.95), i["samples"].as_u64().unwrap_or(12) as usize, wall_db, metric, &scene) {
                Ok(r) => {
                    world.resource_mut::<Predictions>().put(r.claim.clone());
                    ToolOutput::ok(json!({ "decide_first": r.decide_first, "coverage": r.report.prediction, "verdict": r.claim.verdict_text(), "runs": r.claim.runs(), "by_material": r.report.swings, "skipped": r.skipped, "evidence": "Estimate: material losses are ranges, not the chosen products. Not validated on hardware" }))
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "site_set_material" => {
            let Some(object) = i["object"].as_str() else { return Some(ToolOutput::err("give the object's name")) };
            match i["material"].as_str() {
                None => {
                    radio::set_object_material(object, None);
                    ToolOutput::text(format!("{object}: back to the guess from its name."))
                }
                Some(m) => {
                    let Some(mat) = spec::material_named(m) else { return Some(ToolOutput::err(format!("unknown material '{m}': thin, wood, glass, object, concrete, metal, liquid"))) };
                    let mut q = world.query::<&PlacedObject>();
                    if !q.iter(world).any(|o| o.name == object) {
                        return Some(ToolOutput::err(format!("no object named '{object}'")));
                    }
                    let weight = i["weight"].as_f64().map_or(mat.weight(), |w| w as f32);
                    radio::set_object_material(object, Some((mat, weight)));
                    ToolOutput::text(format!("{object} is now {} for radio.", mat.name()))
                }
            }
        }
        "site_conditions" => {
            let mut c = radio::SiteConditions::current();
            if let Some(v) = i["rack_fill"].as_f64() {
                c.rack_fill = (v as f32).clamp(0.0, 1.0);
            }
            if let Some(v) = i["people_per_100m2"].as_f64() {
                c.people_per_100m2 = (v as f32).max(0.0);
            }
            radio::SiteConditions::set(c);
            ToolOutput::ok(json!({ "rack_fill": c.rack_fill, "people_per_100m2": c.people_per_100m2 }))
        }
        "site_set_antenna" => {
            let Some(id) = i["device"].as_str() else { return Some(ToolOutput::err("give the device id")) };
            let antenna = i["antenna"].as_str();
            if let Some(a) = antenna
                && world.resource::<crate::rtls::antenna::AntennaLibrary>().get(a).is_none()
            {
                let names: Vec<String> = world.resource::<crate::rtls::antenna::AntennaLibrary>().0.iter().map(|x| x.label.clone()).collect();
                return Some(ToolOutput::err(format!("no antenna '{a}' in the library; it has: {}", names.join(", "))));
            }
            let mut q = world.query::<&mut Device>();
            let Some(mut d) = q.iter_mut(world).find(|d| d.id == id) else { return Some(ToolOutput::err(format!("no device '{id}'"))) };
            if let Some(a) = antenna {
                d.radio.antenna = Some(AntennaChoice::Library { name: a.to_string() });
            }
            if let Some(w) = i["worn"].as_bool() {
                d.radio.worn = w;
            }
            ToolOutput::ok(json!({ "device": id, "radio": d.radio }))
        }
        "site_erces" => {
            let d = erces::Rules::default();
            let ranges = match i.get("ranges").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(r) => r,
                    Err(e) => return Some(ToolOutput::err(format!("ranges: {e}"))),
                },
                None => erces::default_ranges(),
            };
            let o = erces::ErcesOptions {
                floors: i["floors"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64().map(|x| x as f32)).collect()),
                cell: i["cell"].as_f64().unwrap_or(2.0).max(0.5) as f32,
                outside_dbm: i["outside_dbm"].as_f64().unwrap_or(-70.0) as f32,
                outside_sigma_db: i["outside_sigma_db"].as_f64().unwrap_or(4.0) as f32,
                rules: erces::Rules { threshold_dbm: i["threshold_dbm"].as_f64().map_or(d.threshold_dbm, |v| v as f32), general_share: i["general_share"].as_f64().unwrap_or(d.general_share), critical_share: i["critical_share"].as_f64().unwrap_or(d.critical_share) },
                critical_zones: strings(&i["critical_zones"]),
                ranges,
                samples: i["samples"].as_u64().map_or(30, |v| v as usize),
                wall_db: 6.0,
            };
            let scene = scene_of(world);
            match erces::run(world, o, &scene) {
                Ok(r) => {
                    world.resource_mut::<Predictions>().put(r.claim.clone());
                    let mut v = json!(r);
                    if i["outside_dbm"].is_null() {
                        v["note"] = json!("The outdoor signal is a placeholder: give outside_dbm from a drive test.");
                    }
                    ToolOutput::ok(v)
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "site_compare_tech" => {
            let (prices, prices_given) = match i.get("prices").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value::<compare::Prices>(v.clone()) {
                    Ok(p) => {
                        // Any price left out is still a placeholder.
                        let every = serde_json::to_value(compare::Prices::default()).ok().and_then(|d| d.as_object().map(|o| o.keys().all(|k| v.get(k).is_some()))).unwrap_or(false);
                        (p, every)
                    }
                    Err(e) => return Some(ToolOutput::err(format!("prices: {e}"))),
                },
                None => (compare::Prices::default(), false),
            };
            let mut techs = strings(&i["techs"]);
            if techs.is_empty() {
                techs = ["uwb", "ble", "ble_aoa", "wifi", "rfid"].map(String::from).to_vec();
            }
            if let Some(bad) = techs.iter().find(|t| !["uwb", "ble", "ble_aoa", "wifi", "rfid"].contains(&t.as_str())) {
                return Some(ToolOutput::err(format!("unknown technology '{bad}': uwb, ble, ble_aoa, wifi, rfid")));
            }
            let o = compare::CompareOptions { techs, target_m: i["target_cm"].as_f64().map_or(0.5, |c| c as f32 / 100.0), tags: i["tags"].as_u64().map_or(100, |v| v as usize), years: i["years"].as_f64().unwrap_or(5.0).max(0.1), prices, prices_given, test_s: i["test_s"].as_f64().unwrap_or(3.0).clamp(1.0, 120.0), points: i["points"].as_u64().map_or(30, |v| (v as usize).clamp(5, 200)), settle_s: i["settle_s"].as_f64().unwrap_or(2.0).clamp(0.2, 30.0), moving_share: i["moving_share"].as_f64().unwrap_or(0.2).clamp(0.0, 1.0), report_hz: i["update_hz"].as_f64().unwrap_or(1.0).max(0.001) };
            let scene = scene_of(world);
            let r = compare::run(world, &o, &scene);
            if let Some(c) = r.claim.clone() {
                world.resource_mut::<Predictions>().put(c);
            }
            ToolOutput::ok(json!(r))
        }
        "site_acceptance_plan" => {
            let share = i["pass_share"].as_f64().unwrap_or(0.95);
            let confidence = i["confidence"].as_f64().unwrap_or(0.95);
            if !(0.0..1.0).contains(&share) || !(0.0..1.0).contains(&confidence) {
                return Some(ToolOutput::err("pass_share and confidence are shares between 0 and 1"));
            }
            let Some(target_cm) = i["target_cm"].as_f64().filter(|v| *v > 0.0) else { return Some(ToolOutput::err("give target_cm, the accuracy both sides agreed")) };
            let seed = i["seed"].as_u64().unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs()));
            let needed = acceptance::points_needed(share, confidence);
            let n = i["points"].as_u64().map_or(needed, |v| v as usize);
            let tech = i["tech"].as_str().unwrap_or("uwb");
            if !["uwb", "ble", "wifi"].contains(&tech) {
                return Some(ToolOutput::err(format!("tech '{tech}': uwb, ble or wifi")));
            }
            let points = match acceptance::draw(world, n, i["height"].as_f64().unwrap_or(1.2) as f32, i["step_m"].as_f64().unwrap_or(2.0).max(0.25) as f32, seed) {
                Ok(p) => p,
                Err(e) => return Some(ToolOutput::err(e)),
            };
            if points.is_empty() {
                return Some(ToolOutput::err("no free floor to put test points on"));
            }
            let plan = acceptance::AcceptancePlan { scene: scene_of(world), tech: tech.to_string(), target_m: target_cm as f32 / 100.0, pass_share: share, confidence, points, seed };
            if let Some(path) = i["path"].as_str()
                && let Err(e) = serde_json::to_string_pretty(&plan).map_err(anyhow::Error::from).and_then(|s| std::fs::write(path, s).map_err(anyhow::Error::from))
            {
                return Some(ToolOutput::err(format!("{path}: {e:#}")));
            }
            let got = plan.points.len();
            let mut v = json!(plan);
            v["fingerprint"] = json!(plan.fingerprint());
            v["points_needed"] = json!(needed);
            v["note"] = if got < needed { json!(format!("Only {got} points: fewer than the {needed} it takes to show {:.0} % at {:.0} % confidence, so it can't pass as agreed", share * 100.0, confidence * 100.0)) } else { json!(format!("{needed} points all passing shows {:.0} % at {:.0} % confidence. Agree this before testing", share * 100.0, confidence * 100.0)) };
            world.resource_mut::<acceptance::AcceptanceState>().plan = Some(plan);
            ToolOutput::ok(v)
        }
        "site_acceptance_preview" => {
            let Some(plan) = world.resource::<acceptance::AcceptanceState>().plan.clone() else { return Some(ToolOutput::err("make a plan first: site_acceptance_plan")) };
            let scene = scene_of(world);
            if plan.scene != scene {
                return Some(ToolOutput::err(format!("the plan in force is for '{}', and this is '{scene}': make a new one", plan.scene)));
            }
            let results = match acceptance::preview(world, &plan, i["settle_s"].as_f64().unwrap_or(2.0).clamp(0.2, 30.0)) {
                Ok(r) => r,
                Err(e) => return Some(ToolOutput::err(e)),
            };
            let s = acceptance::score(&plan, &results, None);
            if let Some(c) = acceptance::claim_of(&plan, &s) {
                world.resource_mut::<Predictions>().put(c);
            }
            if let Some(path) = i["path"].as_str()
                && let Err(e) = std::fs::write(path, acceptance::report_html(&plan, &s))
            {
                return Some(ToolOutput::err(format!("{path}: {e}")));
            }
            ToolOutput::ok(json!(s))
        }
        "site_acceptance_score" => {
            let plan = match i["plan_path"].as_str() {
                Some(path) => match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| serde_json::from_str::<acceptance::AcceptancePlan>(&t).map_err(anyhow::Error::from)) {
                    Ok(p) => p,
                    Err(e) => return Some(ToolOutput::err(format!("{path}: {e:#}"))),
                },
                None => match world.resource::<acceptance::AcceptanceState>().plan.clone() {
                    Some(p) => p,
                    None => return Some(ToolOutput::err("make a plan first (site_acceptance_plan), or give plan_path")),
                },
            };
            let results = if let Some(path) = i["path"].as_str() {
                match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| acceptance::parse_results(&t)) {
                    Ok(r) => r,
                    Err(e) => return Some(ToolOutput::err(format!("{path}: {e:#}"))),
                }
            } else {
                match serde_json::from_value::<Vec<acceptance::PointResult>>(i["results"].clone()) {
                    Ok(r) => r,
                    Err(_) => return Some(ToolOutput::err("give path (a file) or results ([{id, reported:[x,y,z]} or {id, error_m}])")),
                }
            };
            let s = acceptance::score(&plan, &results, i["source"].as_str());
            if let Some(c) = acceptance::claim_of(&plan, &s) {
                let p95 = c.prediction.median;
                let mut ledger = world.resource_mut::<Predictions>();
                ledger.put(c);
                if !s.simulated
                    && let Some(source) = s.source.as_deref()
                    && let Some(forecast) = ledger.ledger.get_mut(&format!("site_p95:{}", plan.scene))
                {
                    // Once per plan, however often it's scored. A test with too many points
                    // unlocated has no percentile, and is still the worst miss there is.
                    let note = format!("Acceptance test, plan {}", plan.fingerprint());
                    if !forecast.checks.iter().any(|k| k.note == note) {
                        forecast.check_from(if p95.is_finite() { p95 } else { f64::INFINITY }, &note, source);
                    }
                    ledger.save();
                }
            }
            if let Some(path) = i["report_path"].as_str()
                && let Err(e) = std::fs::write(path, acceptance::report_html(&plan, &s))
            {
                return Some(ToolOutput::err(format!("{path}: {e}")));
            }
            let mut v = json!(s);
            v["evidence"] = json!(match (s.simulated, &s.source) {
                (true, _) => "Simulated: a forecast of the test, never a pass",
                (false, Some(_)) => "Measured, on the word of the source given: nothing here can check that",
                (false, None) => "Unattributed: nobody has said who measured these, so they count for nothing",
            });
            ToolOutput::ok(v)
        }
        "site_anchor_fit" => {
            let ranges = if let Some(path) = i["path"].as_str() {
                match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| anchors::parse(&t)) {
                    Ok(r) => r,
                    Err(e) => return Some(ToolOutput::err(format!("{path}: {e:#}"))),
                }
            } else {
                match serde_json::from_value::<Vec<anchors::AnchorRange>>(i["ranges"].clone()) {
                    Ok(r) => r,
                    Err(_) => return Some(ToolOutput::err("give path (a file) or ranges ([{a, b, range_m}])")),
                }
            };
            if let Err(e) = anchors::check_ranges(&ranges) {
                return Some(ToolOutput::err(format!("{e:#}")));
            }
            let synthetic = ranges.iter().any(|r| r.synthetic);
            let prior = i["prior_m"].as_f64().unwrap_or(2.0).max(0.01);
            let surveyed = strings(&i["surveyed"]);
            let (names, design, links, unknown) = match world.run_system_cached_with(anchors::links_system, (ranges.clone(), true, None)) {
                Ok(v) => v,
                Err(e) => return Some(ToolOutput::err(format!("{e}"))),
            };
            if let Some(bad) = surveyed.iter().find(|s| !names.contains(s)) {
                return Some(ToolOutput::err(format!("no anchor '{bad}' to count as surveyed")));
            }
            let mut fit = match anchors::fit(&names, &design, &links, prior, &surveyed) {
                Ok(f) => f,
                Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
            };
            // What a link crosses depends on where its anchors are, so look again from there.
            for _ in 0..2 {
                let at: Vec<[f32; 3]> = fit.anchors.iter().map(|a| a.fitted).collect();
                let Ok((_, _, again, _)) = world.run_system_cached_with(anchors::links_system, (ranges.clone(), true, Some(at))) else { break };
                match anchors::fit(&names, &design, &again, prior, &surveyed) {
                    Ok(f) => fit = f,
                    Err(e) => return Some(ToolOutput::err(format!("{e:#}"))),
                }
            }
            fit.synthetic = synthetic;
            fit.unknown_devices = unknown;
            let at: Vec<[f32; 3]> = fit.anchors.iter().map(|a| a.fitted).collect();
            let check = world.run_system_cached_with(anchors::links_system, (ranges.clone(), true, Some(at))).ok().and_then(|(_, _, l, _)| anchors::held_out(&names, &design, &l, prior, &surveyed));
            let plain = world.run_system_cached_with(anchors::links_system, (ranges, false, None)).ok().and_then(|(_, _, l, _)| anchors::fit(&names, &design, &l, prior, &surveyed).ok());
            let apply = i["apply"].as_bool().unwrap_or(false);
            let source = i["source"].as_str().map(str::trim).filter(|s| !s.is_empty());
            if apply && synthetic {
                return Some(ToolOutput::err("this log is synthetic, from site_anchor_practice, so it can't move anchors in the scene"));
            }
            if apply && source.is_none() {
                return Some(ToolOutput::err("not applied: nothing says where these ranges came from. Give source, who logged them and how"));
            }
            if apply && !fit.converged {
                return Some(ToolOutput::err("not applied: the fit stopped before it settled, so these positions are unfinished"));
            }
            // Only if it predicts pairs it never saw better than the design does.
            if apply && !check.is_some_and(|(before, after)| anchors::worth_applying(before, after)) {
                return Some(ToolOutput::err(format!("not applied: on pairs held out of the fit, the mean error goes {}, so these ranges don't support moving anything", check.map_or("unchecked, as too few pairs remain".to_string(), |(b, a)| format!("from {:.0} cm to {:.0} cm", b * 100.0, a * 100.0)))));
            }
            if apply {
                let mut q = world.query::<(&Device, &mut Transform)>();
                for (d, mut t) in q.iter_mut(world) {
                    if let Some(a) = fit.anchors.iter().find(|a| a.id == d.id && !fit.not_pinned_down.contains(&a.id)) {
                        t.translation = Vec3::from_array(a.fitted);
                    }
                }
            }
            let mut v = json!(fit);
            v["applied_to_scene"] = json!(apply);
            v["held_out_mean_error_m"] = json!(check.map(|(before, after)| json!({ "design": before, "fitted": after })));
            // Calibrated only if it settled, said where the ranges came from and did better on
            // pairs it never saw. The ledger keeps what it stands on, and practice logs stay out.
            let earned = fit.converged && source.is_some() && check.is_some_and(|(before, after)| anchors::worth_applying(before, after));
            let evidence = if earned { proof::Evidence::Calibrated } else { proof::Evidence::Estimate };
            v["evidence"] = json!(match (synthetic, earned, source) {
                (true, _, _) => "Synthetic: practice ranges, not a measurement".to_string(),
                (_, true, Some(s)) => format!("Calibrated to ranges from {s}, on that word: nothing here checks how they were logged. Not validated on hardware"),
                _ => "Estimate: it needs a source, a fit that settled and a better score on pairs it never saw to count as more. Not validated on hardware".to_string(),
            });
            if !synthetic && let Some((_, after)) = check {
                let scene = scene_of(world);
                let assumptions = vec![format!("Ranges logged by: {}", source.unwrap_or("not stated")), if surveyed.len() < 2 { "Fixes the shape of the set, not where it sits: too few surveyed anchors tie it to the building".to_string() } else { format!("Tied to the building by {} surveyed anchors", surveyed.len()) }];
                world.resource_mut::<Predictions>().put(hardware::exact_claim(&format!("anchor_fit:{scene}"), "anchor_fit", &format!("Anchor positions from ranges between them, {scene}"), proof::Prediction::exact("Mean error on pairs held out of the fit", "m", after, evidence), None, assumptions));
            }
            v["ignoring_what_blocks_each_link"] = json!(plain.map(|p| p.anchors.iter().map(|a| json!({ "id": a.id, "fitted": a.fitted, "moved_m": a.moved_m })).collect::<Vec<_>>()));
            let mut notes = Vec::new();
            if synthetic {
                notes.push("SYNTHETIC ranges for practice. Not a measurement of any site.");
            }
            if !fit.converged {
                notes.push("The fit stopped before it settled, so treat these positions as unfinished.");
            }
            if surveyed.len() < 2 {
                notes.push("Ranges between anchors fix their shape, not where the whole set sits: survey two or more anchors and name them in surveyed to tie it to the building. sigma_m is against the other anchors; absolute_sigma_m is in the building.");
            }
            v["note"] = json!(notes.join(" "));
            ToolOutput::ok(v)
        }
        "site_anchor_practice" => {
            let Some(path) = i["path"].as_str() else { return Some(ToolOutput::err("give path, where to write the practice log")) };
            let moves: BTreeMap<String, [f32; 2]> = match i.get("moves").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(m) => m,
                    Err(e) => return Some(ToolOutput::err(format!("moves: {e}"))),
                },
                None => BTreeMap::new(),
            };
            let ranges = match world.run_system_cached_with(anchors::practice_system, (moves, i["rounds"].as_u64().map_or(3, |v| v as usize), i["seed"].as_u64().unwrap_or(1))) {
                Ok(r) => r,
                Err(e) => return Some(ToolOutput::err(format!("{e}"))),
            };
            if ranges.is_empty() {
                return Some(ToolOutput::err("needs at least two UWB anchors in the scene"));
            }
            let lines: Vec<String> = ranges.iter().map(|r| serde_json::to_string(r).unwrap_or_default()).collect();
            if let Err(e) = std::fs::write(path, lines.join("\n")) {
                return Some(ToolOutput::err(format!("{path}: {e}")));
            }
            ToolOutput::ok(json!({ "written": path, "ranges": ranges.len(), "note": "SYNTHETIC ranges for practice, from moves you chose. Not a measurement of any site." }))
        }
        "site_battery" => {
            let mut rates: Vec<f64> = i["rates"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(Value::as_f64).filter(|v| *v > 0.0).collect());
            if rates.is_empty() {
                rates = vec![0.5, 1.0, 2.0, 5.0, 10.0];
            }
            if rates.iter().any(|r| *r < 0.1) {
                return Some(ToolOutput::err("the simulator doesn't update slower than 0.1 Hz, so it can't judge a slower rate"));
            }
            let o = battery::BatteryOptions { profile: i["profile"].as_str().map(str::to_string), rates, moving_share: i["moving_share"].as_f64().unwrap_or(0.2).clamp(0.0, 1.0), still_hz: i["still_hz"].as_f64().unwrap_or(0.1).max(0.0), target_m: i["target_cm"].as_f64().map_or(0.5, |c| c as f32 / 100.0), target_years: i["target_years"].as_f64().unwrap_or(3.0), speed_mps: i["speed_mps"].as_f64().unwrap_or(1.3).max(0.0), seconds: i["seconds"].as_f64().unwrap_or(10.0).clamp(1.0, 120.0), capacity_mah: i["capacity_mah"].as_f64(), update_uc: i["update_uc"].as_f64() };
            let scene = scene_of(world);
            match battery::run(world, o, &scene) {
                Ok(r) => {
                    if let Some(c) = r.claim.clone() {
                        world.resource_mut::<Predictions>().put(c);
                    }
                    let mut v = json!(r);
                    v["evidence"] = json!("Simulated for accuracy, one run per rate. Battery life is an estimate from typical figures. Not validated on hardware");
                    ToolOutput::ok(v)
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "site_esl" => {
            let scene = scene_of(world);
            let r = esl::run(world, i["place"].as_u64().map_or(0, |v| v as usize), i["apply"].as_bool().unwrap_or(false), i["labels"].as_u64().map(|v| v as usize), i["updates_per_s"].as_f64().unwrap_or(2.0).max(0.01) as f32, i["channels"].as_u64().map_or(3, |v| v.max(1) as usize), i["min_reach"].as_f64().unwrap_or(0.99), i["samples"].as_u64().map_or(30, |v| v as usize), &scene);
            match r {
                Ok(r) => {
                    world.resource_mut::<Predictions>().put(r.claim.clone());
                    ToolOutput::ok(json!(r))
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "design_tunnel" => {
            let belt = match i["conveyor"].as_str() {
                Some(name) => {
                    // Everything else keeps its speed in rad/s, which would read as a very fast belt.
                    let is_conveyor = world.query::<&PlacedObject>().iter(world).any(|o| o.name == name && o.asset.to_string_lossy().to_ascii_lowercase().contains("conveyor"));
                    let control = world.resource::<crate::plant::sim::Plant>().controls.get(name).cloned();
                    match control.filter(|c| is_conveyor && c.on) {
                        Some(c) => Some(c.speed_mps.unwrap_or(0.4) as f64),
                        None => return Some(ToolOutput::err(format!("'{name}' isn't a conveyor that's switched on: switch it on, or give belt_mps"))),
                    }
                }
                None => None,
            };
            let r = hardware::tunnel_advice(i, belt);
            instant(world, r)
        }
        "site_route_coverage" => {
            let tech = i["tech"].as_str().map_or(Some(Tech::Wifi), Tech::parse).filter(|t| matches!(t, Tech::Wifi | Tech::Ble));
            let Some(tech) = tech else { return Some(ToolOutput::err("tech is wifi or ble")) };
            let points: Vec<Vec3> = match i.get("points").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value::<Vec<[f32; 3]>>(v.clone()) {
                    Ok(p) => p.into_iter().map(Vec3::from_array).collect(),
                    Err(e) => return Some(ToolOutput::err(format!("points: {e}"))),
                },
                None => {
                    let wanted = i["device"].as_str();
                    let mut q = world.query::<&Device>();
                    let path = q.iter(world).filter(|d| wanted.is_none_or(|w| d.id == w)).find_map(|d| match &d.motion {
                        crate::rtls::device::Motion::Path { points, .. } => Some(points.clone()),
                        _ => None,
                    });
                    match path {
                        Some(p) => p.into_iter().map(Vec3::from_array).collect(),
                        None => return Some(ToolOutput::err("give points, or a device that follows a path")),
                    }
                }
            };
            let rules = route::RouteRules { threshold_dbm: i["threshold_dbm"].as_f64().unwrap_or(-70.0) as f32, hysteresis_db: i["hysteresis_db"].as_f64().unwrap_or(6.0) as f32, speed_mps: i["speed_mps"].as_f64().unwrap_or(1.5) as f32, handoff_s: i["handoff_ms"].as_f64().unwrap_or(100.0) as f32 / 1000.0, need_mbps: i["need_mbps"].as_f64().unwrap_or(1.0) as f32, noise_dbm: -90.0, channel_mhz: if tech == Tech::Ble { 2.0 } else { 20.0 } };
            let scene = scene_of(world);
            match route::run(world, &points, tech, rules, i["max_gap_m"].as_f64().unwrap_or(1.0) as f32, i["samples"].as_u64().map_or(30, |v| v as usize), &scene) {
                Ok(r) => {
                    world.resource_mut::<Predictions>().put(r.claim.clone());
                    ToolOutput::ok(json!(r))
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "design_medical_coexist" => {
            let from_scene = match i.get("position").filter(|v| !v.is_null()) {
                Some(v) => match serde_json::from_value::<[f32; 3]>(v.clone()) {
                    Ok(at) => match world.run_system_cached_with(medical_neighbours, Vec3::from_array(at)) {
                        Ok(n) => n,
                        Err(e) => return Some(ToolOutput::err(format!("{e}"))),
                    },
                    Err(e) => return Some(ToolOutput::err(format!("position: {e}"))),
                },
                None => Vec::new(),
            };
            let r = hardware::medical_advice(i, from_scene);
            instant(world, r)
        }
        "site_proximity" => {
            let v = proximity::Vehicle { speed_mps: i["speed_mps"].as_f64().unwrap_or(2.5).max(0.1) as f32, decel_mps2: i["decel_mps2"].as_f64().unwrap_or(1.5).max(0.1) as f32, reaction_s: i["reaction_s"].as_f64().unwrap_or(1.0).max(0.0) as f32, walker_mps: i["walker_mps"].as_f64().unwrap_or(1.4).max(0.1) as f32, workers: i["workers"].as_u64().map_or(10, |w| w as usize), update_hz: i["update_hz"].as_f64().unwrap_or(10.0).max(0.1) as f32, slot_s: i["slot_ms"].as_f64().unwrap_or(2.0) as f32 / 1000.0, margin_m: i["margin_m"].as_f64().unwrap_or(0.5).max(0.0) as f32 };
            let scene = scene_of(world);
            match proximity::run(world, v, i["encounters"].as_u64().map_or(400, |n| n.max(1) as usize), i["min_detect"].as_f64().unwrap_or(0.99), &scene) {
                Ok(r) => {
                    world.resource_mut::<Predictions>().put(r.claim.clone());
                    ToolOutput::ok(json!(r))
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "design_sar" => start_job(world, "SAR pre-check", hardware::sar_job(i)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // OUT-B11: healthy: true could be the simulator checked against itself.
    #[test]
    fn a_check_on_the_simulator_is_a_drill_and_never_says_healthy() {
        let anchors: BTreeMap<String, [f32; 3]> = [("A1".to_string(), [0.0, 3.0, 0.0])].into_iter().collect();
        let o = drift::DriftOptions::default();
        let samples: Vec<drift::Sample> = (0..40).map(|i| {
            let tag = [(i * 7 % 20) as f32, 1.0, (i * 13 % 20) as f32];
            let d = ((tag[0]).powi(2) + (tag[1] - 3.0).powi(2) + (tag[2]).powi(2)).sqrt();
            drift::Sample { device: "A1".into(), tag, range_m: d, expected_excess_m: 0.0 }
        }).collect();
        let r = drift::analyse(&samples, &anchors, o);
        assert!(r.healthy, "{r:?}");
        let drill = drift_json(&r, None, &o);
        assert!(drill.get("healthy").is_none(), "{drill}");
        assert_eq!((drill["source"].as_str(), drill["mode"].as_str()), (Some("simulation"), Some("drill")));
        assert!(drill["evidence"].as_str().unwrap().starts_with("Simulated"));
        let real = drift_json(&r, Some("ranges.jsonl"), &o);
        assert_eq!(real["healthy"], json!(true));
        assert_eq!(real["source"], json!("file"));
        assert!(real["false_alarms"].is_string());
    }

    // OUT-M1
    #[test]
    fn every_tool_result_says_what_its_numbers_rest_on() {
        for d in defs() {
            let label = evidence_of(&d.name);
            assert!(!label.is_some_and(|l| l.starts_with("Not stated")), "{} has no evidence label: add it to evidence_of", d.name);
            if label.is_some() {
                let out = stamped(&d.name, ToolOutput::ok(json!({ "number": 1.0 })));
                let v: Value = serde_json::from_str(&out.summary()).unwrap();
                assert!(v["evidence"].is_string(), "{}: {v}", d.name);
            }
        }
        // A claim in the result knows better than the tool's label.
        let claim = proof::Claim { id: "a".into(), recipe: "r".into(), subject: "s".into(), prediction: proof::Prediction::exact("m", "u", 1.0, proof::Evidence::Calibrated), target: None, pass_probability: None, verdict: proof::Verdict::NoTarget, drivers: vec![], assumptions: vec![], checks: vec![], created: 0 };
        let out = stamped("site_stress", ToolOutput::ok(json!({ "claim": claim })));
        assert!(out.summary().contains("Calibrated to measurements: not validated"), "{}", out.summary());
        // What a result says for itself stands, and errors and plain text pass through.
        let own = stamped("design_tag", ToolOutput::ok(json!({ "evidence": "mine" })));
        assert!(own.summary().contains("\"mine\""));
        assert!(stamped("design_tag", ToolOutput::err("no")).is_error);
        assert_eq!(stamped("site_stress", ToolOutput::text("hello")).summary(), "hello");
    }
}
