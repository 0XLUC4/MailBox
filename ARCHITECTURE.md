# Architecture

Mailbox is a mail + calendar + contacts + tasks desktop client in Rust, UI in GPUI (via `gpui-kit`).

## Crates

| Crate | Path | Role |
|---|---|---|
| `mailbox-core` | `crates/core` | Domain model, SQLite store (rusqlite, migrations), `Command` + undo/redo. No UI, no network. Sample data (`sample`) only under `cfg(test)` or the `dev` feature. |
| `mailbox-sync` | `crates/sync` | Protocol backends behind `trait MailBackend` (IMAP/SMTP), sync engine + worker threads, OAuth2, server discovery, .eml/.mbox. |
| `mailbox-ui` | `crates/ui` | GPUI app: `AppState`, theme, shell, module views, components, `sync_bridge`, onboarding. |
| `mailbox-devserver` | `crates/devserver` | Local test IMAP + SMTP server (in-memory, any login), library + binary. Tests and manual runs only. |
| `mailbox` | `apps/mailbox` | Binary: `mailbox [--data <path>]`, default `<data dir>/mailbox.db` (`mailbox_ui::data_dir()`). |

## Mail sync

```
Add account wizard ── test_connection (IMAP login + LIST, SMTP EHLO/AUTH) ──▶ keychain + store
       │                                                    (AccountServers + account.<id>.auth)
       ▼
sync_bridge::start_account ──▶ WorkerHandle (thread, + IDLE thread) ──SyncEvent──▶ bridge ──Command──▶ Store
compose Send ──▶ <data>/outbox/<id>.json (Bcc, In-Reply-To, References, read receipt)
             └─▶ QueueSend → Outbox folder ──▶ SMTP ──▶ APPEND to server Sent (not Gmail) ──▶ local Sent
```

- No account → `onboarding.rs` replaces the content area (Settings stays reachable).
- `AppState::sync_summary()` (`SyncPhase`: no account / idle + last synced / syncing / offline /
  error, plus unsent count) feeds the status area; F9 = Send/Receive, Work Offline toggle.
- A failed send stays in the Outbox with an error toast + Retry (no silent auto-retry).
- `Command::DeleteAccount` removes an account's local data (not undoable); the bridge also drops
  its sync state, attachments and keychain secrets.
- Tests: `crates/sync/tests/fake_servers.rs` and `crates/sync/tests/devserver.rs` run the real
  backend against in-process IMAP/SMTP servers on 127.0.0.1. `mailbox-devserver`
  (`crates/devserver`) is that server as a library + binary for manual end-to-end runs
  (see `docs/agent-testing.md`).

## Data flow

```
view ──Command──▶ AppState::run ──▶ History::run ──▶ command::apply ──▶ Store (SQLite, one transaction)
  ▲                    │                                   │
  │                    │ cx.notify()                       └─ returns the inverse Command (undo)
  └── cx.observe ──────┘   on error: AppEvent::Error ──▶ Shell shows a toast
```

- **Reads** go straight to the store: `self.state.read(cx).store().list_messages(folder_id)`.
  Queries are cheap at this scale; cache in the view (refresh in `cx.observe`) if a read becomes hot.
- **Writes** always go through a `Command` (`mailbox_core::Command`), never `Store::save` from the UI:
  ```rust
  self.state.update(cx, |state, cx| {
      state.run(Command::SetMessageFlags { ids: vec![id], read: Some(true), flagged: None, pinned: None }, cx);
  });
  ```
  `run` applies atomically, pushes the inverse on the undo stack, notifies observers and toasts errors.
  Undo/redo: `state.undo(cx)` / `state.redo(cx)` (Ctrl+Z / Ctrl+Shift+Z).
- **New mutations**: add a variant to `Command`, handle it in `command::apply_inner` returning its inverse,
  add a label, add a test (apply → undo → state equals original). `Command` is serde-serializable for a future CLI/agent.
- **UI selection** (module, folder, message, search query) lives in `AppState` (not undoable). Use the setters
  (`select_folder`, `select_message`, `set_module`, `set_search_query`); they notify.
- **New tables**: append a migration to `store::MIGRATIONS` (never edit a shipped one), implement `store::Record`.

## A view

```rust
pub struct XxxView { state: Entity<AppState>, _subscription: Subscription }

impl XxxView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self { state, _subscription: subscription }
    }
}

impl Render for XxxView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();          // design tokens
        let state = self.state.read(cx);    // read data
        // ... build with components::{Button, IconButton, Avatar, Badge, Banner, Toolbar, EmptyState}
    }
}
```

Look and feel: use `theme` tokens only (no raw colours), spacing `SPACE_*`, radii `RADIUS_*`, sizes `TEXT_*`,
icons `components::IconName` (full Lucide set). Light/dark follows the system; Settings can force it and pick an accent.
Inline errors/statuses in dialogs and cards use `components::Banner` (wraps long hostnames/URLs, shortens
very long server replies with "Copy full message"); toasts with server text go through `components::toast`.
Dialogs that draw their own header/footer use `dialogs::dialog_frame` so only the body scrolls and the card
fits the window.

## File ownership (parallel agents)

| Owner | Files |
|---|---|
| foundation | `crates/core/src/{model,store,command,error,lib}.rs`, `crates/ui/src/{lib,app_state,shell,theme}.rs`, `crates/ui/src/components/` — change only with a small, additive edit |
| mail | `crates/ui/src/mail/` |
| compose | `crates/ui/src/compose/` |
| calendar | `crates/ui/src/calendar/` (recurrence expansion may add a core module `crates/core/src/recurrence.rs`) |
| people + tasks | `crates/ui/src/people/`, `crates/ui/src/tasks/` |
| ribbon / palette / search | `crates/ui/src/ribbon.rs`, `command_palette.rs`, `search.rs` |
| settings / dialogs | `crates/ui/src/settings/`, `crates/ui/src/dialogs/` |
| sync | `crates/sync/` |
| sample data (tests / `dev` feature only) | `crates/core/src/sample.rs` |

Shared additions (a `Command` variant, a store query, a component) are additive: append, don't reshape.

## Conventions

- No `unwrap`/`expect`/`panic!` outside tests; return `Result` (`mailbox_core::Result`, errors via `thiserror`).
  Render code may fall back to empty data (`unwrap_or_default`) since a toast every frame is worse.
- `cargo clippy --all-targets` clean, `cargo fmt` (see `rustfmt.toml`), edition 2024.
- Dependencies pinned in the root `Cargo.toml` `[workspace.dependencies]`; `gpui-kit` exactly (`=0.9.0`),
  imported as `gpui` so GPUI derive macros resolve.
- Tests: `cargo test` — store and command tests in `mailbox-core` use in-memory SQLite.
- Commits: Conventional Commits.
