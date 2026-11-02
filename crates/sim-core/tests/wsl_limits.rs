//! The memory limit on a solver's run, in the solvers' own distro: past it, the run's allocations fail
//! and the VM it shares with the user's distros is left alone.
//!
//!     cargo test --test wsl_limits -- --ignored --nocapture --test-threads=1

use sim_core::jobs::Cancel;
use sim_core::jobs::wsl::RANK_LIMIT;

fn run(script: &str) -> (Vec<String>, Result<(), String>) {
    let mut lines = Vec::new();
    let r = sim_core::provision::distro().run_supervised("/tmp", &[], &["/bin/bash", "-c", script], &Cancel::default(), |l| lines.push(l.to_string()));
    (lines, r.map(|_| ()).map_err(|e| format!("{e:#}")))
}

#[test]
#[ignore = "needs the solver environment"]
fn sim_a_run_is_held_to_its_memory_limit() {
    let available = sim_core::provision::distro().memory_available().unwrap();
    println!("{:.1} GB free in the VM", available / 1e9);

    // It's set, at 80% of what's free.
    let (lines, r) = run("ulimit -v; echo $SENSESIM_LIMIT_KB");
    r.unwrap();
    let (limit, told): (f64, f64) = (lines[0].trim().parse().unwrap(), lines[1].trim().parse().unwrap());
    let share = limit * 1024.0 / available;
    println!("limit {:.1} GB, {:.0}% of what's free", limit * 1024.0 / 1e9, share * 100.0);
    assert_eq!(limit, told, "the limit and what the scripts are told agree");
    assert!((0.7..0.9).contains(&share), "{share}");

    // A process asking for more than that is refused. dd takes its whole buffer at once, without
    // touching it, so this costs no memory even where the limit is large.
    let (lines, r) = run(&format!("dd if=/dev/zero of=/dev/null bs={}K count=1 2>&1", (limit * 1.1) as u64));
    println!("too much: {lines:?} {r:?}");
    assert!(r.is_err(), "it should fail");
    assert!(lines.iter().any(|l| l.contains("memory exhausted")), "{lines:?}");
    // Just under it is fine: a limit that stopped everything would be no use either.
    let (_, r) = run(&format!("dd if=/dev/zero of=/dev/null bs={}K count=1 2>&1", (limit * 0.5) as u64));
    assert!(r.is_ok(), "{r:?}");

    // A parallel solver's ranks get their share of it, in a subshell, and the serial steps around
    // them keep the whole limit.
    let (lines, r) = run(&format!("ulimit -v; ({RANK_LIMIT}; ulimit -v); ulimit -v"));
    r.unwrap();
    let [whole, rank, after]: [f64; 3] = std::array::from_fn(|i| lines[i].trim().parse().unwrap());
    println!("per rank {:.1} GB, then back to {:.1} GB", rank * 1024.0 / 1e9, after * 1024.0 / 1e9);
    assert_eq!(after, whole);
    assert_eq!(rank, (whole / 8.0).floor().max(2097152.0));
}

/// Is the floor in `RANK_LIMIT` high enough? 8 ranks of the real solver, on a real case, at that limit.
#[test]
#[ignore = "needs the solver environment"]
fn sim_a_parallel_solver_starts_at_the_lowest_limit() {
    let (lines, r) = run(&format!(
        "{}\ncd /tmp && rm -rf limitcase && cp -r $FOAM_TUTORIALS/incompressible/icoFoam/cavity/cavity limitcase && cd limitcase\nblockMesh > /dev/null\ncat > system/decomposeParDict <<EOF\nFoamFile {{ version 2.0; format ascii; class dictionary; object decomposeParDict; }}\nnumberOfSubdomains 8;\nmethod scotch;\nEOF\ndecomposePar > /dev/null\nfor gb in 1 2 3 4; do\n  echo \"at $gb GB: $( (ulimit -v $((gb * 1048576)); mpirun --oversubscribe -np 8 checkMesh -parallel 2>&1 || true) | grep -c '^End')\"\ndone\ncd /tmp && rm -rf limitcase\n",
        sim_core::provision::foam_env()
    ));
    println!("{lines:?} {r:?}");
    r.unwrap();
    // `RANK_LIMIT` never goes below 2 GB.
    assert!(lines.iter().any(|l| l == "at 2 GB: 1"), "{lines:?}");
}
