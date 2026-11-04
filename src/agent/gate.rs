//! Decides whether a tool call may run. Every call passes here: the assistant's own, a macro's
//! steps, and calls from MCP clients. The model only supplies arguments, so the text a person
//! is asked to approve is written here, never by the model.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::harness::TurnLog;

/// How much the assistant may do without asking. Measurements and outside systems always ask.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Anything that changes something asks.
    Ask,
    /// Edits to the scene and project run; files elsewhere and external tools ask.
    #[default]
    Edits,
    /// Only measurements and outside systems ask.
    All,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Ask, Mode::Edits, Mode::All];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Ask => "Ask before any change",
            Mode::Edits => "Edits run, riskier actions ask",
            Mode::All => "Only measurements and outside systems ask",
        }
    }
}

/// Where a call came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Origin {
    /// The assistant's model.
    #[default]
    Assistant,
    /// A client of the MCP server: no login, and nobody watching to approve.
    Mcp,
    /// The harness itself, checking the model's work. It decides these calls, not the model.
    Harness,
    /// A script, through `sensesim.call`. Runs unattended, on whatever it was told to do.
    Script,
}

/// What travels with a call, and with any calls it makes in turn.
#[derive(Clone, Default)]
pub struct CallCtx {
    pub origin: Origin,
    pub log: Option<TurnLog>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Plan mode: while set, nothing may change. Shared, so approving lifts it mid-run.
    pub planning: Option<Arc<AtomicBool>>,
}

impl CallCtx {
    pub fn harness(cancel: Arc<AtomicBool>) -> Self {
        Self { origin: Origin::Harness, log: None, cancel: Some(cancel), planning: None }
    }

