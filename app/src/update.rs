//! Self-update from GitHub Releases: a check at startup and every 12 hours
//! (never during playback), the Settings ▸ Update panel with the release
//! notes, and Install: download the installer, verify its ed25519 signature
//! against the key built into the app, run it silently and quit. The
//! installer waits for redjelly to exit and starts the new version.
//!
//! Releases are made by `scripts/release.sh`, which signs the installer.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::Value;

use crate::App;
use crate::models::{Download, fetch};
use crate::subtitles::{Panel, PanelAction, PanelKind, entry, header};

const LATEST_URL: &str = "https://api.github.com/repos/depeter/redjelly/releases/latest";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 3600);
/// Startup is busy signing in; check a little later.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);

/// Public half of the release signing key (`~/.config/redjelly/release-signing.key`
/// on the build box). Rotating it means one manual reinstall.
const PUBLIC_KEY: [u8; 32] = hex32("d0c77dafdef58e78b293b12c8960218a15255b54f79b58174c4fa26ec3b99e82");

const fn hex32(s: &str) -> [u8; 32] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("bad hex"),
        }
    }
    let b = s.as_bytes();
    assert!(b.len() == 64);
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nibble(b[2 * i]) << 4 | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// One line per change.
    pub notes: Vec<String>,
    pub installer_url: String,
    pub installer_name: String,
    pub sig_url: String,
}

/// What the app knows about updates right now.
pub struct UpdateState {
    pub available: Option<Release>,
    pub checking: bool,
    pub installing: bool,
    pub error: Option<String>,
    pub next_check: Instant,
    /// Version already announced with a toast.
    pub announced: Option<String>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            available: None,
            checking: false,
            installing: false,
            error: None,
            next_check: Instant::now() + FIRST_CHECK_DELAY,
            announced: None,
        }
    }
}

/// `"v0.1.12"` / `"0.1.12"` → (0, 1, 12).
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

