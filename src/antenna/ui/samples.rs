//! Sample projects: the page that lists them, and opening one onto its results. Those that
//! solve in seconds solve when opened. Those that take minutes, or need openEMS, come with
//! their results stored, so anyone can look at them straight away and solve them again.

use std::io::Read;
use std::sync::Arc;

use antenna_core::geometry::Geometry;
use antenna_core::project::SimResult;
use antenna_core::samples::{self, Group, Opens, Sample};
use bevy_egui::egui::{self, vec2};

use super::{Page, group, view3d, weak};
use crate::antenna::Studio;
use crate::theme;

/// Results stored with the app, by sample. `antenna_bake_samples` writes them; a test checks
/// they still belong to their samples and that they meet what those say.
const STORED: &[(&str, &[u8])] = &[];

/// The stored result of a sample, if it has one.
pub fn stored(id: &str) -> Option<SimResult> {
    let bytes = STORED.iter().find(|s| s.0 == id)?.1;
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut json).ok()?;
    serde_json::from_slice(&json).ok()
}

pub fn has_stored(id: &str) -> bool {
    STORED.iter().any(|s| s.0 == id)
}

/// Too slow to solve on opening, or needing openEMS: these want a result in `STORED`.
const SLOW: [&str; 11] = ["rfid_panel", "wifi_corner", "slot_plate", "cband_waveguide", "wifi_ifa", "wifi5_patch", "cband_dra", "uwb_disc", "wifi_mimo", "uwb_vivaldi", "xband_horn"];

/// How opening it goes, in a few words, for its card.
pub fn opening_words(s: &Sample) -> &'static str {
    if has_stored(s.id) {
        "Results included"
    } else if SLOW.contains(&s.id) {
        "Takes minutes to solve"
    } else {
        match s.opens {
            Opens::FarField => "Solves in seconds",
            Opens::Reconfig => "Solves when opened",
            Opens::Gate => "Solves when opened",
            Opens::Drawing => "Opens on the drawing",
            Opens::Phased | Opens::DirectionFinding => "Solves when opened",
        }
    }
}

/// Opens a sample, and lands on what it's for. `Err` says why it can't right now.
pub fn open(st: &mut Studio, s: &'static Sample) -> Result<(), String> {
    if st.busy() {
        return Err("A job is running. Wait for it to finish or cancel it, then open the sample.".to_string());
    }
    let project = s.project().map_err(|e| format!("{e:#}"))?;
    st.project = project;
    st.clear_latest();
    let u = &mut st.ui;
    u.file = None;
    u.cam = Default::default();
    u.opt_vars.clear();
    u.tol.clear();
    u.goals.clear();
    u.sweep_var.clear();
    u.reconfig = Default::default();
    u.far_port = 0;
    u.far_k = 0;
    u.plane_k = 0;
    u.use_drive = !s.drive.is_empty();
    u.draw = Default::default();
    let page = match s.opens {
        Opens::FarField => {
            u.view = view3d::ViewMode::Pattern;
            Page::FarField
        }
        Opens::Drawing => {
            u.view = view3d::ViewMode::Model;
            u.draw.tool = super::draw::Tool::Select;
            Page::Draw
        }
        Opens::Reconfig => {
            u.view = view3d::ViewMode::Model;
            Page::Reconfig
        }
        Opens::Gate => {
            u.view = view3d::ViewMode::Model;
            Page::Gate
        }
        Opens::Phased => {
            u.view = view3d::ViewMode::Pattern;
            Page::Phased
        }
        Opens::DirectionFinding => {
            u.view = view3d::ViewMode::Model;
            Page::DirectionFinding
        }
    };
    u.page = page;
    // It stays on this page's message and view rather than resetting them on arrival.
    u.shown_page = Some(page);
    let note = match s.opens {
        // On the drawing, which is what it's for: the solve is there when you turn to the results.
        Opens::Drawing => {
            super::draw::entered(st);
            st.ui.reconfig.view_f = Some(s.centre());
            st.run();
            s.about.to_string()
        }
        Opens::FarField => match stored(s.id) {
            Some(r) => {
                let k = r.far_index(s.centre()).unwrap_or(0);
                st.ui.far_k = k;
                st.show_result(Arc::new(r));
                format!("{} Results stored with the app: Run on the Solver setup page solves it again.", s.about)
            }
            None => {
                st.ui.reconfig.view_f = Some(s.centre());
                st.run();
                s.about.to_string()
            }
        },
        Opens::Reconfig => {
            st.run_reconfig();
            s.about.to_string()
        }
        Opens::Phased | Opens::DirectionFinding => {
            st.ui.reconfig.view_f = Some(s.centre());
            st.run();
            s.about.to_string()
        }
        Opens::Gate => {
            let g = st.project.geometry().map_err(|e| format!("{e:#}"))?;
            let spec = antenna_core::recipes::gate::GateSpec::for_project(&st.project, &g);
            let project = st.project.clone();
            st.ui.gate.keep_spec(spec, &project);
            st.run_gate(spec);
            s.about.to_string()
        }
    };
    st.ui.message = Some((note, true));
    Ok(())
}

