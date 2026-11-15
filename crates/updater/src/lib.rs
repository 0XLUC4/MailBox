//! In-app updates from the GitHub Releases of Mailbox. No UI, no personal data sent: one
//! anonymous GET of the latest release's `latest.json` (a plain download that follows redirects,
//! not the rate-limited GitHub API), then the exe it points to.
//!
//! Release convention (README, "Releasing"): tag `vX.Y.Z`, assets
//! `Mailbox-X.Y.Z-windows-x64.exe`, `Mailbox-X.Y.Z-macos-arm64.app.tar.gz` and `latest.json`:
//! `{ "version", "pub_date", "notes", "platforms": { "<platform>": { "url", "sha256", "size" } } }`
//! with `windows-x86_64` (the exe) and `darwin-aarch64` (Mailbox.app as a tar.gz).
//!
//! Windows swaps the running exe; macOS swaps the running `Mailbox.app` bundle. Elsewhere
//! (Linux) a newer release is only announced.
//!
//! Test hook: `MAILBOX_UPDATE_FEED=<path or URL of a latest.json>` replaces the GitHub manifest;
//! its asset URL may then be a local file path.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// Version of the running app (the workspace version).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Points the check at another `latest.json` (local path or URL) instead of GitHub.
pub const FEED_ENV: &str = "MAILBOX_UPDATE_FEED";
/// The `platforms` key this build installs; `None`: no in-place install (only announced).
pub const PLATFORM: Option<&str> = if cfg!(windows) {
    Some("windows-x86_64")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("darwin-aarch64")
} else {
    None
};
/// Whether this build installs updates itself.
pub const SUPPORTED: bool = PLATFORM.is_some();

const MANIFEST_URL: &str = "https://github.com/0XLUC4/MailBox/releases/latest/download/latest.json";
const RELEASE_PAGE_URL: &str = "https://github.com/0XLUC4/MailBox/releases/tag/v";
const USER_AGENT: &str = "Mailbox-updater";
/// Set on the relaunched process so it waits for the previous one to exit.
const AFTER_UPDATE_ENV: &str = "MAILBOX_AFTER_UPDATE";
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;

/// Why a check or an update failed. [`Error::message`] is short English text (and an i18n key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Offline,
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
            Error::NoRelease => "No release published yet",
            Error::BadResponse => "Unexpected answer from GitHub",
            Error::MissingAsset => "This release has no package for this system",
            Error::Checksum => "The download is damaged (checksum mismatch)",
            Error::DevBuild => "Development build: updates are installed only in a release build",
            Error::Unsupported => "Updates are installed automatically only on Windows and macOS",
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
    /// The new exe (HTTPS, or a local path with the [`FEED_ENV`] hook).
    pub package_url: String,
    /// Lowercase hex SHA-256 of the exe.
    pub sha256: String,
    /// Size of the exe in bytes.
    pub size: u64,
}

impl Release {
    /// The bullet lines of the release notes (`- …` or `* …`) as `- …`, at most `max`;
    /// without bullets, the first line of the notes (headings unmarked).
    pub fn highlights(&self, max: usize) -> Vec<String> {
        let lines = self.notes.lines().map(str::trim);
        let bullets: Vec<String> = lines
            .clone()
            .filter_map(|line| line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")))
            .map(|item| format!("- {}", item.trim()))
            .take(max)
            .collect();
        if !bullets.is_empty() {
            return bullets;
        }
        lines
            .map(|line| line.trim_start_matches('#').trim())
            .find(|line| !line.is_empty())
            .map(|line| vec![line.to_owned()])
            .unwrap_or_default()
    }
}

/// `latest.json`, written by `scripts/package.ps1` (`pub_date` is informative only).
#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    platforms: HashMap<String, Asset>,
}

#[derive(Deserialize)]
struct Asset {
    url: String,
    sha256: String,
    size: u64,
}

/// Fetches `latest.json` (or [`FEED_ENV`]): `Some` when it announces a newer version.
pub fn check() -> Result<Option<Release>, Error> {
    let json = match std::env::var(FEED_ENV) {
        Ok(feed) if is_url(&feed) => get_text(&feed)?,
        Ok(path) => fs::read_to_string(path).map_err(|_| Error::Offline)?,
        Err(_) => get_text(MANIFEST_URL)?,
    };
    parse_manifest(&json, Version::current())
}