/// The GitHub "latest release" JSON → a release with a signed installer.
fn parse_release(json: &Value) -> Option<Release> {
    let version = json["tag_name"].as_str()?.trim_start_matches('v').to_owned();
    parse_version(&version)?;
    let assets = json["assets"].as_array()?;
    let url_of = |name: &str| {
        assets.iter().find(|a| a["name"].as_str() == Some(name))?["browser_download_url"].as_str().map(str::to_owned)
    };
    let installer_name = assets.iter().find_map(|a| {
        let n = a["name"].as_str()?;
        (n.starts_with("redjelly-install-") && n.ends_with(".exe")).then(|| n.to_owned())
    })?;
    let notes = json["body"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    Some(Release {
        version,
        notes,
        installer_url: url_of(&installer_name)?,
        sig_url: url_of(&format!("{installer_name}.sig"))?,
        installer_name,
    })
}

/// The latest release, if it's newer than this build.
pub async fn latest(http: &reqwest::Client) -> anyhow::Result<Option<Release>> {
    let resp = http
        .get(LATEST_URL)
        .header("User-Agent", concat!("redjelly/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    // No release published yet.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let json: Value = resp.error_for_status()?.json().await?;
    let release = parse_release(&json).ok_or_else(|| anyhow::anyhow!("latest release has no signed installer"))?;
    Ok(is_newer(&release.version, env!("CARGO_PKG_VERSION")).then_some(release))
}

pub fn verify(data: &[u8], sig: &[u8], key: &[u8; 32]) -> anyhow::Result<()> {
    let key = VerifyingKey::from_bytes(key)?;
    let sig = Signature::from_slice(sig)?;
    key.verify_strict(data, &sig)?;
    Ok(())
}

/// Downloads the installer into `dir` and checks its signature; a file
/// that fails the check is deleted.
pub async fn download(
    http: &reqwest::Client,
    release: &Release,
    dir: &Path,
    status: &(dyn Fn(String, f32) + Send + Sync),
) -> anyhow::Result<PathBuf> {
    let dest = dir.join(&release.installer_name);
    let d = Download { label: format!("redjelly {}", release.version), url: release.installer_url.clone(), dest: dest.clone(), archive: None };
    fetch(http, &d, status, &AtomicBool::new(false)).await?;
    let sig = http.get(&release.sig_url).send().await?.error_for_status()?.bytes().await?;
    let data = tokio::fs::read(&dest).await?;
    if let Err(e) = verify(&data, &sig, &PUBLIC_KEY) {
        let _ = tokio::fs::remove_file(&dest).await;
        anyhow::bail!("the download's signature doesn't match ({e}); not installing it");
    }
    Ok(dest)
}

impl App {
    /// From the UI tick: the periodic check, and the toast once it's quiet.
    pub fn update_tick(&self, now: Instant) {
        let playing = self.session.borrow().is_some();
        if self.standalone || playing {
            return;
        }
        let due = {
            let u = self.update.borrow();
            !u.checking && !u.installing && now >= u.next_check
        };
        if due && self.config.borrow().update.auto_check {
            self.check_for_update(false);
        }
        let announce = {
            let u = self.update.borrow();
            let skipped = self.config.borrow().update.skipped.clone();
            u.available
                .as_ref()
                .filter(|r| u.announced.as_ref() != Some(&r.version) && skipped.as_ref() != Some(&r.version))
                .map(|r| r.version.clone())
        };
        if let Some(v) = announce
            && self.ui().get_toast().is_empty()
        {
            self.update.borrow_mut().announced = Some(v.clone());
            self.toast(format!("redjelly {v} is available · Settings ▸ Update"));
        }
    }

    pub fn check_for_update(&self, manual: bool) {
        {
            let mut u = self.update.borrow_mut();
            if u.checking {
                return;
            }
            u.checking = true;
            u.next_check = Instant::now() + CHECK_INTERVAL;
        }
        self.refresh_update_views();
        let http = self.http.clone();
        self.spawn_ui(async move { latest(&http).await }, move |app, result| {
            {
                let mut u = app.update.borrow_mut();
                u.checking = false;
                match result {
                    Ok(r) => {
                        u.available = r;
                        u.error = None;
                    }
                    Err(e) => {
                        tracing::warn!("update check: {e:#}");
                        u.error = Some(format!("{e:#}"));
                    }
                }
            }
            if manual {
                let u = app.update.borrow();
                let msg = match (&u.available, &u.error) {
                    (_, Some(e)) => format!("Couldn't check for updates: {e}"),
                    (Some(r), None) => format!("redjelly {} is available", r.version),
                    (None, None) => "redjelly is up to date".to_owned(),
                };
                drop(u);
                app.toast(msg);
            }
            app.refresh_update_views();
        });
    }

    /// Value shown on the Settings ▸ Update card.
    pub fn update_summary(&self) -> String {
        let u = self.update.borrow();
        if u.installing {
            "Installing…".into()
        } else if let Some(r) = &u.available {
            format!("{} available", r.version)
        } else if u.checking {
            "Checking…".into()
        } else if u.error.is_some() {
            "Couldn't check".into()
        } else {
            "Check for updates".into()
        }
    }

    pub fn open_update_panel(&self) {
        *self.panel.borrow_mut() = Some(Panel { kind: PanelKind::Update, ..Default::default() });
        let ui = self.ui();
        ui.set_panel_title("Update".into());
        ui.set_panel_visible(true);
        self.refresh_update_panel();
        let has_release = self.update.borrow().available.is_some();
        if !has_release && !self.update.borrow().checking {
            self.check_for_update(false);
        }
    }

    pub fn refresh_update_panel(&self) {
        let u = self.update.borrow();
        let current = env!("CARGO_PKG_VERSION");
        let status = if u.installing {
            "Downloading the update…".to_owned()
        } else if u.checking {
            format!("redjelly {current} · checking for updates…")
        } else if let Some(e) = &u.error {
            format!("redjelly {current} · couldn't check: {e}")
        } else if let Some(r) = &u.available {
            format!("redjelly {current} installed · {} available", r.version)
        } else {
            format!("redjelly {current} is up to date")
        };
        let mut entries = Vec::new();
        if let Some(r) = &u.available {
            if cfg!(windows) {
                entries.push(entry(format!("Install {} now", r.version), "redjelly restarts by itself", PanelAction::UpdateInstall));
            }
            let skipped = self.config.borrow().update.skipped.as_ref() == Some(&r.version);
            if !skipped {
                entries.push(entry("Skip this version", "No more reminders for it", PanelAction::UpdateSkip));
            }
        }
        entries.push(entry("Check again", "", PanelAction::UpdateCheck));
        if let Some(r) = &u.available
            && !r.notes.is_empty()
        {
            entries.push(header(format!("WHAT'S NEW IN {}", r.version)));
            entries.extend(r.notes.iter().map(|n| entry(n.clone(), "", PanelAction::None)));
        }
        drop(u);
        self.ui().set_panel_status(status.into());
        self.show_panel_entries(entries);
    }

    fn refresh_update_views(&self) {
        if self.panel.borrow().as_ref().is_some_and(|p| p.kind == PanelKind::Update) {
            self.refresh_update_panel();
        }
        self.refresh_settings();
    }

    pub fn skip_update(&self) {
        let v = self.update.borrow().available.as_ref().map(|r| r.version.clone());
        self.config.borrow_mut().update.skipped = v;
        self.save_config();
        self.toast("Skipped; Settings ▸ Update can still install it");
        self.refresh_update_views();
    }

    pub fn install_update(&self) {
        let release = {
            let mut u = self.update.borrow_mut();
            let Some(r) = u.available.clone() else { return };
            if u.installing {
                return;
            }
            u.installing = true;
            r
        };
        self.refresh_update_views();
        let http = self.http.clone();
        let dir = self.update_dir.clone();
        let progress = crate::subtitles::toast_progress();
        self.spawn_ui(
            async move {
                tokio::fs::create_dir_all(&dir).await?;
                download(&http, &release, &dir, &move |msg, _| progress(msg)).await
            },
            |app, result: anyhow::Result<PathBuf>| match result.and_then(|p| run_installer(&p)) {
                Ok(()) => {
                    tracing::info!("update installer started; quitting");
                    let _ = slint::quit_event_loop();
                }
                Err(e) => {
                    tracing::error!("update: {e:#}");
                    app.update.borrow_mut().installing = false;
                    app.toast(format!("Update failed: {e:#}"));
                    app.refresh_update_views();
                }
            },
        );
    }
}

/// Starts the installer silently; it waits for us to exit, then relaunches.
#[cfg(windows)]
fn run_installer(path: &Path) -> anyhow::Result<()> {
    std::process::Command::new(path).args(["/S", "/RELAUNCH"]).spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn run_installer(_path: &Path) -> anyhow::Result<()> {
    anyhow::bail!("self-update is only available on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn compares_versions() {
        assert_eq!(parse_version("v0.1.12"), Some((0, 1, 12)));
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.2.3"), None);
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.99"));
        assert!(!is_newer("0.1.9", "0.1.9"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn parses_github_release() {
        let json: Value = serde_json::from_str(
            r#"{
              "tag_name": "v0.1.3",
              "body": "- Fix crash in the audio panel\r\n- Self-update\r\n\r\n",
              "assets": [
                {"name": "redjelly-install-0.1.3.zip", "browser_download_url": "https://x/zip"},
                {"name": "redjelly-install-0.1.3.exe", "browser_download_url": "https://x/exe"},
                {"name": "redjelly-install-0.1.3.exe.sig", "browser_download_url": "https://x/sig"}
              ]
            }"#,
        )
        .unwrap();
        let r = parse_release(&json).unwrap();
        assert_eq!(r.version, "0.1.3");
        assert_eq!(r.notes, ["Fix crash in the audio panel", "Self-update"]);
        assert_eq!(r.installer_url, "https://x/exe");
        assert_eq!(r.sig_url, "https://x/sig");
    }

    #[test]
    fn unsigned_release_is_ignored() {
        let json: Value = serde_json::from_str(
            r#"{"tag_name": "v0.1.3", "assets": [{"name": "redjelly-install-0.1.3.exe", "browser_download_url": "https://x/exe"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_release(&json), None);
    }

    #[test]
    fn checks_signatures() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let public = key.verifying_key().to_bytes();
        let data = b"installer bytes";
        let sig = key.sign(data).to_bytes();
        assert!(verify(data, &sig, &public).is_ok());
        assert!(verify(b"installer bytez", &sig, &public).is_err());
        assert!(verify(data, &sig, &PUBLIC_KEY).is_err());
        assert!(verify(data, &sig[..63], &public).is_err());
    }

    /// `scripts/release.sh` signs with `openssl pkeyutl -sign -rawin`; this
    /// signature came from it (seed = 32 × 0x07).
    #[test]
    fn accepts_openssl_signatures() {
        let public = hex32("ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c");
        let sig = hex32("b532b6bd121f98f5957f51a4fd2cba6adf84771a01c83c4ca57039b88b586d34")
            .into_iter()
            .chain(hex32("8dd2b08b6ad117f349ff970fc309e5d3c0cbe9c03d0b23f0b99c470e0e138f05"))
            .collect::<Vec<_>>();
        assert!(verify(b"installer bytes", &sig, &public).is_ok());
        assert_eq!(SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(), public);
    }

    /// The built-in key is a valid curve point.
    #[test]
    fn public_key_parses() {
        assert!(VerifyingKey::from_bytes(&PUBLIC_KEY).is_ok());
    }
}
