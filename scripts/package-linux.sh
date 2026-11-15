#!/usr/bin/env bash
# Builds Mailbox for Linux x86_64 and writes
#   dist/Mailbox-<version>-linux-x64.tar.gz   mailbox/ (binary, .desktop entry, icon)
#
#   scripts/package-linux.sh [--skip-build]
#
# Unpack anywhere and run mailbox/mailbox; copy mailbox.desktop to ~/.local/share/applications
# (fix its Exec/Icon paths) for a menu entry. Publishes nothing.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="$(awk '/^\[workspace\.package\]/{in_ws=1} in_ws && /^version *=/{gsub(/[^0-9.]/, "", $3); print $3; exit}' Cargo.toml)"
[[ -n "$version" ]] || { echo "version not found in [workspace.package] of Cargo.toml" >&2; exit 1; }

if [[ "${1:-}" != "--skip-build" ]]; then
    cargo build --release -p mailbox
fi
bin="${CARGO_TARGET_DIR:-target}/release/mailbox"
[[ -f "$bin" ]] || { echo "$bin not found: build first" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
pkg="$work/mailbox"
mkdir -p "$pkg"
install -m 755 "$bin" "$pkg/mailbox"
cp assets/png/icon-256.png "$pkg/mailbox.png"
cat > "$pkg/mailbox.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Mailbox
Comment=Mail, calendar and contacts
Exec=mailbox %u
Icon=mailbox
Terminal=false
Categories=Network;Email;Office;
MimeType=x-scheme-handler/mailto;
DESKTOP

mkdir -p dist
archive="dist/Mailbox-$version-linux-x64.tar.gz"
tar -czf "$archive" -C "$work" mailbox
echo "Wrote $archive ($(du -h "$archive" | cut -f1))"