    pub fn planning(&self) -> bool {
        self.planning.as_ref().is_some_and(|p| p.load(Ordering::Relaxed))
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

/// What a tool does, for deciding who has to agree to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Looks at things. Includes `ask_user` and `update_plan`, which only talk to the user.
    Read,
    /// Changes what is in the scene.
    Scene,
    /// Moves the camera or captures the view. Harmless, but two callers would disturb each other.
    View,
    /// Changes other project state: designs, studies, runs, saved tools, predictions.
    Project,
    /// Writes or reads a file outside the project and the workspace.
    Files,
    /// Shows something in another app on this computer.
    OpensOutside,
    /// Aims the app at a real system outside it.
    Outside,
    /// Records something as a real measurement.
    Measurement,
    /// A tool from an external MCP server: it could do anything.
    External,
    /// A macro. Runs freely: each of its steps comes back through the gate.
    Macro,
    /// Nobody has said what this does, so nobody may assume.
    Unknown,
}

/// Tools that exist only inside the assistant's own loop. The MCP port has no login, and a macro
/// runs unattended, so neither may reach them.
pub fn agent_only(name: &str) -> bool {
    super::workspace::is_tool(name) || matches!(name, "python_run" | "delegate" | "open_layout_tab" | "propose_plan")
}

/// What a tool does, ignoring where it writes. Input matters for a few that do different things.
pub fn class_of(name: &str, input: &Value) -> Class {
    use Class::*;
    if name.starts_with(super::toolmaker::PREFIX) {
        return Macro;
    }
    match name {
        // Control and lookups.
        "ask_user" | "update_plan" | "get_screen" | "propose_plan" | "read_file" | "list_files" | "open_file" | "list_asset_categories" | "search_assets" | "get_asset_info" | "get_scene" | "verify_scene" | "check_against_depth" | "perceive_photo" | "load_reference_image"
        | "search_knowledge" | "read_knowledge" | "list_knowledge" | "sim_faces" | "design_job" | "sim_catalog" | "sim_studies" | "sim_jobs" | "sim_check_faces"
        | "sim_results" | "sim_probe" | "plant_list_ports" | "plant_status" | "outcomes_ledger" | "site_material_ranges" | "rtls_list_regions" | "rtls_list_profiles" | "rtls_list_devices" | "rtls_status"
        | "rtls_coverage" | "antenna_assess" | "antenna_calc" | "antenna_sweep_results" | "antenna_converge_results" | "antenna_yield_results" | "antenna_gate_results" | "antenna_list_samples"
        | "antenna_references" | "antenna_list_templates" | "antenna_get" | "antenna_model_get" | "antenna_results" | "antenna_job_state" | "antenna_wait" | "antenna_reconfig_parts" | "antenna_reconfig_results" | "antenna_reconfig_found"
        | "antenna_reconfig_tuning" | "antenna_reconfig_spread" | "antenna_phased_results" | "antenna_phased_table" | "antenna_df_results" | "antenna_df_fix" | "antenna_df_switched" | "antenna_chain_budget" | "antenna_chain_results" | "antenna_radar_results" | "antenna_tag_options" | "antenna_sketch_view" => Read,

        // Reading an optimizer's progress is a lookup; applying its result changes the design.
        // Reading a tag's two metrics changes nothing. Tuning replaces the last result, and applying moves the design.
        "antenna_tag_tune" => {
            if input["only_read"].as_bool().unwrap_or(false) && !input["apply"].as_bool().unwrap_or(false) {
                Read
            } else {
                Project
            }
        }

        "antenna_optimize_status" => {
            if input["apply"].as_bool().unwrap_or(false) {
                Project
            } else {
                Read
            }
        }

        "place_objects" | "update_object" | "delete_objects" | "clear_scene" | "load_template" | "add_photo_backdrop" | "place_from_photo" | "optimize_layout" | "add_structure" | "set_object_material"
        | "plant_add_part" | "plant_route_pipe" | "plant_connect" | "plant_disconnect" | "plant_build" | "plant_set" | "rtls_auto_deploy" | "rtls_smart_plan" | "rtls_add_device" | "rtls_update_device"
        | "rtls_remove_device" | "rtls_add_zone" | "rtls_optimize_anchors" => Scene,

        "set_camera" | "frame_all" | "screenshot" | "compare_to_photo" | "antenna_reconfig_view" => View,

        "python_run" | "write_file" | "open_layout_tab" | "download_assets" | "set_photo_layout" | "create_asset" | "define_tool" | "delete_tool" | "save_layout" | "export_usda" | "sim_study_create" | "sim_study_set" | "sim_run" | "sim_cancel"
        | "plant_run" | "plant_define_port" | "design_uwb_bias" | "design_shelf" | "design_aoa" | "design_integrate" | "design_tag" | "design_coexist" | "design_field_yield" | "design_tunnel"
        | "design_medical_coexist" | "design_sar" | "site_survey_plan" | "site_trace" | "site_guess_materials" | "site_survey_clear" | "site_survey_practice" | "site_stress" | "site_set_material"
        | "site_conditions" | "site_erces" | "site_compare_tech" | "site_acceptance_plan" | "site_acceptance_preview" | "site_anchor_practice" | "site_battery" | "site_esl" | "site_route_coverage"
        | "site_proximity" | "site_set_antenna" | "site_import_bim" | "rtls_set_region" | "rtls_run" | "rtls_control" | "antenna_spec" | "antenna_sweep" | "antenna_converge" | "antenna_match" | "antenna_yield"
        | "antenna_array" | "antenna_save_pattern" | "antenna_import_cad" | "antenna_states" | "antenna_gate_test" | "antenna_find_phases" | "antenna_open_sample" | "antenna_new" | "antenna_set" | "antenna_run"
        | "antenna_optimize" | "antenna_cancel" | "antenna_reconfig_set" | "antenna_reconfig_solve" | "antenna_reconfig_search" | "antenna_phased_set" | "antenna_phased_shape" | "antenna_df_set" | "antenna_df_study" | "antenna_chain_set" | "antenna_chain_run" | "antenna_radar_set" | "antenna_radar_run" | "antenna_stack" | "antenna_draw" | "antenna_model_add" | "antenna_model_clear" | "antenna_tag_try" | "antenna_tag_design" | "antenna_tag_open" | "antenna_tag_retune" => Project,

        "open_external" => OpensOutside,

        // Where results go: webhooks, Modbus, UDP, writes to a real energy system.
        "rtls_configure_outputs" => Outside,
        "rtls_openems" => {
            if input["action"] == "status" {
                Read
            } else {
                Outside
            }
        }

        // A number recorded here counts as measured from then on.
        "outcomes_check" | "site_survey_fit" | "site_anchor_fit" | "site_acceptance_score" | "site_drift_baseline" | "site_drift_check" | "design_fit_measured" => Measurement,

        _ => Unknown,
    }
}

/// The file a tool would write, if it was given one.
fn written_path<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    // A .json path saves beside itself as .sesim, in the same folder, so the check still holds.
    let key = match name {
        "save_layout" | "export_usda" | "site_survey_practice" | "site_anchor_practice" | "site_acceptance_plan" | "site_acceptance_preview" => "path",
        "site_acceptance_score" => "report_path",
        _ => return None,
    };
    input[key].as_str().filter(|s| !s.is_empty())
}

