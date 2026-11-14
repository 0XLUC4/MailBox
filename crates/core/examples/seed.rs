//! Fills a database with the sample dataset, for demos and manual testing:
//! `cargo run -p mailbox-core --features dev --example seed -- <path.db>`

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: seed <path.db>");
        return ExitCode::from(2);
    };
    let result = mailbox_core::Store::open(&path)
        .and_then(|store| mailbox_core::sample::generate(&store, chrono::Utc::now()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("seed failed: {err}");
            ExitCode::FAILURE
        }
    }
}
