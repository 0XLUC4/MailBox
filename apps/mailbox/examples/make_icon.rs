//! Renders `assets/icon.svg` to `assets/png/icon-<size>.png` and a multi-size `assets/icon.ico`.
//! A size with its own hand-tuned `assets/icon-<size>.svg` (pixel-aligned, simplified details for
//! 16/24/32 px) is rendered from that file instead.
//!
//! `cargo run -p mailbox --example make_icon` (run from anywhere: paths are relative to the
//! workspace root). Commit the outputs; the build embeds `assets/icon.ico` (see `build.rs`).

use std::path::{Path, PathBuf};

const PNG_SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];
/// Sizes stored in the .ico (Windows uses at most 256 px).
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn render(tree: &resvg::usvg::Tree, size: u32) -> Result<Vec<u8>, String> {
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(size, size).ok_or(format!("bad icon size {size}"))?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|err| err.to_string())
}

/// ICO file whose images are PNG-encoded (supported since Windows Vista).
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        // 0 means 256 in the one-byte width/height fields.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

fn load(path: &Path) -> Result<resvg::usvg::Tree, String> {
    let svg = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default())
        .map_err(|err| format!("{}: {err}", path.display()))
}

fn run(assets: &Path) -> Result<(), String> {
    let tree = load(&assets.join("icon.svg"))?;
    let png_dir = assets.join("png");
    std::fs::create_dir_all(&png_dir).map_err(|err| err.to_string())?;
    let mut ico_images = Vec::new();
    for size in PNG_SIZES {
        let tuned = assets.join(format!("icon-{size}.svg"));
        let png = if tuned.exists() {
            render(&load(&tuned)?, size)?
        } else {
            render(&tree, size)?
        };
        let path = png_dir.join(format!("icon-{size}.png"));
        std::fs::write(&path, &png).map_err(|err| format!("{}: {err}", path.display()))?;
        if ICO_SIZES.contains(&size) {
            ico_images.push((size, png));
        }
    }
    std::fs::write(assets.join("icon.ico"), ico(&ico_images)).map_err(|err| err.to_string())
}

fn main() -> std::process::ExitCode {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    match run(&assets) {
        Ok(()) => {
            println!("icons written to {}", assets.display());
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("make_icon: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
