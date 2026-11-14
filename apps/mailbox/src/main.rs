//! `mailbox [--data <path>]`
//!
//! Opens the user's database: `<data dir>/mailbox.db` (`%APPDATA%\mailbox` on Windows,
//! `$XDG_DATA_HOME/mailbox` or `~/.local/share/mailbox` elsewhere, `MAILBOX_DATA_DIR`
//! overrides), or the SQLite file given with `--data`. A new database has no account: the app
//! starts on the onboarding screen.

use std::path::PathBuf;
use std::process::ExitCode;

use mailbox_core::Store;

const USAGE: &str = "usage: mailbox [--data <path>]";

#[derive(Debug, Default)]
struct Args {
    data: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data" => {
                let path = args.next().ok_or("--data needs a path")?;
                parsed.data = Some(PathBuf::from(path));
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok(parsed)
}

fn open_store(args: &Args) -> Result<Store, String> {
    let path = args
        .data
        .clone()
        .unwrap_or_else(|| mailbox_ui::data_dir().join("mailbox.db"));
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    }
    Store::open(&path).map_err(|err| format!("cannot open {}: {err}", path.display()))
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match open_store(&args) {
        Ok(store) => {
            mailbox_ui::run(store);
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
        let args = parse(&["--data", "x.db"]).unwrap();
        assert_eq!(args.data, Some(PathBuf::from("x.db")));
        assert!(parse(&[]).unwrap().data.is_none());
        assert!(parse(&["--data"]).is_err());
        assert!(parse(&["--sample"]).is_err(), "sample data is not shipped");
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn opens_a_fresh_database_without_accounts() {
        let dir = std::env::temp_dir().join(format!("mailbox-main-{}", std::process::id()));
        let path = dir.join("nested").join("mailbox.db");
        let store = open_store(&Args {
            data: Some(path.clone()),
        })
        .unwrap();
        assert!(store.list_accounts().unwrap().is_empty());
        assert!(path.exists());
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