/// The file a tool would read into the chat.
fn read_path<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    matches!(name, "read_file" | "list_files" | "open_file").then(|| input["path"].as_str()).flatten().filter(|s| !s.is_empty())
}

/// Where this app's own working files go.
pub fn workspace_root() -> PathBuf {
    let base = crate::app_paths::data_base();
    base.join("SenseSim").join("agent")
}

/// Whether `path` is inside `root`, spelling aside. A path this can't be sure about isn't inside.
pub fn is_under(path: &str, root: &Path) -> bool {
    let plain = |p: &Path| -> Option<String> {
        let abs = std::path::absolute(p).ok()?;
        // Windows resolves `..` above; a verbatim path (`\\?\`) keeps it, and means what it says.
        if abs.components().any(|c| c == Component::ParentDir) || abs.to_string_lossy().starts_with(r"\\?\") {
            return None;
        }
        Some(abs.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase())
    };
    match (plain(Path::new(path)), plain(root)) {
        (Some(p), Some(r)) => p == r || p.starts_with(&format!("{r}\\")),
        _ => false,
    }
}

/// A question for the user, and what they can make it stop asking.
#[derive(Debug, PartialEq, Eq)]
pub struct Approval {
    pub text: String,
    /// (button label, key in the chat's approved set).
    pub remember: Option<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Run,
    Ask(Approval),
    Refuse(String),
}

/// What a decision depends on besides the call.
pub struct Policy<'a> {
    pub mode: Mode,
    /// What the user has allowed for this chat.
    pub approved: &'a HashSet<String>,
    /// Folders that count as the project.
    pub roots: &'a [PathBuf],
    /// The tool belongs to an external MCP server.
    pub external: bool,
}

const REMEMBER_EDITS: &str = "edits";

fn details(input: &Value) -> String {
    let text = serde_json::to_string_pretty(input).unwrap_or_default();
    if text.chars().count() > 1200 {
        format!("{}\n…", text.chars().take(1200).collect::<String>())
    } else {
        text
    }
}

