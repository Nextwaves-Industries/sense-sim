//! A pick that an edit has moved must stop every solver route, before any solver starts. A route that
//! took the face's number on trust would apply the condition to some other face, with no error, and the
//! answer would look like any other. No solvers are needed: the check comes first.

use std::time::{Duration, Instant};

use sim_core::catalog::{self, AnalysisId, Needs};
use sim_core::geometry::Partition;
use sim_core::jobs::manager::{JobManager, Kind, State};
use sim_core::results::ResultSet;
use sim_core::study::{Condition, FaceRef, Study};

fn square() -> Partition {
    Partition { name: "s".into(), positions: vec![[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]], triangles: vec![[0, 1, 2], [0, 2, 3]], tri_face: vec![0, 1], faces: 2 }
}

fn study(id: AnalysisId) -> Study {
    let mut s = Study::new("t".into(), id);
    s.body = Some("B".into());
    // Face 0's number, with a point that's now on face 1.
    let kind = s.offered_conditions().remove(0);
    s.conditions.push(Condition { kind, faces: vec![FaceRef { face: 0, point: [0.25, 0.75, 0.0], second: false }] });
    s
}

fn run(id: AnalysisId, work: impl FnOnce(&Study, &sim_core::jobs::manager::JobCtx) -> anyhow::Result<ResultSet> + Send + 'static) -> String {
    let root = std::env::temp_dir().join(format!("sense-sim-moved-faces-{id:?}-{}", std::process::id()));
    let jobs = JobManager::new(root.clone());
    let s = study(id);
    let job = jobs.submit("t", "t", Kind::Cpu, move |ctx| work(&s, ctx));
    let t0 = Instant::now();
    let error = loop {
        let state = jobs.list().into_iter().find(|j| j.id == job).expect("the job").state;
        match state {
            State::Failed { error } => break error,
            State::Done => panic!("{id:?} ran on a moved face"),
            _ => {}
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "{id:?} went on past the check to start a solver");
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = std::fs::remove_dir_all(root);
    error
}

#[test]
fn every_flow_route_stops_on_a_moved_face() {
    for a in catalog::ANALYSES.iter().filter(|a| matches!(a.needs, Needs::Foam | Needs::Gpu)) {
        let id = a.id;
        let needs = a.needs;
        let e = run(id, move |s, ctx| if needs == Needs::Foam { sim_core::foam::study::run_study(s, &square(), ctx) } else { sim_core::lbm::study::run_study(s, &square(), ctx) });
        assert!(e.contains("pick the face again"), "{id:?}: {e}");
    }
}

#[test]
fn electromagnetics_stops_on_a_moved_face() {
    let geometry = || sim_core::ccx::study::FeaGeometry { partition: square(), step: Vec::new(), second: None };
    for a in catalog::ANALYSES.iter().filter(|a| a.needs == Needs::Elmer) {
        let e = run(a.id, move |s, ctx| sim_core::elmer_study::run_study(s, &geometry(), ctx));
        assert!(e.contains("pick the face again"), "{:?}: {e}", a.id);
    }
}