/// Reads a `latest.json`: `None` when its version is not newer than `current`.
pub fn parse_manifest(json: &str, current: Version) -> Result<Option<Release>, Error> {
    parse_manifest_for(json, current, PLATFORM)
}

/// [`parse_manifest`] for the `platform` key; `None` only reads the version and notes.
pub fn parse_manifest_for(
    json: &str,
    current: Version,
    platform: Option<&str>,
) -> Result<Option<Release>, Error> {
    // Windows PowerShell may write a BOM, which serde_json rejects.
    let json = json.trim_start_matches('\u{feff}');
    let manifest: Manifest = serde_json::from_str(json).map_err(|_| Error::BadResponse)?;
    let version = Version::parse(&manifest.version).ok_or(Error::BadResponse)?;
    if version <= current {
        return Ok(None);
    }
    let notes = manifest.notes.unwrap_or_default();
    let page_url = format!("{RELEASE_PAGE_URL}{version}");
    let Some(platform) = platform else {
        return Ok(Some(Release {
            version,
            notes,
            page_url,
            package_url: String::new(),
            sha256: String::new(),
            size: 0,
        }));
    };
    let mut platforms = manifest.platforms;
    let asset = platforms.remove(platform).ok_or(Error::MissingAsset)?;
    let sha256 = asset.sha256.trim().to_ascii_lowercase();
    let valid_hash = sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit());
    if asset.url.trim().is_empty() || !valid_hash || asset.size == 0 {
        return Err(Error::BadResponse);
    }
    Ok(Some(Release {
        version,
        notes,
        page_url,
        package_url: asset.url,
        sha256,
        size: asset.size,
    }))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Checks a downloaded exe against the size and SHA-256 announced by `release`.
pub fn verify(bytes: &[u8], release: &Release) -> Result<(), Error> {
    if bytes.len() as u64 != release.size || sha256_hex(bytes) != release.sha256 {
        return Err(Error::Checksum);
    }
    Ok(())
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
    if !SUPPORTED {
        return Err(Error::Unsupported);
    }
    let exe = std::env::current_exe()?;
    if is_dev_build(&exe) {
        return Err(Error::DevBuild);
    }
    Ok(exe)
}

/// Downloads the package of `release`, checks its size and SHA-256, puts it in place of the
/// running app (the exe on Windows, `Mailbox.app` on macOS) and starts the new one with the
/// same arguments. `on_progress` receives the bytes downloaded so far (out of `release.size`).
/// On `Ok` the caller must quit right away; on any error the installed app is left (or put
/// back) as it was.
pub fn install(release: &Release, on_progress: impl Fn(u64)) -> Result<(), Error> {
    let exe = installed_exe()?;
    #[cfg(target_os = "macos")]
    let bundle = macos::bundle_of(&exe)
        .ok_or_else(|| Error::Install("not running from Mailbox.app".into()))?;
    let bytes = fetch(&release.package_url, release.size, on_progress)?;
    verify(&bytes, release)?;
    #[cfg(target_os = "macos")]
    return macos::install(&exe, &bundle, &bytes);
    #[cfg(not(target_os = "macos"))]
    install_exe(&exe, &bytes)
}

/// Windows: the downloaded exe replaces the running one.
#[cfg(not(target_os = "macos"))]
fn install_exe(exe: &Path, bytes: &[u8]) -> Result<(), Error> {
    // Staged next to the exe so the final rename never crosses volumes.
    let staged = staged_exe_path(exe);
    if let Err(err) = fs::write(&staged, bytes) {
        let _ = fs::remove_file(&staged);
        return Err(err.into());
    }
    swap_exe(exe, &staged)?;
    if let Err(err) = relaunch(exe) {
        let _ = rollback(exe);
        return Err(err);
    }
    Ok(())
}

/// Where the running exe goes during an update: `<name>.old.exe` next to it.
pub fn old_exe_path(exe: &Path) -> PathBuf {
    sibling(exe, "old.exe")
}

