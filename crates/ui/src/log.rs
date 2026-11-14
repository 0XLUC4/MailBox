//! Error log: `<data dir>/logs/mailbox-<date>-<time>-<pid>.log`, one file per run that logged
//! something, the last [`KEEP`] kept. The release exe has no console, so errors and panics are
//! written here; they still go to stderr too (a console in debug builds, nowhere otherwise).

use std::fmt::Display;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Log files kept, this run's included.
const KEEP: usize = 5;

static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Logs panics too. Call once, early in `main`.
pub fn init() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        error(format_args!("panic: {info}\n{backtrace}"));
        default_hook(info);
    }));
}

/// Writes `message` (timestamped) to the log file and stderr. Never fails.
pub fn error(message: impl Display) {
    eprintln!("mailbox: {message}");
    if let Some(file) = FILE.get_or_init(open) {
        let mut file = file.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(
            file,
            "{} {message}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
    }
}

/// Creates this run's log file (on the first error: a quiet run leaves none) and deletes the
/// oldest ones.
fn open() -> Option<Mutex<File>> {
    let dir = crate::data_dir().join("logs");
    let name = format!(
        "mailbox-{}-{}.log",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        std::process::id()
    );
    let file = fs::create_dir_all(&dir)
        .and_then(|()| File::create(dir.join(name)))
        .inspect_err(|err| eprintln!("mailbox: cannot create a log in {}: {err}", dir.display()))
        .ok()?;
    prune(&dir, KEEP);
    Some(Mutex::new(file))
}

/// Deletes all but the `keep` newest `mailbox-*.log` files (names sort by date).
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("mailbox-") && name.ends_with(".log"))
        })
        .collect();
    logs.sort();
    let extra = logs.len().saturating_sub(keep);
    for old in &logs[..extra] {
        let _ = fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn prune_keeps_the_newest_logs() {
        let dir = std::env::temp_dir().join(format!("mailbox-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for day in 1..=7 {
            std::fs::write(dir.join(format!("mailbox-2026110{day}-120000-1.log")), "").unwrap();
        }
        std::fs::write(dir.join("other.txt"), "").unwrap();
        super::prune(&dir, 3);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "mailbox-20261105-120000-1.log",
                "mailbox-20261106-120000-1.log",
                "mailbox-20261107-120000-1.log",
                "other.txt"
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
