//! Installing the external solvers. Nothing here runs without the user having agreed: each tool
//! says what it is and how big before it starts. Downloads are pinned by SHA-256 and fetched from
//! the upstream projects, not re-hosted.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::jobs::Cancel;
use crate::jobs::wsl::Distro;
use crate::tools::solvers_dir;

/// The WSL distro solvers run in. Never the user's own.
pub const DISTRO: &str = "SenseSimSolvers";
const SOLVER_USER: &str = "solver";

/// A file to fetch.
#[derive(Debug, Clone, Copy)]
pub struct Download {
    pub url: &'static str,
    pub sha256: &'static str,
    /// Size in bytes, for the consent message.
    pub size: u64,
}

pub const GMSH: Download = Download { url: "https://gmsh.info/bin/Windows/gmsh-4.15.2-Windows64.zip", sha256: "A8208B56AEDEE3434907A893E303422ACACD9003140766E09F2ABBC664BA7157", size: 39_200_000 };
pub const CALCULIX: Download = Download { url: "https://www.dhondt.de/calculix_2.23_4win.zip", sha256: "87FC4E1721DF37370EC67D0A244BB14F9BF804B0A95DF49E2AE7369D30EC72E8", size: 61_700_000 };
pub const UBUNTU_ROOTFS: Download = Download { url: "https://cloud-images.ubuntu.com/wsl/releases/24.04/current/ubuntu-noble-wsl-amd64-wsl.rootfs.tar.gz", sha256: "8251E27FFFF381A4AF5F41DCB94D867DE3E0D9774A9241908AB34555D99315EA", size: 356_000_000 };

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02X}")).collect())
}

/// Fetches `d` to `to`, unless a good copy is there. A file with the wrong hash is deleted and an
/// error returned: a changed upstream file must not be run.
pub fn fetch(d: &Download, to: &Path, cancel: &Cancel, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    if to.exists() && sha256_of(to)?.eq_ignore_ascii_case(d.sha256) {
        return Ok(());
    }
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
    }
    let part = to.with_extension("part");
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3600))).build().into();
    let response = agent.get(d.url).call().map_err(|e| anyhow!("downloading {}: {e}", d.url))?;
    let mut body = response.into_body();
    let mut reader = body.as_reader();
    let mut out = std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let n = reader.read(&mut buffer).with_context(|| format!("downloading {}", d.url))?;
        if n == 0 {
            break;
        }
        out.write_all(&buffer[..n])?;
        done += n as u64;
        progress(done, d.size);
    }
    drop(out);
    let got = sha256_of(&part)?;
    if !got.eq_ignore_ascii_case(d.sha256) {
        let _ = std::fs::remove_file(&part);
        bail!("{} has changed upstream: its SHA-256 is {got}, expected {}. It was not installed.", d.url, d.sha256);
    }
    std::fs::rename(&part, to)?;
    Ok(())
}

/// Unpacks a zip, refusing paths that would leave `dir`.
pub fn unzip(zip_path: &Path, dir: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path)?).with_context(|| format!("reading {}", zip_path.display()))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let out = dir.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut f, &mut w)?;
    }
    Ok(())
}

/// Installs Gmsh and CalculiX: native, no WSL. All the structural analyses need.
pub fn install_native(cancel: &Cancel, mut on_step: impl FnMut(&str)) -> Result<()> {
    for (name, d, dir) in [("Gmsh", GMSH, "gmsh-4.15.2"), ("CalculiX", CALCULIX, "calculix-2.23")] {
        let folder = solvers_dir().join(dir);
        let zip_path = folder.join("download.zip");
        on_step(&format!("Downloading {name}"));
        fetch(&d, &zip_path, cancel, |_, _| {})?;
        on_step(&format!("Unpacking {name}"));
        unzip(&zip_path, &folder)?;
        let _ = std::fs::remove_file(&zip_path);
    }
    if crate::tools::gmsh().is_none() || crate::tools::ccx().is_none() {
        bail!("the solvers are not where they should be after installing");
    }
    Ok(())
}

/// The folder holding our distro's virtual disk.
pub fn distro_dir() -> PathBuf {
    solvers_dir().join("wsl")
}

pub fn distro() -> Distro {
    Distro { name: DISTRO.into(), user: Some(SOLVER_USER.into()) }
}

/// Has our distro been fully set up? Set by the last step of [`install_wsl`].
pub fn wsl_ready() -> bool {
    let d = distro();
    if !d.exists().unwrap_or(false) {
        return false;
    }
    d.script(true, "test -f /opt/sensesim/ready", &Cancel::default(), |_| {}).is_ok()
}

/// The OpenFOAM release solvers are pinned to.
pub const OPENFOAM: &str = "2512";

/// What a shell needs to run OpenFOAM's tools in this distro.
pub fn foam_env() -> String {
    // OpenFOAM's scripts do not survive `set -e`, which Distro::script turns on.
    format!("set +e; source /usr/lib/openfoam/openfoam{OPENFOAM}/etc/bashrc; set -e")
}