/// Where the downloaded exe waits for the swap: `<name>.new.exe` next to it.
pub fn staged_exe_path(exe: &Path) -> PathBuf {
    sibling(exe, "new.exe")
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let stem = exe
        .file_stem()
        .map_or_else(|| "mailbox".into(), |stem| stem.to_string_lossy());
    exe.with_file_name(format!("{stem}.{suffix}"))
}

/// Moves `staged` (in the exe's folder) to `exe`, keeping the current one as `<name>.old.exe`
/// (Windows lets a running exe be renamed, not overwritten). On failure `exe` is left as it was
/// and `staged` is deleted.
pub fn swap_exe(exe: &Path, staged: &Path) -> Result<(), Error> {
    let old = old_exe_path(exe);
    let _ = fs::remove_file(&old);
    if let Err(err) = fs::rename(exe, &old) {
        let _ = fs::remove_file(staged);
        return Err(err.into());
    }
    if let Err(err) = fs::rename(staged, exe) {
        let _ = fs::remove_file(staged);
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
    #[cfg(target_os = "macos")]
    let Some(old) = macos::bundle_of(&exe).map(|bundle| macos::old_bundle_path(&bundle)) else {
        return;
    };
    #[cfg(not(target_os = "macos"))]
    let old = old_exe_path(&exe);
    let after_update = std::env::var_os(AFTER_UPDATE_ENV).is_some();
    let remove = |path: &Path| {
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    };
    for _ in 0..75 {
        if !old.exists() || remove(&old).is_ok() || !after_update {
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

/// macOS: the app is a bundle (`Mailbox.app/Contents/MacOS/mailbox`); the release ships it as
/// a tar.gz, unpacked next to the installed bundle, then the two bundles are swapped.
#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    /// `…/Mailbox.app` for an exe at `…/Mailbox.app/Contents/MacOS/<name>`.
    pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
        let bundle = exe.parent()?.parent()?.parent()?;
        (bundle.extension()? == "app").then(|| bundle.to_path_buf())
    }

    /// Where the running bundle goes during an update: `<name>.old.app` next to it.
    pub fn old_bundle_path(bundle: &Path) -> PathBuf {
        let stem = bundle
            .file_stem()
            .map_or_else(|| "Mailbox".into(), |stem| stem.to_string_lossy());
        bundle.with_file_name(format!("{stem}.old.app"))
    }

    pub fn install(exe: &Path, bundle: &Path, archive: &[u8]) -> Result<(), Error> {
        let dir = bundle
            .parent()
            .ok_or_else(|| Error::Install("no folder around Mailbox.app".into()))?;
        // Unpacked next to the bundle so the final rename never crosses volumes.
        let staging = dir.join(".mailbox-update");
        let _ = fs::remove_dir_all(&staging);
        let result = unpack_and_swap(exe, bundle, &staging, archive);
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn unpack_and_swap(
        exe: &Path,
        bundle: &Path,
        staging: &Path,
        archive: &[u8],
    ) -> Result<(), Error> {
        fs::create_dir_all(staging)?;
        let archive_path = staging.join("update.tar.gz");
        fs::write(&archive_path, archive)?;
        let status = Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(staging)
            .status()?;
        if !status.success() {
            return Err(Error::Install(format!("tar failed ({status})")));
        }
        let staged = fs::read_dir(staging)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
            .ok_or_else(|| Error::Install("no .app in the update".into()))?;
        // The exe inside the new bundle, at the same place as the running one.
        let relative = exe
            .strip_prefix(bundle)
            .map_err(|_| Error::Install("exe outside its bundle".into()))?
            .to_path_buf();
        if !staged.join(&relative).is_file() {
            return Err(Error::Install(
                "the update has no Mailbox executable".into(),
            ));
        }

        let old = old_bundle_path(bundle);
        let _ = fs::remove_dir_all(&old);
        fs::rename(bundle, &old)?;
        if let Err(err) = fs::rename(&staged, bundle) {
            let _ = fs::rename(&old, bundle);
            return Err(err.into());
        }
        if let Err(err) = relaunch(&bundle.join(&relative)) {
            let _ = fs::remove_dir_all(bundle);
            let _ = fs::rename(&old, bundle);
            return Err(err);
        }
        Ok(())
    }
}

fn is_url(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

/// GET that follows redirects (`releases/latest/download/...` answers with 302s).
fn get(url: &str) -> Result<ureq::Response, Error> {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .redirects(10)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build()
        .get(url)
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(404, _) => Error::NoRelease,
            ureq::Error::Status(..) => Error::BadResponse,
            ureq::Error::Transport(_) => Error::Offline,
        })
}

fn get_text(url: &str) -> Result<String, Error> {
    get(url)?.into_string().map_err(|_| Error::Offline)
}

/// Downloads at most `size + 1` bytes of `url` (an oversized file then fails [`verify`]),
/// reporting the bytes read so far; with the [`FEED_ENV`] test hook, a non-HTTPS URL is a local
/// file path.
fn fetch(url: &str, size: u64, on_progress: impl Fn(u64)) -> Result<Vec<u8>, Error> {
    if size > MAX_DOWNLOAD {
        return Err(Error::BadResponse);
    }
    if !url.starts_with("https://") {
        if std::env::var_os(FEED_ENV).is_none() {
            return Err(Error::BadResponse);
        }
        let bytes = fs::read(url)?;
        on_progress(bytes.len() as u64);
        return Ok(bytes);
    }
    read_with_progress(get(url)?.into_reader().take(size + 1), on_progress)
}

fn read_with_progress(mut reader: impl Read, on_progress: impl Fn(u64)) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(read) => {
                bytes.extend_from_slice(&chunk[..read]);
                on_progress(bytes.len() as u64);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Error::Offline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    const HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const WINDOWS: &str = "windows-x86_64";

    /// The tests read manifests as the Windows build does.
    fn parse_manifest(json: &str, current: Version) -> Result<Option<Release>, Error> {
        parse_manifest_for(json, current, Some(WINDOWS))
    }

    #[test]
    fn announces_without_a_package_where_updates_are_not_installed() {
        let json = manifest("0.2.0", serde_json::json!({}));
        let release = parse_manifest_for(&json, v("0.1.0"), None)
            .unwrap()
            .unwrap();
        assert_eq!(release.version, v("0.2.0"));
        assert!(release.package_url.is_empty());
        assert_eq!(parse_manifest_for(&json, v("0.2.0"), None), Ok(None));
    }

    fn manifest(version: &str, platforms: serde_json::Value) -> String {
        serde_json::json!({
            "version": version, "pub_date": "2026-11-14T10:00:00Z",
            "notes": "## Fixes\n- faster sync\n", "platforms": platforms,
        })
        .to_string()
    }

    fn windows_asset(url: &str, sha256: &str, size: u64) -> serde_json::Value {
        serde_json::json!({ WINDOWS: { "url": url, "sha256": sha256, "size": size } })
    }

    fn release(sha256: &str, size: u64) -> Release {
        Release {
            version: v("0.2.0"),
            notes: String::new(),
            page_url: String::new(),
            package_url: String::new(),
            sha256: sha256.into(),
            size,
        }
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
    fn parses_a_newer_manifest() {
        let url = "https://github.com/0XLUC4/MailBox/releases/download/v0.2.0/Mailbox-0.2.0-windows-x64.exe";
        let mut platforms = windows_asset(url, &HASH.to_ascii_uppercase(), 1234);
        platforms["linux-x86_64"] =
            serde_json::json!({ "url": "https://x/l", "sha256": HASH, "size": 1 });
        let json = format!("\u{feff}{}", manifest("0.2.0", platforms));
        let release = parse_manifest(&json, v("0.1.0")).unwrap().unwrap();
        assert_eq!(release.version, v("0.2.0"));
        assert_eq!(release.package_url, url);
        assert_eq!(release.sha256, HASH, "lowercased");
        assert_eq!(release.size, 1234);
        assert_eq!(
            release.page_url,
            "https://github.com/0XLUC4/MailBox/releases/tag/v0.2.0"
        );
        assert_eq!(release.highlights(5), ["- faster sync"]);
    }

    #[test]
    fn ignores_versions_that_are_not_newer() {
        let same = manifest("0.2.0", windows_asset("https://x/e.exe", HASH, 1));
        assert_eq!(parse_manifest(&same, v("0.2.0")), Ok(None));
        assert_eq!(parse_manifest(&same, v("0.3.0")), Ok(None));
        // An older manifest needs no Windows entry at all.
        let old = manifest("0.1.0", serde_json::json!({}));
        assert_eq!(parse_manifest(&old, v("0.2.0")), Ok(None));
    }

    #[test]
    fn rejects_missing_platform_and_bad_manifests() {
        let other_only = manifest(
            "0.2.0",
            serde_json::json!({ "darwin-aarch64": { "url": "https://x/m", "sha256": HASH, "size": 1 } }),
        );
        assert_eq!(
            parse_manifest(&other_only, v("0.1.0")),
            Err(Error::MissingAsset)
        );
        let no_platforms = manifest("0.2.0", serde_json::json!({}));
        assert_eq!(
            parse_manifest(&no_platforms, v("0.1.0")),
            Err(Error::MissingAsset)
        );
        for platforms in [
            windows_asset("https://x/e.exe", "abc", 1),
            windows_asset("https://x/e.exe", &"z".repeat(64), 1),
            windows_asset("", HASH, 1),
            windows_asset("https://x/e.exe", HASH, 0),
        ] {
            let json = manifest("0.2.0", platforms);
            assert_eq!(
                parse_manifest(&json, v("0.1.0")),
                Err(Error::BadResponse),
                "{json}"
            );
        }
        assert_eq!(parse_manifest("{}", v("0.1.0")), Err(Error::BadResponse));
        assert_eq!(
            parse_manifest("not json", v("0.1.0")),
            Err(Error::BadResponse)
        );
        let bad_version = manifest("nightly", windows_asset("https://x/e.exe", HASH, 1));
        assert_eq!(
            parse_manifest(&bad_version, v("0.1.0")),
            Err(Error::BadResponse)
        );
    }

    #[test]
    fn verifies_size_and_checksum() {
        assert_eq!(sha256_hex(b"abc"), HASH);
        assert_eq!(verify(b"abc", &release(HASH, 3)), Ok(()));
        assert_eq!(verify(b"abc", &release(HASH, 4)), Err(Error::Checksum));
        assert_eq!(verify(b"abd", &release(HASH, 3)), Err(Error::Checksum));
        assert_eq!(verify(b"abcd", &release(HASH, 3)), Err(Error::Checksum));
    }

    #[test]
    fn highlights_fall_back_to_the_first_line() {
        let mut release = release(HASH, 1);
        release.notes = "Styled mail on macOS

More details."
            .into();
        assert_eq!(release.highlights(3), ["Styled mail on macOS"]);
        release.notes = "## Fixes
* one
- two
- three"
            .into();
        assert_eq!(release.highlights(2), ["- one", "- two"]);
        release.notes = String::new();
        assert!(release.highlights(3).is_empty());
    }

    #[test]
    fn reports_download_progress() {
        let data = vec![7u8; 150 * 1024];
        let seen = std::cell::RefCell::new(Vec::new());
        let bytes = read_with_progress(&data[..], |done| seen.borrow_mut().push(done)).unwrap();
        assert_eq!(bytes, data);
        let seen = seen.into_inner();
        assert_eq!(seen.last(), Some(&(data.len() as u64)));
        assert!(seen.windows(2).all(|pair| pair[0] < pair[1]), "{seen:?}");
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
        let staged = staged_exe_path(&exe);
        assert_eq!(staged, dir.join("Mailbox.new.exe"));
        fs::write(&exe, "v1").unwrap();
        fs::write(&staged, "v2").unwrap();
        fs::write(old_exe_path(&exe), "stale").unwrap();

        swap_exe(&exe, &staged).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v2");
        assert_eq!(
            fs::read_to_string(dir.join("Mailbox.old.exe")).unwrap(),
            "v1"
        );
        assert!(!staged.exists());

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
        assert!(swap_exe(&exe, &staged_exe_path(&exe)).is_err());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v1");
        assert!(!old_exe_path(&exe).exists());

        // The exe itself is gone (cannot be renamed): the staged download is cleaned up.
        let ghost = dir.join("ghost.exe");
        let staged = staged_exe_path(&ghost);
        fs::write(&staged, "v2").unwrap();
        assert!(swap_exe(&ghost, &staged).is_err());
        assert!(!staged.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
