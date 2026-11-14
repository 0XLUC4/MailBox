//! In-app updates from the GitHub Releases of Mailbox. No UI, no personal data sent: one
//! anonymous GET on the public "latest release" API, then the two assets.
//!
//! Release convention (README, "Releases"): tag `vX.Y.Z`, assets
//! `Mailbox-X.Y.Z-windows-x64.zip` (with `mailbox.exe` at its root) and `SHA256SUMS.txt`.
//!
//! Test hook: `MAILBOX_UPDATE_FEED=<file.json>` replaces the GitHub answer (same JSON shape);
//! its asset URLs may then be local file paths.

use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// Version of the running app (the workspace version).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Points the check at a local JSON file instead of GitHub (tests, manual checks).
pub const FEED_ENV: &str = "MAILBOX_UPDATE_FEED";

const LATEST_URL: &str = "https://api.github.com/repos/0XLUC4/MailBox/releases/latest";
const USER_AGENT: &str = "Mailbox-updater";
const SUMS_NAME: &str = "SHA256SUMS.txt";
const EXE_NAME: &str = "mailbox.exe";
/// Set on the relaunched process so it waits for the previous one to exit.
const AFTER_UPDATE_ENV: &str = "MAILBOX_AFTER_UPDATE";
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;

/// Why a check or an update failed. [`Error::message`] is short English text (and an i18n key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Offline,
    RateLimited,
    NoRelease,
    BadResponse,
    MissingAsset,
    Checksum,
    DevBuild,
    Unsupported,
    Install(String),
}

impl Error {
    pub fn message(&self) -> &'static str {
        match self {
            Error::Offline => "Offline: could not reach GitHub",
            Error::RateLimited => "GitHub rate limit reached, try again later",
            Error::NoRelease => "No release published yet",
            Error::BadResponse => "Unexpected answer from GitHub",
            Error::MissingAsset => "This release has no Windows package",
            Error::Checksum => "The download is damaged (checksum mismatch)",
            Error::DevBuild => "Development build: updates are installed only in a release build",
            Error::Unsupported => "Updates are installed automatically only on Windows",
            Error::Install(_) => "Could not install the update",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Install(detail) => write!(f, "{}: {detail}", self.message()),
            other => f.write_str(other.message()),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Install(err.to_string())
    }
}

/// `major.minor.patch`, compared numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parses `1.2.3` or `v1.2.3`; anything else (pre-release suffix included) is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches(['v', 'V']).split('.');
        let version = Self {
            major: parts.next()?.parse().ok()?,
            minor: parts.next()?.parse().ok()?,
            patch: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(version)
    }

    pub fn current() -> Self {
        Self::parse(CURRENT_VERSION).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A newer release that can be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub notes: String,
    /// Release page on GitHub ("What's new").
    pub page_url: String,
    pub package_name: String,
    pub package_url: String,
    pub sums_url: String,
}