/// A sample's model, drawn in `rect`. Models are built once.
pub fn thumbnail(ui: &mut egui::Ui, rect: egui::Rect, index: usize) {
    static MODELS: std::sync::OnceLock<Vec<Geometry>> = std::sync::OnceLock::new();
    let models = MODELS.get_or_init(|| samples::all().iter().map(|s| s.project().and_then(|p| p.geometry()).unwrap_or_default()).collect());
    if let Some(g) = models.get(index) {
        view3d::thumbnail(ui, rect, g);
    }
}

fn card(ui: &mut egui::Ui, st: &mut Studio, index: usize, s: &'static Sample, w: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, w * 0.72 + 66.0), egui::Sense::click());
    let pic = egui::Rect::from_min_size(rect.min, vec2(w, w * 0.72));
    thumbnail(ui, pic, index);
    let stroke = if resp.hovered() { egui::Stroke::new(1.0, theme::STROKE_STRONG) } else { egui::Stroke::new(1.0, theme::STROKE) };
    ui.painter().rect_stroke(pic, 4.0, stroke, egui::StrokeKind::Inside);
    let title = ui.painter().layout(s.title.to_string(), egui::FontId::proportional(13.0), theme::TEXT, w);
    let title_h = title.size().y;
    ui.painter().galley(pic.left_bottom() + vec2(0.0, 3.0), title, theme::TEXT);
    let line = format!("{} · {}", s.band_text(), opening_words(s));
    let sub = ui.painter().layout(line, egui::FontId::proportional(11.0), theme::WEAK, w);
    ui.painter().galley(pic.left_bottom() + vec2(0.0, 5.0 + title_h), sub, theme::WEAK);
    let held = s.held_to();
    let tip = if held.is_empty() { s.about.to_string() } else { format!("{}\n\nChecked against a solver: {held}.", s.about) };
    let resp = resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked()
        && let Err(e) = open(st, s)
    {
        st.ui.message = Some((e, false));
    }
}

