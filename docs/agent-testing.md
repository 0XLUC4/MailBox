# Testing the app for real (agents)

Build and test are REQUIRED for every task. Code that was not compiled and seen running is not done.

## Build
```bash
export CARGO_TARGET_DIR=C:/Users/Luca/Desktop/MailBox/mailbox/target   # shared, avoids recompiling GPUI
cargo check --workspace --all-targets && cargo test --workspace
cargo build -p mailbox
```
Test modules in `mailbox-ui` must not `use super::*` when the parent has `use gpui::*` (gpui's `test` macro shadows `#[test]`).

## Run without locking the shared exe
Windows locks a running exe, which breaks other agents' builds. Copy it first:
```powershell
$me = "$env:TEMP\mbx-<your-agent-name>"; New-Item -ItemType Directory -Force $me | Out-Null
Copy-Item C:\Users\Luca\Desktop\MailBox\mailbox\target\debug\mailbox.exe "$me\mailbox.exe" -Force
$p = Start-Process "$me\mailbox.exe" -ArgumentList "--data","$me\test.db" -PassThru -NoNewWindow
```
Always use `--data` with a temp db (never the user's real data). Sample data (2 accounts, ~200
messages, events, contacts, tasks): `cargo run -p mailbox-core --features dev --example seed -- <temp.db>`.

Every worktree shares one target dir and its crates get the same artifact names: if a build
misses your latest change (e.g. "cannot find function" that exists), `touch crates/*/src/lib.rs`
and rebuild, then copy the exe right away. Stop it afterwards: `Stop-Process -Id $p.Id -Force`.

## Drive and look at the window
Capture ONLY the app window, never the whole screen (privacy). Click, then type (typing alone does not reach the field):
```powershell
Add-Type @"
using System; using System.Runtime.InteropServices;
public class M { [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y); [DllImport("user32.dll")] public static extern void mouse_event(int f,int x,int y,int d,int e);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
public struct R { public int L,T,Ri,B; }
public static void Click(int x,int y){ SetCursorPos(x,y); mouse_event(2,0,0,0,0); mouse_event(4,0,0,0,0);} }
"@
Add-Type -AssemblyName System.Drawing,System.Windows.Forms
$p.Refresh(); $h=$p.MainWindowHandle; [M]::SetForegroundWindow($h) | Out-Null
$r=New-Object M+R; [M]::GetWindowRect($h,[ref]$r) | Out-Null
[M]::Click($r.L+100,$r.T+100)                                   # window-relative coords
[System.Windows.Forms.SendKeys]::SendWait("hello{ENTER}")
$w=$r.Ri-$r.L; $hh=$r.B-$r.T; $bmp=New-Object System.Drawing.Bitmap $w,$hh
[System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($r.L,$r.T,0,0,$bmp.Size); $bmp.Save("$me\shot.png")
```
Then read the PNG to check. Delete temp files when done.

Also set `MAILBOX_DATA_DIR` to your temp dir: sync state, the Outbox headers, attachments and the
single-instance port file live there, and without it your test app forwards to (or collides with)
another agent's running instance. The AgentDeck hook refuses new console windows: run the app and
the devserver as background commands of your shell tool instead, e.g. (bash)
`MAILBOX_DATA_DIR="$TEMP/mbx-me/data" "$TEMP/mbx-me/mailbox.exe" --data "$TEMP/mbx-me/data/test.db"`.

Other agents drive their own windows at the same time: `SetForegroundWindow` can silently fail and
your clicks/keys then land in someone else's window (and theirs in yours). Before every click and
every keystroke check `GetForegroundWindow() == $h` (tap Alt + `SetForegroundWindow` to get it
back, abort if it fails), send keys one at a time, and wait ~800 ms after a click before typing.

## Local mail server for end-to-end tests (`mailbox-devserver`)
Plain IMAP4rev1 (IDLE, MOVE, UIDPLUS, SPECIAL-USE) + SMTP on 127.0.0.1, in-memory mailboxes INBOX,
Sent, Drafts, Trash, Archive, Junk. Any user name and password are accepted. SMTP stores every
message and files a copy in INBOX when a recipient is `@<domain>` (default `mailbox.test`), so mail
to yourself comes back on the next sync. State is lost when it stops. Never use a real account.
```bash
cargo build -p mailbox-devserver   # then copy target/debug/mailbox-devserver.exe to your temp dir
mailbox-devserver --seed --imap-port 21143 --smtp-port 21025 --save "$TEMP/mbx-me/deliveries"
```
- `--seed`: 4 recent INBOX messages (a thread, HTML, an attachment) + 1 in Archive.
- `--bulk N`: N more INBOX notifications (text part opens with a `=====` line). A few thousand
  make the first sync last several seconds: kill the app mid-sync to test restarts.
- `--save DIR`: each SMTP delivery written to `DIR/delivery-N.eml` (check the exact MIME, Bcc...).
- `--verbose`: log every IMAP/SMTP command; `--domain D`: the local domain.
- Pick your own ports (defaults 1143/1025) so agents do not share a server.

In the app: Add account -> `me@mailbox.test`, any password -> on Server settings set both servers to
`127.0.0.1`, your ports, Security **None** -> Test connection -> Add account. The password goes to
the Windows credential store (service `mailbox`): remove the account when done.
Stop the server to test send failures (the message stays in the Outbox, toast with Retry); start it
again on the same ports and click Retry.

In tests, use the library in-process (`[dev-dependencies] mailbox-devserver.workspace = true`):
```rust
let server = mailbox_devserver::DevServer::start(mailbox_devserver::Config { seed: true, ..Default::default() })?;
// server.imap_port(), server.smtp_port(); server.with_store(|s| s.deliveries.len()); server.stop();
```
See `crates/sync/tests/devserver.rs`.
