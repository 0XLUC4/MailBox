# Vendored crates

## gpui-pre-windows 0.3.11

GPUI's Windows backend, unchanged except two edits in `src/directx_renderer.rs`:

1. `DirectComposition::new`: the DirectComposition target is created with `topmost = false`.
2. `shader_resources::build_shader_blob` (debug builds only): the HLSL sources are embedded with
   `include_str!` and compiled with `D3DCompile`, instead of read at runtime from
   `CARGO_MANIFEST_DIR`. Cargo reuses this crate's artifact across git worktrees sharing a target
   directory, so a debug binary could point at a deleted worktree and panic at startup
   ("Recreating GPU state for DirectWrite: ... (os error 3)").

Why: Mailbox shows HTML mail in a WebView2 child window. With a topmost target, GPUI's visual
covers child windows, so the app used to turn DirectComposition off
(`GPUI_DISABLE_DIRECT_COMPOSITION`), which presents through a flip-model swap chain on the window.
On NVIDIA RTX 50 drivers (616+) that path flashes white on redraws (multi-plane overlay). With a
non-topmost target, DirectComposition stays on and child windows show above the app's visual.

To update: copy the new version from the cargo registry and re-apply both changes.
