//! Sets up the `SenseSimSolvers` WSL distro: OpenFOAM and Elmer. The user's distros aren't touched.

use sim_core::jobs::Cancel;
use sim_core::provision;

fn main() -> anyhow::Result<()> {
    let t0 = std::time::Instant::now();
    provision::install_wsl(&Cancel::default(), |l| println!("{l}"))?;
    println!("READY in {:.1} min; ready flag: {}", t0.elapsed().as_secs_f64() / 60.0, provision::wsl_ready());
    Ok(())
}
