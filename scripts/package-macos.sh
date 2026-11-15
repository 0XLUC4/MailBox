#!/usr/bin/env bash
# Builds Mailbox for Apple Silicon (M1 to M5, aarch64-apple-darwin) and writes
#   dist/Mailbox-<version>-macos-arm64.dmg   Mailbox.app (ad-hoc signed) + a link to /Applications
#
#   scripts/package-macos.sh [--skip-build]
#
# Runs on macOS only (needs Xcode command line tools: iconutil, codesign, hdiutil).
# Not notarized: on first launch macOS asks to confirm (right-click > Open). Publishes nothing.
set -euo pipefail

target=aarch64-apple-darwin
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="$(awk '/^\[workspace\.package\]/{in_ws=1} in_ws && /^version *=/{gsub(/[^0-9.]/, "", $3); print $3; exit}' Cargo.toml)"
[[ -n "$version" ]] || { echo "version not found in [workspace.package] of Cargo.toml" >&2; exit 1; }

if [[ "${1:-}" != "--skip-build" ]]; then
    cargo build --release -p mailbox --target "$target"
fi
bin="${CARGO_TARGET_DIR:-target}/$target/release/mailbox"
[[ -f "$bin" ]] || { echo "$bin not found: build first" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
app="$work/dmg/Mailbox.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/mailbox"

# App icon from the PNG renders in assets/png.
iconset="$work/icon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256; do
    cp "assets/png/icon-$size.png" "$iconset/icon_${size}x${size}.png"
    cp "assets/png/icon-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
cp assets/png/icon-512.png "$iconset/icon_512x512.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/icon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Mailbox</string>
    <key>CFBundleDisplayName</key><string>Mailbox</string>
    <key>CFBundleIdentifier</key><string>io.github.0xluc4.mailbox</string>
    <key>CFBundleExecutable</key><string>mailbox</string>
    <key>CFBundleIconFile</key><string>icon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Apple Silicon refuses unsigned code: ad-hoc signature (no Apple Developer account needed).
codesign --force --deep --sign - "$app"

mkdir -p dist
dmg="dist/Mailbox-$version-macos-arm64.dmg"
ln -s /Applications "$work/dmg/Applications"
rm -f "$dmg"
hdiutil create -volname "Mailbox $version" -srcfolder "$work/dmg" -fs HFS+ -format UDZO -ov "$dmg" >/dev/null
echo "Wrote $dmg ($(du -h "$dmg" | cut -f1))"