/// Creates the distro and installs OpenFOAM and Elmer in it. Long: several GB. The user's own
/// distros are not touched. Each step can be repeated safely, so a failed install can be re-run.
pub fn install_wsl(cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<()> {
    let root = Distro::new(DISTRO);
    if !root.exists()? {
        on_line("Downloading the Ubuntu image");
        let image = solvers_dir().join("rootfs").join("ubuntu-noble-wsl-amd64.tar.gz");
        fetch(&UBUNTU_ROOTFS, &image, cancel, |_, _| {})?;
        on_line("Creating the solver environment");
        root.import(&distro_dir(), &image)?;
    }
    let mut say = |l: &str| on_line(l);
    root.script(
        true,
        &format!(
            "id -u {SOLVER_USER} >/dev/null 2>&1 || useradd -m -s /bin/bash {SOLVER_USER}\nprintf '[interop]\\nappendWindowsPath=false\\n[user]\\ndefault={SOLVER_USER}\\n' > /etc/wsl.conf\nexport DEBIAN_FRONTEND=noninteractive\napt-get update -y\napt-get install -y --no-install-recommends ca-certificates curl gnupg tar\n"
        ),
        cancel,
        &mut say,
    )
    .context("preparing the distro")?;
    // WSL reads wsl.conf at start.
    root.terminate()?;

    root.script(
        true,
        &format!(
            "export DEBIAN_FRONTEND=noninteractive\nif ! dpkg -s openfoam{OPENFOAM}-default >/dev/null 2>&1; then\n  curl -fsSL https://dl.openfoam.com/add-debian-repo.sh | bash\n  apt-get update -y\n  apt-get install -y --no-install-recommends openfoam{OPENFOAM}-default\nfi\n"
        ),
        cancel,
        &mut say,
    )
    .context("installing OpenFOAM")?;

    root.script(
        true,
        "export DEBIAN_FRONTEND=noninteractive\nif ! command -v ElmerSolver >/dev/null 2>&1; then\n  apt-get install -y --no-install-recommends software-properties-common\n  add-apt-repository -y ppa:elmer-csc-ubuntu/elmer-csc-ppa\n  apt-get update -y\n  apt-get install -y --no-install-recommends elmerfem-csc\nfi\n",
        cancel,
        &mut say,
    )
    .context("installing Elmer")?;

    root.install_supervisor()?;
    // Prove it works before calling it ready.
    root.script(
        true,
        &format!("{}\nsimpleFoam -help >/dev/null\nblockMesh -help >/dev/null\nsnappyHexMesh -help >/dev/null\ncommand -v ElmerSolver\ncommand -v ElmerGrid\nmkdir -p /opt/sensesim\ntouch /opt/sensesim/ready\n", foam_env()),
        cancel,
        &mut say,
    )
    .context("checking the installed solvers")?;
    root.terminate()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serves `body` once, on a local port, and says where.
    fn serve_once(body: &'static [u8]) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(body);
        });
        url
    }

    /// The whole way: a real download, of bytes that aren't what the pinned hash says.
    #[test]
    fn a_corrupted_download_is_not_installed_and_leaves_nothing_behind() {
        const ABC: &str = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
        let dir = std::env::temp_dir().join("sense-sim-provision-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let to = dir.join("tool.zip");

        let bad: &'static str = Box::leak(serve_once(b"abd").into_boxed_str());
        let e = fetch(&Download { url: bad, sha256: ABC, size: 3 }, &to, &Cancel::default(), |_, _| {}).unwrap_err().to_string();
        assert!(e.contains("not installed") && e.contains(ABC), "{e}");
        assert!(!to.exists(), "the file wasn't installed");
        assert!(!to.with_extension("part").exists(), "and no partial file is left");

        let good: &'static str = Box::leak(serve_once(b"abc").into_boxed_str());
        fetch(&Download { url: good, sha256: ABC, size: 3 }, &to, &Cancel::default(), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"abc");

        // A file already there with a good hash isn't fetched again: this address no longer answers.
        fetch(&Download { url: good, sha256: ABC, size: 3 }, &to, &Cancel::default(), |_, _| {}).unwrap();
    }

    #[test]
    fn unzip_keeps_inside_its_folder() {
        let dir = std::env::temp_dir().join("sense-sim-unzip-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let z = dir.join("evil.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
            let o: zip::write::SimpleFileOptions = Default::default();
            w.start_file("../escaped.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.start_file("ok/inside.txt", o).unwrap();
            w.write_all(b"y").unwrap();
            w.finish().unwrap();
        }
        let out = dir.join("out");
        unzip(&z, &out).unwrap();
        assert!(out.join("ok/inside.txt").exists());
        assert!(!dir.join("escaped.txt").exists(), "a path left the folder");
    }
}
