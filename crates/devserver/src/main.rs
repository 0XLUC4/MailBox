//! `mailbox-devserver [--imap-port 1143] [--smtp-port 1025] [--domain mailbox.test] [--seed]
//! [--bulk N] [--save DIR] [--verbose]`: local IMAP + SMTP test server for Mailbox. Runs until
//! killed. `--bulk N` adds N notification-style messages to INBOX.
//! `--save` writes every SMTP delivery to `DIR/delivery-N.eml` (to inspect the exact MIME).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use mailbox_devserver::{Config, DevServer, bulk_messages, lock};

const USAGE: &str = "usage: mailbox-devserver [--imap-port N] [--smtp-port N] \
                     [--domain D] [--seed] [--bulk N] [--save DIR] [--verbose]";

/// Parsed command line: server config, `--save` directory, `--bulk` count.
type Args = (Config, Option<PathBuf>, usize);

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut save = None;
    let mut bulk = 0;
    let mut config = Config {
        imap_port: 1143,
        smtp_port: 1025,
        ..Config::default()
    };
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--imap-port" => config.imap_port = port(&value("--imap-port")?)?,
            "--smtp-port" => config.smtp_port = port(&value("--smtp-port")?)?,
            "--domain" => config.domain = value("--domain")?,
            "--seed" => config.seed = true,
            "--bulk" => {
                let text = value("--bulk")?;
                bulk = text
                    .parse()
                    .map_err(|_| format!("invalid count `{text}`"))?;
            }
            "--save" => save = Some(PathBuf::from(value("--save")?)),
            "--verbose" | "-v" => config.verbose = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok((config, save, bulk))
}

fn port(text: &str) -> Result<u16, String> {
    text.parse().map_err(|_| format!("invalid port `{text}`"))
}

fn main() -> ExitCode {
    let (config, save, bulk) = match parse_args(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let domain = config.domain.clone();
    let server = match DevServer::start(config) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("mailbox-devserver: cannot listen: {err}");
            return ExitCode::FAILURE;
        }
    };
    server.with_store(|store| bulk_messages(store, bulk));
    println!(
        "mailbox-devserver listening on 127.0.0.1\n  IMAP port {}  SMTP port {}  security: None\n  \
         any user name / password is accepted; mail to *@{domain} lands in INBOX",
        server.imap_port(),
        server.smtp_port()
    );
    // Log each SMTP delivery as it arrives.
    let store = server.store();
    let mut seen = 0;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let store = lock(&store);
        for (index, delivery) in store.deliveries.iter().enumerate().skip(seen) {
            if let Some(dir) = &save {
                let path = dir.join(format!("delivery-{}.eml", index + 1));
                if let Err(err) = std::fs::create_dir_all(dir)
                    .and_then(|()| std::fs::write(&path, &delivery.data))
                {
                    eprintln!("cannot write {}: {err}", path.display());
                }
            }
            println!(
                "delivered {} bytes from {} to {}",
                delivery.data.len(),
                delivery.from,
                delivery.recipients.join(", ")
            );
        }
        seen = store.deliveries.len();
    }
}
