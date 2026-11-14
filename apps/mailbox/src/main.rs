//! `mailbox [--sample] [--data <path>] [mailto:...]`
//!
//! - `--data <path>`: SQLite database file (default: `<data dir>/mailbox/mailbox.db`).
//! - `--sample`: fill the database with sample data if it is empty. Without `--data`,
//!   the sample runs in memory so your real database is never touched.
//! - `mailto:...`: open compose (how Windows launches the default mail app). When Mailbox is
//!   already running, the URL is handed to that window and this process exits.

use std::path::PathBuf;
use std::process::ExitCode;

use mailbox_core::{Store, sample};

const USAGE: &str = "usage: mailbox [--sample] [--data <path>] [mailto:...]";

#[derive(Debug, Default)]
struct Args {
    sample: bool,
    data: Option<PathBuf>,
    mailto: Option<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sample" => parsed.sample = true,
            "--data" => {
                let path = args.next().ok_or("--data needs a path")?;
                parsed.data = Some(PathBuf::from(path));
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if mailbox_ui::system_integration::mailto_from_args([other]).is_some() => {
                parsed.mailto = Some(other.to_owned())
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok(parsed)
}

/// Platform data directory: %APPDATA% on Windows, $XDG_DATA_HOME or ~/.local/share elsewhere.
fn default_data_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("mailbox").join("mailbox.db")
}

fn open_store(args: &Args) -> Result<Store, String> {
    let store = match (&args.data, args.sample) {
        (Some(path), _) => Store::open(path),
        (None, true) => Store::open_in_memory(),
        (None, false) => {
            let path = default_data_path();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
            }
            Store::open(&path)
        }
    }
    .map_err(|err| format!("cannot open database: {err}"))?;

    let is_empty = store
        .list_accounts()
        .map_err(|err| err.to_string())?
        .is_empty();
    if args.sample && is_empty {
        sample::generate(&store, chrono::Utc::now())
            .map_err(|err| format!("cannot generate sample data: {err}"))?;
    }
    Ok(store)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    // Single instance: a running Mailbox takes the request (and comes to the front).
    if mailbox_ui::system_integration::forward_to_running(args.mailto.as_deref()) {
        return ExitCode::SUCCESS;
    }
    match open_store(&args) {
        Ok(store) => {
            mailbox_ui::run_with(store, args.mailto);
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("mailbox: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_flags() {
        let args = parse(&["--sample", "--data", "x.db"]).unwrap();
        assert!(args.sample);
        assert_eq!(args.data, Some(PathBuf::from("x.db")));
        assert!(parse(&["--data"]).is_err());
        assert!(parse(&["--nope"]).is_err());
        let mailto = parse(&["mailto:a@b.co?subject=Hi"]).unwrap();
        assert_eq!(mailto.mailto.as_deref(), Some("mailto:a@b.co?subject=Hi"));
    }
}
