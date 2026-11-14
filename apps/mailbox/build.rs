//! Windows: embeds `assets/icon.ico` as icon resource 1 (the exe icon in Explorer, and the icon
//! GPUI loads for the window and taskbar) plus version info.

fn main() {
    println!("cargo:rerun-if-changed=mailbox.rc");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("mailbox.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap_or_else(|err| println!("cargo:warning=app icon not embedded: {err}"));
    }
}