pub fn page(ui: &mut egui::Ui, st: &mut Studio) {
    theme::caption(ui, "Sample projects");
    weak(ui, "Real uses, each in the band it's for and checked against a solver. Click one to open it on its results. All of it stays yours to change. Simulated, not measured.");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut st.ui.sample_search).hint_text("Search").desired_width(150.0));
        let label = st.ui.sample_group.map_or("All uses", Group::label);
        egui::ComboBox::from_id_salt("sample_group").selected_text(label).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.ui.sample_group, None, "All uses");
            for g in Group::ALL {
                ui.selectable_value(&mut st.ui.sample_group, Some(g), g.label());
            }
        });
    });
    let needle = st.ui.sample_search.to_lowercase();
    let chosen = st.ui.sample_group;
    let width = ui.available_width();
    let cols = ((width / 140.0).floor() as usize).max(2);
    let w = (width - 8.0 * (cols as f32 - 1.0)) / cols as f32;
    for g in Group::ALL {
        if chosen.is_some_and(|c| c != g) {
            continue;
        }
        let list: Vec<(usize, &'static Sample)> = samples::all().iter().enumerate().filter(|(_, s)| s.group == g).filter(|(_, s)| needle.is_empty() || s.title.to_lowercase().contains(&needle) || s.about.to_lowercase().contains(&needle) || g.label().to_lowercase().contains(&needle)).collect();
        if list.is_empty() {
            continue;
        }
        group(ui, |ui| {
            theme::caption(ui, g.label());
            let inner_w = (ui.available_width() - 8.0 * (cols as f32 - 1.0)) / cols as f32;
            for row in list.chunks(cols) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (index, s) in row {
                        card(ui, st, *index, s, inner_w.min(w));
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::antenna::run::{self, JobCtx, RunOptions};

    /// Stored results have to belong to the samples as they stand, and do what those say.
    #[test]
    fn antenna_stored_samples_are_current_and_do_what_they_say() {
        let mut problems = Vec::new();
        for (id, _) in STORED {
            let s = samples::by_id(id).unwrap_or_else(|| panic!("a result stored for no sample: {id}"));
            let r = stored(id).unwrap_or_else(|| panic!("{id}: the stored result won't read"));
            let p = s.project().unwrap();
            let vars = p.vars().unwrap();
            for (k, v) in &vars {
                let had = r.vars.get(k).copied().unwrap_or(f64::NAN);
                if (had - v).abs() > 1e-9 * v.abs().max(1e-9) {
                    problems.push(format!("{id}: solved with {k} = {had}, the sample has {v}: bake it again"));
                }
            }
            assert_eq!((r.network.f.first().copied(), r.network.f.last().copied()), (Some(p.setup.f_lo), Some(p.setup.f_hi)), "{id}");
            assert_eq!(r.far_f, p.setup.far_freqs, "{id}");
            let f = s.figures(&r).unwrap_or_else(|| panic!("{id}: no pattern"));
            println!("{id:18} worst {:6.1} dB gain {:5.1} dBi AR {:4.1} res {:?}, {}", f.worst_db, f.gain_dbi, f.axial_ratio_db, f.resonance.map(|x| (x / 1e6).round()), r.solver);
            problems.extend(s.shortfalls(&f).into_iter().map(|c| format!("{id}: {c}")));
            assert!(r.warnings.iter().all(|w| !w.contains("diverged")), "{id}: {:?}", r.warnings);
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// A drawn sample opens on the drawing with nothing to set up, and still solves.
    #[test]
    fn antenna_a_drawn_sample_opens_on_the_drawing_and_solves() {
        let mut st = Studio::default();
        let s = samples::by_id("drawn_dipole").unwrap();
        open(&mut st, s).unwrap();
        assert_eq!(st.ui.page, Page::Draw);
        assert_eq!(st.ui.draw.tool, super::super::draw::Tool::Select);
        let t0 = std::time::Instant::now();
        while st.busy() {
            assert!(t0.elapsed().as_secs() < 120);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let r = st.latest().expect("solved");
        let f = s.figures(&r).unwrap();
        assert!(s.shortfalls(&f).is_empty(), "{:?}", s.shortfalls(&f));
    }

    /// A sample with a stored result opens at once with it; one without solves.
    #[test]
    fn antenna_a_sample_opens_on_its_results() {
        let mut st = Studio::default();
        let quick = samples::by_id("ham_yagi").unwrap();
        open(&mut st, quick).unwrap();
        assert_eq!(st.ui.page, Page::FarField);
        assert_eq!(st.project.name, quick.title);
        let t0 = std::time::Instant::now();
        while st.busy() {
            assert!(t0.elapsed().as_secs() < 120);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let r = st.latest().expect("solved");
        assert!(r.solver.contains("MoM"), "{}", r.solver);
        let f = quick.figures(&r).unwrap();
        assert!(quick.shortfalls(&f).is_empty(), "{:?}", quick.shortfalls(&f));
        // Opening another clears the last one's results, so they can't be read as its own.
        let second = samples::by_id("noaa_turnstile").unwrap();
        open(&mut st, second).unwrap();
        assert!(st.latest().is_none_or(|r| !std::ptr::eq(&*r, &*st.latest().unwrap()) || r.network.ports() == 2));
        assert!(st.ui.use_drive, "its ports work together");
        while st.busy() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(st.latest().unwrap().network.ports(), 2);
    }

    #[test]
    fn antenna_a_sample_can_wait_for_a_running_job() {
        let mut st = Studio::default();
        st.project = antenna_core::project::Project::from_template("dipole", Some("1GHz")).unwrap();
        st.run();
        if st.busy() {
            let name = st.project.name.clone();
            let e = open(&mut st, samples::by_id("ham_yagi").unwrap()).unwrap_err();
            assert!(e.contains("job"), "{e}");
            assert_eq!(st.project.name, name, "nothing changed");
        }
    }

    /// Solves samples and stores their results, for samples too slow to solve when opened:
    /// `BAKE=wifi_ifa,uwb_disc cargo test --bin SenseSim antenna_bake_samples -- --ignored --nocapture`
    /// Then add them to `STORED`.
    #[test]
    #[ignore]
    fn antenna_bake_samples() {
        let list = std::env::var("BAKE").expect("BAKE=sample,sample");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/antenna_samples");
        std::fs::create_dir_all(&dir).unwrap();
        for id in list.split(',').filter(|x| !x.is_empty()) {
            let (id, sets) = id.split_once(':').unwrap_or((id, ""));
            let s = samples::by_id(id).unwrap_or_else(|| panic!("no sample {id}"));
            let mut p = s.project().unwrap();
            for set in sets.split('+').filter(|x| !x.is_empty()) {
                let (k, v) = set.split_once('=').unwrap();
                p.params.iter_mut().find(|r| r.name == k).unwrap_or_else(|| panic!("{k}")).expr = v.to_string();
            }
            let dry = std::env::var("BAKE_DRY").is_ok();
            let c = JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() };
            let t0 = std::time::Instant::now();
            let mut r = match run::simulate(&p, &c, &RunOptions { keep: false, report: false, ..Default::default() }) {
                Ok(r) => r,
                Err(e) => {
                    println!("{id}: FAILED {e:#}");
                    continue;
                }
            };
            r.dir.clear();
            let f = s.figures(&r);
            let shortfalls = f.as_ref().map_or(Vec::new(), |x| s.shortfalls(x));
            println!("{id} {sets}: {} size {} in {:.0} s: {:?}, short of: {shortfalls:?}, warnings {:?}", r.solver, r.size, t0.elapsed().as_secs_f64(), f, r.warnings);
            if !dry {
                let json = serde_json::to_vec(&r).unwrap();
                let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
                gz.write_all(&json).unwrap();
                let bytes = gz.finish().unwrap();
                std::fs::write(dir.join(format!("{id}.json.gz")), &bytes).unwrap();
                println!("{id}: {} kB stored", bytes.len() / 1024);
            }
        }
    }
}
