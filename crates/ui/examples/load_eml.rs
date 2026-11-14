//! Imports `.eml` files into the first account's Inbox of a database, newest last, like
//! "Import .mbox / .eml" does (attachments go to `MAILBOX_DATA_DIR`). For manual rendering
//! checks: `cargo run -p mailbox-ui --example load_eml -- <db> crates/ui/tests/fixtures/mail/*.eml`

use std::process::ExitCode;

use mailbox_core::{Command, FolderRole, Store};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(db) = args.next() else {
        eprintln!("usage: load_eml <path.db> <file.eml>...");
        return ExitCode::from(2);
    };
    match load(&db, args) {
        Ok(count) => {
            println!("imported {count} messages");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("load_eml failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn load(
    db: &str,
    files: impl Iterator<Item = String>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let store = Store::open(db)?;
    let account = store
        .list_accounts()?
        .into_iter()
        .next()
        .ok_or("no account in the database (seed it first)")?;
    let inbox = store
        .list_folders(account.id)?
        .into_iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .ok_or("the account has no Inbox")?;
    let mut count = 0;
    for file in files {
        let mut parsed = mailbox_sync::formats::read_eml(&std::fs::read(&file)?)?;
        let attachments = std::mem::take(&mut parsed.attachments);
        let mut message = parsed.message;
        message.account_id = account.id;
        message.folder_id = inbox.id;
        let inverse =
            mailbox_core::command::apply(&store, Command::SaveMessage(Box::new(message)))?;
        let Command::DeleteMessages { ids } = inverse else {
            return Err("unexpected inverse command".into());
        };
        let id = ids.first().copied().ok_or("no message id")?;
        for attachment in attachments {
            let path = mailbox_ui::sync_bridge::attachment_path(id, &attachment.name);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, attachment.bytes)?;
        }
        count += 1;
    }
    Ok(count)
}