pub fn decide(name: &str, input: &Value, ctx: &CallCtx, policy: &Policy) -> Verdict {
    if ctx.origin == Origin::Harness {
        return Verdict::Run;
    }
    if ctx.origin == Origin::Mcp && agent_only(name) {
        return Verdict::Refuse(format!("{name} is only for the assistant in the app."));
    }
    if ctx.origin == Origin::Script && (agent_only(name) || matches!(name, "ask_user" | "update_plan" | "define_tool" | "delete_tool")) {
        return Verdict::Refuse(format!("{name} can't be called from a script."));
    }
    let mut class = if policy.external { Class::External } else { class_of(name, input) };

    // A script's output must never become a measurement, whatever anyone clicks: the person
    // approving can't tell a generated file from a real one.
    if class == Class::Measurement {
        if ctx.origin == Origin::Script {
            return Verdict::Refuse(format!("{name} records real measurements, and a script's numbers aren't measurements."));
        }
        if ctx.log.as_ref().is_some_and(TurnLog::ran_script) {
            return Verdict::Refuse(format!("{name} records real measurements, and a script ran in this turn, so its data can't be told from a real one. Ask the user to run this in a new message, with data from the instrument."));
        }
        let root = workspace_root();
        for key in ["path", "plan_path"] {
            if let Some(file) = input[key].as_str().filter(|f| is_under(f, &root)) {
                return Verdict::Refuse(format!("{file} is in the assistant's own workspace, so {name} can't take it as a real measurement. It needs the file the instrument wrote."));
            }
        }
    }
    let outside = |p: &&str| !policy.roots.iter().any(|r| is_under(p, r));
    let written = written_path(name, input).filter(outside);
    let read = read_path(name, input).filter(outside);
    let touched = written.or(read);
    let folder = touched.map(|w| Path::new(w).parent().unwrap_or(Path::new(w)).to_string_lossy().to_lowercase());
    if touched.is_some() && matches!(class, Class::Project | Class::Read) {
        class = Class::Files;
    }

    // Reading elsewhere still only asks. Everything else that isn't looking waits for the plan.
    if ctx.planning() && !matches!(class, Class::Read | Class::View | Class::Macro) && !(class == Class::Files && written.is_none()) {
        return Verdict::Refuse(format!("Plan mode is on, so {name} can't run yet. Put it in your plan with propose_plan, and it can run once the user approves."));
    }

    let (asks, text, remember) = match class {
        Class::Read | Class::Macro => (false, String::new(), None),
        Class::Scene | Class::View | Class::Project => (policy.mode == Mode::Ask, format!("Allow the assistant to run {name}?"), Some(("Allow edits for this chat".to_string(), REMEMBER_EDITS.to_string()))),
        Class::Files => (
            policy.mode != Mode::All,
            if written.is_some() { format!("Allow the assistant to write a file outside this project?\n{}", touched.unwrap_or_default()) } else { format!("Allow the assistant to read this, from outside this project? What it reads goes to the AI provider.\n{}", touched.unwrap_or_default()) },
            folder.map(|f| ("Allow this folder for this chat".to_string(), format!("folder:{f}"))),
        ),
        Class::External => {
            let server = name.split("__").next().unwrap_or(name);
            (policy.mode != Mode::All, format!("Allow the external tool {name}, from the MCP server \"{server}\"? It isn't part of this app, and could do anything that server can."), Some(("Allow this server for this chat".to_string(), format!("server:{server}"))))
        }
        Class::OpensOutside => (policy.mode != Mode::All, "Allow the assistant to open this in another app?".to_string(), None),
        Class::Outside => (true, format!("Allow the assistant to connect this app to a system outside it? {name} sends data out or starts a server. Check the addresses below."), None),
        Class::Measurement => (true, format!("Allow the assistant to record this as a real measurement? Only allow it if these numbers or this file came from a real instrument. {name} will count them as measured from now on."), None),
        Class::Unknown => (true, format!("The app doesn't know what {name} does. Allow it?"), None),
    };
    if !asks || remember.as_ref().is_some_and(|(_, key)| policy.approved.contains(key)) {
        return Verdict::Run;
    }
    // Nobody is there to ask, and Stop couldn't release the call if it waited.
    if ctx.origin == Origin::Mcp {
        return Verdict::Refuse(format!("{name} needs the user's approval, which an MCP client can't ask for. Use the assistant in the app for this."));
    }
    // A loop in a script would be a wall of cards. Anything needing approval is for the model to ask.
    if ctx.origin == Origin::Script {
        return Verdict::Refuse(format!("{name} needs the user's approval, which a script can't ask for. Call it directly instead of from the script."));
    }
    Verdict::Ask(Approval { text: format!("{text}\n\n{}", details(input)), remember })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent::harness::Who;
    use crate::agent::tools::ToolOutput;

    fn verdict(name: &str, input: Value, origin: Origin, mode: Mode) -> Verdict {
        let roots = [workspace_root()];
        let approved = HashSet::new();
        decide(name, &input, &CallCtx { origin, ..Default::default() }, &Policy { mode, approved: &approved, roots: &roots, external: false })
    }

    fn runs(v: &Verdict) -> bool {
        *v == Verdict::Run
    }
    fn asks(v: &Verdict) -> bool {
        matches!(v, Verdict::Ask(_))
    }
    fn refused(v: &Verdict) -> bool {
        matches!(v, Verdict::Refuse(_))
    }

    /// A new tool has to be given a class: until then it asks, and this fails to say so.
    #[test]
    fn every_tool_has_a_class() {
        let all = crate::agent::tools::tool_defs(true).into_iter().chain(crate::agent::workspace::defs()).chain([crate::agent::python::def(), crate::agent::planning::def()]);
        let unknown: Vec<String> = all.map(|d| d.name).filter(|n| class_of(n, &json!({})) == Class::Unknown).collect();
        assert!(unknown.is_empty(), "add these to class_of in gate.rs: {unknown:?}");
    }

    /// The plan's table, one row per class and one column per mode.
    #[test]
    fn modes_ask_for_what_they_say() {
        let o = Origin::Assistant;
        for mode in Mode::ALL {
            assert!(runs(&verdict("get_scene", json!({}), o, mode)), "lookups never ask");
            assert!(runs(&verdict("custom_shelf", json!({}), o, mode)), "macro steps are judged one by one");
            assert!(asks(&verdict("rtls_configure_outputs", json!({}), o, mode)), "outside systems always ask");
            assert!(asks(&verdict("outcomes_check", json!({ "measured": 1.0 }), o, mode)), "measurements always ask");
            assert!(asks(&verdict("something_new", json!({}), o, mode)), "unknown tools always ask");
        }
        for (name, input) in [("place_objects", json!({})), ("screenshot", json!({})), ("antenna_set", json!({}))] {
            assert!(asks(&verdict(name, input.clone(), o, Mode::Ask)), "{name}");
            assert!(runs(&verdict(name, input.clone(), o, Mode::Edits)), "{name}");
            assert!(runs(&verdict(name, input, o, Mode::All)), "{name}");
        }
        let outside = json!({ "path": r"C:\Windows\x.json" });
        assert!(asks(&verdict("save_layout", outside.clone(), o, Mode::Ask)));
        assert!(asks(&verdict("save_layout", outside.clone(), o, Mode::Edits)));
        assert!(runs(&verdict("save_layout", outside, o, Mode::All)));
    }

    #[test]
    fn the_default_is_edits_and_old_settings_files_keep_working() {
        assert_eq!(Mode::default(), Mode::Edits);
        assert_eq!(serde_json::to_value(Mode::Edits).unwrap(), json!("edits"));
        assert_eq!(serde_json::from_value::<Mode>(json!("all")).unwrap(), Mode::All);
    }

    #[test]
    fn the_input_can_change_what_a_tool_is() {
        assert_eq!(class_of("rtls_openems", &json!({ "action": "status" })), Class::Read);
        assert_eq!(class_of("rtls_openems", &json!({ "action": "start" })), Class::Outside);
        assert_eq!(class_of("rtls_openems", &json!({})), Class::Outside, "no action isn't a safe action");
        assert_eq!(class_of("antenna_optimize_status", &json!({})), Class::Read);
        assert_eq!(class_of("antenna_optimize_status", &json!({ "apply": true })), Class::Project);
    }

    #[test]
    fn mcp_clients_are_refused_instead_of_left_waiting() {
        let o = Origin::Mcp;
        assert!(runs(&verdict("get_scene", json!({}), o, Mode::Edits)));
        assert!(runs(&verdict("place_objects", json!({}), o, Mode::Edits)), "ordinary MCP use keeps working");
        assert!(refused(&verdict("rtls_configure_outputs", json!({}), o, Mode::All)));
        assert!(refused(&verdict("outcomes_check", json!({}), o, Mode::All)));
        assert!(refused(&verdict("save_layout", json!({ "path": r"C:\Windows\x.json" }), o, Mode::Edits)));
        assert!(refused(&verdict("place_objects", json!({}), o, Mode::Ask)));
    }

    #[test]
    fn the_harness_checking_the_models_work_isnt_stopped() {
        assert!(runs(&verdict("screenshot", json!({}), Origin::Harness, Mode::Ask)));
        assert!(runs(&verdict("set_camera", json!({}), Origin::Harness, Mode::Ask)));
    }

    #[test]
    fn inside_means_inside() {
        let root = PathBuf::from(r"C:\Data\Project");
        for p in [r"C:\Data\Project\a.json", r"c:\data\PROJECT\sub\a.json", r"C:\Data\Project", "C:/Data/Project/a.json", r"C:\Data\Project\x\..\a.json"] {
            assert!(is_under(p, &root), "{p}");
        }
        for p in [r"C:\Data\Project2\a.json", r"C:\Data\Other\a.json", r"C:\Data\Project\..\Other\a.json", r"\\?\C:\Data\Project\..\Other\a.json", r"\\server\share\a.json", ""] {
            assert!(!is_under(p, &root), "{p}");
        }
    }

    #[test]
    fn a_file_in_the_project_needs_no_approval_and_one_elsewhere_does() {
        let inside = workspace_root().join("chat").join("a.json").to_string_lossy().to_string();
        assert!(runs(&verdict("save_layout", json!({ "path": inside }), Origin::Assistant, Mode::Edits)));
        for (tool, key) in [("export_usda", "path"), ("site_survey_practice", "path"), ("site_anchor_practice", "path"), ("site_acceptance_plan", "path"), ("site_acceptance_preview", "path")] {
            assert!(asks(&verdict(tool, json!({ key: r"C:\Windows\x" }), Origin::Assistant, Mode::Edits)), "{tool}");
        }
        assert!(runs(&verdict("site_acceptance_plan", json!({}), Origin::Assistant, Mode::Edits)), "no file, nothing to approve");
    }

    #[test]
    fn allowing_for_the_chat_is_remembered_only_where_offered() {
        let roots = [workspace_root()];
        let input = json!({ "path": r"C:\Reports\x.json" });
        let ctx = CallCtx::default();
        let no = HashSet::new();
        let Verdict::Ask(a) = decide("save_layout", &input, &ctx, &Policy { mode: Mode::Edits, approved: &no, roots: &roots, external: false }) else { panic!() };
        let (_, key) = a.remember.expect("a folder can be allowed");
        assert!(a.text.contains(r"C:\Reports\x.json"));
        let yes: HashSet<String> = [key].into();
        let p = Policy { mode: Mode::Edits, approved: &yes, roots: &roots, external: false };
        assert_eq!(decide("save_layout", &input, &ctx, &p), Verdict::Run);
        assert!(asks(&decide("save_layout", &json!({ "path": r"C:\Other\x.json" }), &ctx, &p)), "only that folder");

        // Measurements and outside systems ask every time.
        let Verdict::Ask(m) = verdict("outcomes_check", json!({}), Origin::Assistant, Mode::Edits) else { panic!() };
        assert!(m.remember.is_none());
        let Verdict::Ask(o) = verdict("rtls_configure_outputs", json!({}), Origin::Assistant, Mode::Edits) else { panic!() };
        assert!(o.remember.is_none());
    }

    #[test]
    fn external_tools_ask_once_per_server() {
        let roots = [workspace_root()];
        let ctx = CallCtx::default();
        let no = HashSet::new();
        let p = Policy { mode: Mode::Edits, approved: &no, roots: &roots, external: true };
        let Verdict::Ask(a) = decide("github__create_issue", &json!({}), &ctx, &p) else { panic!() };
        let (_, key) = a.remember.unwrap();
        assert_eq!(key, "server:github");
        let yes: HashSet<String> = [key].into();
        let p = Policy { mode: Mode::Edits, approved: &yes, roots: &roots, external: true };
        assert_eq!(decide("github__list_prs", &json!({}), &ctx, &p), Verdict::Run);
        assert!(asks(&decide("other__x", &json!({}), &ctx, &p)));
    }

    /// Keeps the rule that synthetic data never counts as calibration.
    #[test]
    fn a_script_cant_turn_its_output_into_a_measurement() {
        let in_workspace = workspace_root().join("chat").join("walk.csv").to_string_lossy().to_string();
        for key in ["path", "plan_path"] {
            assert!(refused(&verdict("site_survey_fit", json!({ key: in_workspace }), Origin::Assistant, Mode::All)), "{key}");
        }
        assert!(asks(&verdict("site_survey_fit", json!({ "path": r"C:\Survey\walk.csv" }), Origin::Assistant, Mode::All)));

        let log = TurnLog::default();
        log.push(Who::Model, "python_run", &json!({}), &ToolOutput::text("{}"));
        let ctx = CallCtx { log: Some(log), ..Default::default() };
        let roots = [workspace_root()];
        let no = HashSet::new();
        let p = Policy { mode: Mode::All, approved: &no, roots: &roots, external: false };
        assert!(refused(&decide("outcomes_check", &json!({ "measured": 1.0 }), &ctx, &p)));
    }

    /// Enforced here, so plan mode doesn't rest on the model behaving.
    #[test]
    fn plan_mode_lets_it_look_but_not_touch() {
        let flag = Arc::new(AtomicBool::new(true));
        let ctx = CallCtx { planning: Some(flag.clone()), ..Default::default() };
        let roots = [workspace_root()];
        let no = HashSet::new();
        let p = Policy { mode: Mode::All, approved: &no, roots: &roots, external: false };
        for name in ["get_scene", "search_knowledge", "antenna_calc", "get_screen", "screenshot", "set_camera", "propose_plan", "custom_shelf"] {
            assert_eq!(decide(name, &json!({}), &ctx, &p), Verdict::Run, "{name} only looks");
        }
        for name in ["place_objects", "antenna_set", "antenna_run", "sim_run", "define_tool", "write_file", "open_external", "rtls_configure_outputs", "outcomes_check", "something_new"] {
            assert!(refused(&decide(name, &json!({}), &ctx, &p)), "{name} would change something");
        }
        assert!(refused(&decide("save_layout", &json!({ "path": r"C:\x\a.json" }), &ctx, &p)), "writing elsewhere");
        // An external tool could do anything.
        let ext = Policy { mode: Mode::All, approved: &no, roots: &roots, external: true };
        assert!(refused(&decide("github__create_issue", &json!({}), &ctx, &ext)));
        // Reading outside the project still follows the mode, as it does outside plan mode.
        let edits = Policy { mode: Mode::Edits, approved: &no, roots: &roots, external: false };
        assert!(asks(&decide("read_file", &json!({ "path": r"C:\x\a.csv" }), &ctx, &edits)));

        // Approving lifts it at once, for calls already under way.
        flag.store(false, Ordering::Relaxed);
        assert_eq!(decide("place_objects", &json!({}), &ctx, &p), Verdict::Run);
    }

    /// A script can do what the scene needs, and nothing that takes a person's say-so.
    #[test]
    fn scripts_can_edit_but_not_escape() {
        let o = Origin::Script;
        for (name, input) in [("get_scene", json!({})), ("update_object", json!({})), ("place_objects", json!({})), ("antenna_calc", json!({})), ("rtls_run", json!({}))] {
            assert!(runs(&verdict(name, input, o, Mode::Edits)), "{name}");
        }
        for name in ["python_run", "delegate", "read_file", "write_file", "open_file", "open_external", "propose_plan", "ask_user", "update_plan", "define_tool", "delete_tool", "rtls_configure_outputs", "outcomes_check", "site_survey_fit", "site_acceptance_score", "something_new"] {
            assert!(refused(&verdict(name, json!({}), o, Mode::All)), "{name}");
        }
        assert!(refused(&verdict("save_layout", json!({ "path": r"C:\x\a.json" }), o, Mode::Edits)), "writing elsewhere");
        assert!(refused(&verdict("place_objects", json!({}), o, Mode::Ask)), "no wall of cards from a loop");
        // In plan mode a script can't change anything either.
        let ctx = CallCtx { origin: o, planning: Some(Arc::new(AtomicBool::new(true))), ..Default::default() };
        let roots = [workspace_root()];
        let no = HashSet::new();
        let p = Policy { mode: Mode::All, approved: &no, roots: &roots, external: false };
        assert!(refused(&decide("place_objects", &json!({}), &ctx, &p)));
        assert_eq!(decide("get_scene", &json!({}), &ctx, &p), Verdict::Run);
    }

    #[test]
    fn the_assistants_own_tools_are_out_of_reach_of_mcp_and_macros() {
        for name in ["read_file", "list_files", "write_file", "open_file", "open_external", "python_run", "delegate", "open_layout_tab", "propose_plan"] {
            assert!(agent_only(name), "{name}");
            assert!(refused(&verdict(name, json!({}), Origin::Mcp, Mode::All)), "{name}");
        }
        assert!(!agent_only("get_scene"));
    }

    #[test]
    fn reading_inside_is_free_and_reading_elsewhere_asks() {
        let inside = workspace_root().join("chat").join("files").join("a.csv").to_string_lossy().to_string();
        for name in ["read_file", "list_files", "open_file"] {
            assert!(runs(&verdict(name, json!({ "path": inside }), Origin::Assistant, Mode::Edits)), "{name}");
            let Verdict::Ask(a) = verdict(name, json!({ "path": r"C:\Users\me\Documents\a.csv" }), Origin::Assistant, Mode::Edits) else { panic!("{name}") };
            assert!(a.text.contains("goes to the AI provider"), "{}", a.text);
            assert!(a.remember.is_some(), "a folder can be allowed");
            assert!(runs(&verdict(name, json!({ "path": r"C:\Users\me\Documents\a.csv" }), Origin::Assistant, Mode::All)), "{name}");
        }
    }

    #[test]
    fn opening_in_another_app_asks_each_time_unless_all_is_on() {
        let Verdict::Ask(a) = verdict("open_external", json!({ "target": "https://x.example" }), Origin::Assistant, Mode::Edits) else { panic!() };
        assert!(a.remember.is_none());
        assert!(a.text.contains("https://x.example"));
        assert!(runs(&verdict("open_external", json!({ "target": "https://x.example" }), Origin::Assistant, Mode::All)));
    }

    #[test]
    fn the_card_shows_the_arguments_and_a_long_one_is_cut() {
        let Verdict::Ask(a) = verdict("rtls_configure_outputs", json!({ "webhook": "http://attacker.example" }), Origin::Assistant, Mode::Edits) else { panic!() };
        assert!(a.text.contains("http://attacker.example"), "{}", a.text);
        let Verdict::Ask(b) = verdict("rtls_configure_outputs", json!({ "x": "y".repeat(5000) }), Origin::Assistant, Mode::Edits) else { panic!() };
        assert!(b.text.chars().count() < 1500);
    }
}
