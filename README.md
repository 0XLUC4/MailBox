<div align="center">

<img src="assets/png/icon-128.png" width="96" alt="Mailbox icon: a white envelope on a black rounded square">

# Mailbox

**Mail, calendar, contacts and tasks in one fast, native desktop app.**

Written in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), the GPU
UI framework behind the Zed editor. No Electron, and your data stays on your computer.

[Download the latest release](https://github.com/0XLUC4/MailBox/releases/latest)

</div>

![Mailbox: the inbox with an HTML email shown exactly as it was sent](docs/images/screenshot.png)

<sub>Screenshot taken with demo data (made-up people and messages).</sub>

## Features

- **Mail**: any IMAP / SMTP provider (Gmail, Outlook, iCloud, your own domain…), automatic server
  detection, folders and favorites, conversations, advanced search (`from:`, `has:attachment`,
  `is:unread`, `after:`…), flags, categories, rules, snooze, send later, offline mode.
- **Emails shown exactly as they were sent**, rendered by the Windows WebView2 engine and locked
  down: JavaScript off, remote images blocked until you allow them.
- **Compose**: rich text, attachments, signatures, auto-saved drafts, undo send, several compose
  windows at once.
- **Calendar**: day, week, month and agenda views, recurring events, reminders, multiple calendars,
  invitations (accept / decline), `.ics` import and export, CalDAV sync.
- **Contacts**: full contact cards, groups, duplicate merging, `.vcf` import and export, CardDAV sync.
- **Tasks**: My Day, Important, Planned, custom lists, steps, reminders, flagged emails.
- **Built-in updates**: the arrow above Settings shows when a new version is out, then
  "Update and restart".
- Light / dark theme, accent color, English and French UI, undo / redo everywhere
  (`Ctrl+Z` / `Ctrl+Shift+Z`).

## Install (Windows)

1. Download `Mailbox-X.Y.Z-windows-x64.zip` from the
   [releases](https://github.com/0XLUC4/MailBox/releases/latest).
2. Unzip it anywhere and run `mailbox.exe`. No installer needed.
3. Add your account: email address and password. For Gmail, use an
   [app password](https://myaccount.google.com/apppasswords).

Your mail is stored in `%APPDATA%\Mailbox` and your password in the Windows Credential Manager.

## Build from source

Requires a recent stable [Rust](https://rustup.rs) toolchain.

```sh
cargo run -p mailbox                      # run the app
cargo run -p mailbox -- --data ./test.db  # use another database
cargo test --workspace                    # run all tests
```

Portable package:

```powershell
cargo build --release -p mailbox
powershell -ExecutionPolicy Bypass -File scripts\package.ps1   # dist\Mailbox-<version>-windows-x64.zip
```

## Releasing

Bump `version` in `[workspace.package]` (`Cargo.toml`), commit, then:

```sh
git tag vX.Y.Z && git push origin vX.Y.Z
```

`.github/workflows/release.yml` builds on Windows and publishes the release with
`Mailbox-X.Y.Z-windows-x64.zip` and `SHA256SUMS.txt`. The app checks
`https://api.github.com/repos/0XLUC4/MailBox/releases/latest` at startup and every 6 hours, verifies
the zip's SHA-256 before replacing `mailbox.exe`, and rolls back if anything fails.

## Project layout

| Folder | Purpose |
|---|---|
| `crates/core` | Data model, SQLite store, undoable commands |
| `crates/sync` | IMAP / SMTP, OAuth2, CalDAV / CardDAV, `.eml` / `.mbox` |
| `crates/ui` | The GPUI application |
| `crates/updater` | Updates from GitHub releases |
| `crates/devserver` | Local fake IMAP / SMTP server for tests |
| `apps/mailbox` | The executable (embedded Windows icon) |
| `assets` | Icon (SVG, PNG, ICO) |

See [ARCHITECTURE.md](ARCHITECTURE.md) for details.
