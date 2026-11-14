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
Always use `--data` with a temp db (never the user's real data). Stop it afterwards: `Stop-Process -Id $p.Id -Force`.

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

## Local mail server for end-to-end tests
`cargo run -p mailbox-devserver` (once it exists) serves fake IMAP/SMTP on 127.0.0.1; add an account pointing at it (security None). Never use a real mail account.
