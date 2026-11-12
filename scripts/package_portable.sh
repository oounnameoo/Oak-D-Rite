#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OS_NAME="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH_NAME="$(uname -m)"

case "$OS_NAME" in
    darwin) OS_NAME="macos" ;;
esac

case "$ARCH_NAME" in
    x86_64|amd64) ARCH_NAME="x86_64" ;;
    arm64|aarch64) ARCH_NAME="arm64" ;;
esac

BUNDLE_NAME="oakd-lite-studio-${OS_NAME}-${ARCH_NAME}-portable"
RELEASE_DIR="${ROOT_DIR}/target/release"
DIST_DIR="${ROOT_DIR}/dist"
BUNDLE_DIR="${DIST_DIR}/${BUNDLE_NAME}"
ZIP_PATH="${DIST_DIR}/${BUNDLE_NAME}.zip"

cd "$ROOT_DIR"
cargo build --release --bin oakd-lite-gui

rm -rf "$BUNDLE_DIR" "$ZIP_PATH"
mkdir -p "$BUNDLE_DIR"

cp "${RELEASE_DIR}/oakd-lite-gui" "$BUNDLE_DIR/"

# Copy any staged shared libraries (.so / .dylib) from target/release
shopt -s nullglob
for lib in "${RELEASE_DIR}"/*.so* "${RELEASE_DIR}"/*.dylib; do
    cp -a "$lib" "$BUNDLE_DIR/"
done
shopt -u nullglob

# On macOS, ensure @executable_path rpath is set so bundled .dylibs load without install
if [ "$OS_NAME" = "macos" ]; then
    install_name_tool -add_rpath "@executable_path" "${BUNDLE_DIR}/oakd-lite-gui" 2>/dev/null || true
fi

cat > "${BUNDLE_DIR}/README-PORTABLE.txt" <<'EOF'
OAK-D Lite Studio (Portable)
============================

How to use:
1. Extract this archive and keep all libraries next to the oakd-lite-gui binary.
2. Plug in your OAK-D Lite via USB 3.0.
3. Run: ./oakd-lite-gui

Linux USB note (first-time setup):
If permission is denied opening USB, add the udev rule once:
  echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
  sudo udevadm control --reload-rules && sudo udevadm trigger
EOF

(cd "$DIST_DIR" && zip -r -y "$ZIP_PATH" "$BUNDLE_NAME")
echo "Created portable zip: $ZIP_PATH"