impl Release {
    /// First lines of the release notes as one plain line, at most `max_chars` long.
    pub fn notes_excerpt(&self, max_chars: usize) -> String {
        let text = self
            .notes
            .lines()
            .map(|line| line.trim().trim_start_matches(['#', '*', '-', '>']).trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.chars().count() <= max_chars {
            return text;
        }
        let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The release zip's file name for `version`.
pub fn package_name(version: Version) -> String {
    format!("Mailbox-{version}-windows-x64.zip")
}

/// Asks GitHub (or [`FEED_ENV`]) for the latest release: `Some` when it is newer than this app.
pub fn check() -> Result<Option<Release>, Error> {
    let json = match std::env::var_os(FEED_ENV) {
        Some(path) => fs::read_to_string(path).map_err(|_| Error::Offline)?,
        None => get(LATEST_URL)?.into_string().map_err(|_| Error::Offline)?,
    };
    parse_latest(&json, Version::current())
}

/// Reads a GitHub "latest release" JSON: `None` when it is not newer than `current`, a draft or
/// a pre-release.
pub fn parse_latest(json: &str, current: Version) -> Result<Option<Release>, Error> {
    let release: GithubRelease = serde_json::from_str(json).map_err(|_| Error::BadResponse)?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = Version::parse(&release.tag_name).ok_or(Error::BadResponse)?;
    if version <= current {
        return Ok(None);
    }
    let package_name = package_name(version);
    let url_of = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
    };
    let (Some(package_url), Some(sums_url)) = (url_of(&package_name), url_of(SUMS_NAME)) else {
        return Err(Error::MissingAsset);
    };
    Ok(Some(Release {
        version,
        notes: release.body.unwrap_or_default(),
        page_url: release.html_url,
        package_name,
        package_url,
        sums_url,
    }))
}

/// Lowercase SHA-256 of `file_name` in a `sha256sum` list (`<hex>  <name>` or `<hex> *<name>`).
pub fn checksum_for(sums: &str, file_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        let valid = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        (valid && name == file_name).then(|| hash.to_ascii_lowercase())
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// True for an exe built by cargo (`target*/debug` or `target*/release`): never replaced.
pub fn is_dev_build(exe: &Path) -> bool {
    let mut dirs = exe
        .ancestors()
        .skip(1)
        .filter_map(Path::file_name)
        .map(|name| name.to_string_lossy().to_ascii_lowercase());
    match (dirs.next(), dirs.next()) {
        (Some(profile), Some(target)) => {
            matches!(profile.as_str(), "debug" | "release") && target.starts_with("target")
        }
        _ => false,
    }
}

/// The running exe, when it may be updated (release / portable install).
pub fn installed_exe() -> Result<PathBuf, Error> {
    if !cfg!(windows) {
        return Err(Error::Unsupported);
    }
    let exe = std::env::current_exe()?;
    if is_dev_build(&exe) {
        return Err(Error::DevBuild);
    }
    Ok(exe)
}

/// Downloads `release`, checks it against `SHA256SUMS.txt`, puts it in place of the running
/// exe and starts it with the same arguments. On `Ok` the caller must quit right away; on any
/// error the installed exe is left (or put back) as it was.
pub fn install(release: &Release) -> Result<(), Error> {
    let exe = installed_exe()?;
    let package = fetch(&release.package_url)?;
    let sums = String::from_utf8(fetch(&release.sums_url)?).map_err(|_| Error::BadResponse)?;
    let expected = checksum_for(&sums, &release.package_name).ok_or(Error::Checksum)?;
    if sha256_hex(&package) != expected {
        return Err(Error::Checksum);
    }

    let work = std::env::temp_dir().join(format!("mailbox-update-{}", release.version));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let result = (|| {
        let zip = work.join(&release.package_name);
        fs::write(&zip, &package)?;
        let unpacked = work.join("unpacked");
        extract_zip(&zip, &unpacked)?;
        let new_exe = unpacked.join(EXE_NAME);
        if !new_exe.is_file() {
            return Err(Error::MissingAsset);
        }
        swap_exe(&exe, &new_exe)?;
        if let Err(err) = relaunch(&exe) {
            let _ = rollback(&exe);
            return Err(err);
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

/// Where the running exe goes during an update: `<name>.old.exe` next to it.
pub fn old_exe_path(exe: &Path) -> PathBuf {
    sibling(exe, "old.exe")
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let stem = exe
        .file_stem()
        .map_or_else(|| "mailbox".into(), |stem| stem.to_string_lossy());
    exe.with_file_name(format!("{stem}.{suffix}"))
}

/// Puts `new_exe` at `exe`, keeping the current one as `<name>.old.exe` (Windows lets a running
/// exe be renamed, not overwritten). Anything that fails leaves `exe` as it was.
pub fn swap_exe(exe: &Path, new_exe: &Path) -> Result<(), Error> {
    let staged = sibling(exe, "new.exe");
    let old = old_exe_path(exe);
    // Staged next to the exe so the final rename never crosses volumes.
    fs::copy(new_exe, &staged)?;
    let _ = fs::remove_file(&old);
    if let Err(err) = fs::rename(exe, &old) {
        let _ = fs::remove_file(&staged);
        return Err(err.into());
    }
    if let Err(err) = fs::rename(&staged, exe) {
        let _ = fs::remove_file(&staged);
        let _ = fs::rename(&old, exe);
        return Err(err.into());
    }
    Ok(())
}

/// Undoes [`swap_exe`]: the previous exe goes back in place.
pub fn rollback(exe: &Path) -> Result<(), Error> {
    let old = old_exe_path(exe);
    if !old.is_file() {
        return Err(Error::Install("no previous version to restore".into()));
    }
    if exe.exists() {
        fs::remove_file(exe)?;
    }
    fs::rename(old, exe)?;
    Ok(())
}

/// Call first thing in `main`: deletes the `<name>.old.exe` an update left. Right after an
/// update the previous process may still be closing (its exe stays locked), so this waits for
/// it a few seconds; that also keeps the new instance from handing itself to the old one.
pub fn finish_pending_update() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let old = old_exe_path(&exe);
    let after_update = std::env::var_os(AFTER_UPDATE_ENV).is_some();
    for _ in 0..75 {
        if !old.exists() || fs::remove_file(&old).is_ok() || !after_update {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn relaunch(exe: &Path) -> Result<(), Error> {
    Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(AFTER_UPDATE_ENV, "1")
        .spawn()?;
    Ok(())
}

/// Unzips with the `tar.exe` shipped with Windows 10+ (it refuses paths leaving `into`).
fn extract_zip(zip: &Path, into: &Path) -> Result<(), Error> {
    fs::create_dir_all(into)?;
    let system_root =
        std::env::var_os("SystemRoot").map_or_else(|| "C:\\Windows".into(), PathBuf::from);
    let mut command = Command::new(system_root.join("System32").join("tar.exe"));
    command.arg("-xf").arg(zip).arg("-C").arg(into);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let status = command.status()?;
    if !status.success() {
        return Err(Error::Install(format!("unzip failed ({status})")));
    }
    Ok(())
}

fn get(url: &str) -> Result<ureq::Response, Error> {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build()
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(403 | 429, _) => Error::RateLimited,
            ureq::Error::Status(404, _) => Error::NoRelease,
            ureq::Error::Status(..) => Error::BadResponse,
            ureq::Error::Transport(_) => Error::Offline,
        })
}

/// Downloads `url`; with the [`FEED_ENV`] test hook, a non-HTTPS URL is a local file path.
fn fetch(url: &str) -> Result<Vec<u8>, Error> {
    if !url.starts_with("https://") {
        if std::env::var_os(FEED_ENV).is_none() {
            return Err(Error::BadResponse);
        }
        return Ok(fs::read(url)?);
    }
    let mut bytes = Vec::new();
    get(url)?
        .into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Offline)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release_json(tag: &str, draft: bool, prerelease: bool, assets: &[&str]) -> String {
        let assets: Vec<_> = assets
            .iter()
            .map(|name| {
                serde_json::json!({ "name": name, "browser_download_url": format!("https://x/{name}") })
            })
            .collect();
        serde_json::json!({
            "tag_name": tag, "body": "## Fixes\n- faster sync\n", "html_url": "https://page",
            "draft": draft, "prerelease": prerelease, "assets": assets,
        })
        .to_string()
    }

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.1") > v("0.1.0"));
        assert_eq!(v("v2.0.10").to_string(), "2.0.10");
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1.2.3-beta", "latest"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::current().to_string(), CURRENT_VERSION);
    }

    #[test]
    fn finds_a_newer_release_and_its_assets() {
        let json = release_json(
            "v0.2.0",
            false,
            false,
            &[
                "Mailbox-0.2.0-windows-x64.zip",
                "SHA256SUMS.txt",
                "other.zip",
            ],
        );
        let release = parse_latest(&json, v("0.1.0")).unwrap().unwrap();
        assert_eq!(release.version, v("0.2.0"));
        assert_eq!(release.package_name, "Mailbox-0.2.0-windows-x64.zip");
        assert_eq!(
            release.package_url,
            "https://x/Mailbox-0.2.0-windows-x64.zip"
        );
        assert_eq!(release.sums_url, "https://x/SHA256SUMS.txt");
        assert_eq!(release.page_url, "https://page");
        assert_eq!(release.notes_excerpt(100), "Fixes faster sync");
        assert_eq!(release.notes_excerpt(8), "Fixes f…");
    }

    #[test]
    fn skips_old_draft_and_prerelease() {
        let assets = ["Mailbox-0.2.0-windows-x64.zip", "SHA256SUMS.txt"];
        let same = release_json("v0.2.0", false, false, &assets);
        assert_eq!(parse_latest(&same, v("0.2.0")), Ok(None));
        assert_eq!(parse_latest(&same, v("0.3.0")), Ok(None));
        let draft = release_json("v0.2.0", true, false, &assets);
        assert_eq!(parse_latest(&draft, v("0.1.0")), Ok(None));
        let pre = release_json("v0.2.0", false, true, &assets);
        assert_eq!(parse_latest(&pre, v("0.1.0")), Ok(None));
    }

    #[test]
    fn reports_missing_assets_and_bad_json() {
        let wrong_version = release_json(
            "v0.2.0",
            false,
            false,
            &["Mailbox-0.1.9-windows-x64.zip", "SHA256SUMS.txt"],
        );
        assert_eq!(
            parse_latest(&wrong_version, v("0.1.0")),
            Err(Error::MissingAsset)
        );
        let no_sums = release_json("v0.2.0", false, false, &["Mailbox-0.2.0-windows-x64.zip"]);
        assert_eq!(parse_latest(&no_sums, v("0.1.0")), Err(Error::MissingAsset));
        assert_eq!(parse_latest("{}", v("0.1.0")), Err(Error::BadResponse));
        let bad_tag = release_json("nightly", false, false, &[]);
        assert_eq!(parse_latest(&bad_tag, v("0.1.0")), Err(Error::BadResponse));
    }

    #[test]
    fn parses_checksum_lists() {
        let hash = "A".repeat(64);
        let sums = format!(
            "{}  other.zip\n{hash}  Mailbox-0.2.0-windows-x64.zip\n{} *star.zip\nbad  short.zip\n",
            "0".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(
            checksum_for(&sums, "Mailbox-0.2.0-windows-x64.zip"),
            Some("a".repeat(64))
        );
        assert_eq!(checksum_for(&sums, "star.zip"), Some("b".repeat(64)));
        assert_eq!(checksum_for(&sums, "short.zip"), None);
        assert_eq!(checksum_for(&sums, "missing.zip"), None);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn recognizes_cargo_builds() {
        assert!(is_dev_build(Path::new(
            r"C:\src\mailbox\target\debug\mailbox.exe"
        )));
        assert!(is_dev_build(Path::new(
            r"C:\src\target-update\release\mailbox.exe"
        )));
        assert!(is_dev_build(Path::new("/src/target/release/mailbox")));
        assert!(!is_dev_build(Path::new(r"C:\Apps\Mailbox\mailbox.exe")));
        assert!(!is_dev_build(Path::new(r"C:\Users\me\release\mailbox.exe")));
        assert!(!is_dev_build(Path::new("mailbox.exe")));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mailbox-updater-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn swaps_the_exe_and_rolls_back() {
        let dir = temp_dir("swap");
        let exe = dir.join("Mailbox.exe");
        let new_exe = dir.join("download").join("mailbox.exe");
        fs::create_dir_all(new_exe.parent().unwrap()).unwrap();
        fs::write(&exe, "v1").unwrap();
        fs::write(&new_exe, "v2").unwrap();
        fs::write(old_exe_path(&exe), "stale").unwrap();

        swap_exe(&exe, &new_exe).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v2");
        assert_eq!(
            fs::read_to_string(dir.join("Mailbox.old.exe")).unwrap(),
            "v1"
        );
        assert!(!dir.join("Mailbox.new.exe").exists());

        rollback(&exe).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v1");
        assert!(!old_exe_path(&exe).exists());
        assert!(rollback(&exe).is_err(), "nothing left to restore");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_swap_leaves_the_exe_untouched() {
        let dir = temp_dir("fail");
        let exe = dir.join("mailbox.exe");
        fs::write(&exe, "v1").unwrap();
        assert!(swap_exe(&exe, &dir.join("missing.exe")).is_err());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v1");
        assert!(!old_exe_path(&exe).exists());

        // The exe itself is gone (cannot be renamed): the staged copy is cleaned up.
        let ghost = dir.join("ghost.exe");
        fs::write(dir.join("new.bin"), "v2").unwrap();
        assert!(swap_exe(&ghost, &dir.join("new.bin")).is_err());
        assert!(!dir.join("ghost.new.exe").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
