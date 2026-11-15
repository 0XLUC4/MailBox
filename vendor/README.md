# Vendored crates

## gpui-pre-windows 0.3.11

GPUI's Windows backend, unchanged except `src/directx_renderer.rs`
(`DirectComposition::new`): the DirectComposition target is created with `topmost = false`.

Why: Mailbox shows HTML mail in a WebView2 child window. With a topmost target, GPUI's visual
covers child windows, so the app used to turn DirectComposition off
(`GPUI_DISABLE_DIRECT_COMPOSITION`), which presents through a flip-model swap chain on the window.
On NVIDIA RTX 50 drivers (616+) that path flashes white on redraws (multi-plane overlay). With a
non-topmost target, DirectComposition stays on and child windows show above the app's visual.

To update: copy the new version from the cargo registry and re-apply the one-line change.
