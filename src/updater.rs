//! Check GitHub Releases off the UI thread and hand verified updates to the OS installer.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use bevy::prelude::{ResMut, Resource};
use bevy_egui::{EguiContexts, egui};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const API: &str = "https://api.github.com/repos/Nextwaves-Industries/sense-sim/releases";
const TOKEN: &str = "SENSESIM_UPDATE_TOKEN";
const MAX_INSTALLER_SIZE: u64 = 1024 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Clone, Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Clone)]
struct Update {
    version: Version,
    asset: Asset,
}

enum Status {
    Idle,
    Checking,
    Current,
    Available(Update),
    Downloading(Update),
    InstallerOpened,
    Error(String, Option<Update>),
}

#[derive(Resource)]
pub struct Updater {
    status: Status,
    worker: Option<Mutex<Receiver<Result<Status>>>>,
    show_prompt: bool,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            status: Status::Idle,
            worker: None,
            show_prompt: false,
        }
    }
}

pub fn check_on_launch(mut updater: ResMut<Updater>) {
    updater.check();
}

pub fn prompt(mut contexts: EguiContexts, mut updater: ResMut<Updater>) -> bevy::prelude::Result {
    updater.poll();
    if updater.show_prompt {
        let ctx = contexts.ctx_mut()?;
        let mut open = true;
        egui::Window::new("Sense Sim update")
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                updater.controls(ui);
            });
        updater.show_prompt = open;
    }
    Ok(())
}

impl Updater {
    fn start(&mut self, status: Status, work: impl FnOnce() -> Result<Status> + Send + 'static) {
        let (sender, receiver) = mpsc::channel();
        self.status = status;
        match std::thread::Builder::new()
            .name("sense-sim-updater".into())
            .spawn(move || {
                let _ = sender.send(work());
            }) {
            Ok(_) => self.worker = Some(Mutex::new(receiver)),
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn check(&mut self) {
        self.start(Status::Checking, check);
    }

    fn install(&mut self, update: Update) {
        self.start(Status::Downloading(update.clone()), move || {
            download_and_open(&update.asset)?;
            Ok(Status::InstallerOpened)
        });
    }

    fn poll(&mut self) {
        let Some(worker) = &mut self.worker else {
            return;
        };
        let result = worker.get_mut().expect("updater receiver lock").try_recv();
        match result {
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => self.fail("Update worker stopped.".into()),
            Ok(Err(error)) => self.fail(format!("{error:#}")),
            Ok(Ok(status)) => {
                if matches!(status, Status::Available(_)) {
                    self.show_prompt = true;
                }
                self.status = status;
            }
        }
        self.worker = None;
    }

    fn fail(&mut self, message: String) {
        let retry = match &self.status {
            Status::Downloading(update) => Some(update.clone()),
            _ => None,
        };
        self.status = Status::Error(message, retry);
    }

    pub fn settings(&mut self, ui: &mut egui::Ui) {
        ui.label(format!("Installed version {VERSION}"));
        self.controls(ui);
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let mut install = None;
        match &self.status {
            Status::Idle => {}
            Status::Checking => {
                ui.spinner();
                ui.label("Checking for updates…");
            }
            Status::Current => {
                ui.label("Sense Sim is up to date.");
            }
            Status::Available(update) => {
                ui.label(format!("Sense Sim {} is available.", update.version));
                ui.label("Save your work before continuing in the installer.");
                if ui.button("Download and open installer").clicked() {
                    install = Some(update.clone());
                }
            }
            Status::Downloading(_) => {
                ui.spinner();
                ui.label("Downloading and verifying installer…");
            }
            Status::InstallerOpened => {
                ui.label("Installer opened. Save your work and quit Sense Sim before continuing, then reopen it after installation.");
            }
            Status::Error(message, retry) => {
                ui.label(message);
                if let Some(update) = retry
                    && ui.button("Retry download").clicked()
                {
                    install = Some(update.clone());
                }
            }
        }
        if let Some(update) = install {
            self.install(update);
        }
        if self.worker.is_none() && ui.button("Check for updates").clicked() {
            self.check();
        }
    }
}

fn installer_name(version: &Version) -> Option<String> {
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => return Some(format!("SenseSim-Setup-{version}.exe")),
        ("macos", "aarch64") => "arm64",
        ("macos", "x86_64") => "x64",
        _ => return None,
    };
    Some(format!("SenseSim-{version}-macos-{platform}.pkg"))
}

fn github(
    url: &str,
    accept: &str,
    timeout: Duration,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut request = agent
        .get(url)
        .header("User-Agent", "SenseSim")
        .header("Accept", accept)
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Ok(token) = std::env::var(TOKEN)
        && !token.trim().is_empty()
    {
        request = request.header("Authorization", format!("Bearer {}", token.trim()));
    }
    request.call()
}

fn check() -> Result<Status> {
    let current = Version::parse(VERSION)?;
    ensure!(
        installer_name(&current).is_some(),
        "Updates are unavailable for this platform."
    );
    let mut response = match github(
        &format!("{API}/latest"),
        "application/vnd.github+json",
        Duration::from_secs(20),
    ) {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => {
            anyhow::bail!(
                "GitHub has no accessible release. For a private repository, set {TOKEN} and reopen Sense Sim."
            );
        }
        Err(error) => return Err(error).context("Could not check GitHub for updates"),
    };
    let release: Release = serde_json::from_slice(&response.body_mut().read_to_vec()?)?;
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .context("Invalid release tag")?,
    )?;
    if !version.pre.is_empty() || version.cmp_precedence(&current).is_le() {
        return Ok(Status::Current);
    }
    let name = installer_name(&version).context("Updates are unavailable for this platform")?;
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == name)
        .context("The latest release has no installer for this platform")?;
    expected_digest(&asset)?;
    Ok(Status::Available(Update { version, asset }))
}

fn expected_digest(asset: &Asset) -> Result<&str> {
    ensure!(
        (1..=MAX_INSTALLER_SIZE).contains(&asset.size),
        "Invalid installer size."
    );
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .context("GitHub did not provide an installer SHA-256 digest")?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid installer digest."
    );
    Ok(digest)
}

fn download_and_open(asset: &Asset) -> Result<()> {
    let expected = expected_digest(asset)?;
    let directory = tempfile::Builder::new()
        .prefix("sensesim-update-")
        .tempdir()?;
    let path = directory.path().join(&asset.name);
    let mut output = std::fs::File::create_new(&path)?;
    let mut response = github(
        &format!("{API}/assets/{}", asset.id),
        "application/octet-stream",
        Duration::from_secs(900),
    )
    .context("Could not download the installer")?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(asset.size + 1)
        .reader();
    let mut hash = Sha256::new();
    let mut received = 0;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        received += count as u64;
        ensure!(
            received <= asset.size,
            "Installer exceeded its published size."
        );
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    ensure!(received == asset.size, "Installer download was incomplete.");
    ensure!(
        format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected),
        "Installer failed SHA-256 verification."
    );
    drop(output);

    #[cfg(target_os = "windows")]
    std::process::Command::new(&path)
        .env_remove(TOKEN)
        .spawn()
        .context("Could not start the installer")?;
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open")
            .arg(&path)
            .env_remove(TOKEN)
            .status()
            .context("Could not open the installer")?;
        ensure!(status.success(), "macOS could not open the installer.");
    }
    // The OS installer still needs the file after this worker exits.
    let _ = directory.keep();
    Ok(())
}
