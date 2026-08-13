#!/usr/bin/env bash
# build.sh - Compile the dc34-leds app and bundle the signed UF2 firmware.
#
# Produces: dc34-leds/firmware/{loader,xous,swap}.uf2
#
# Usage:
#   ./build.sh                        # build with the current background
#   ./build.sh --image path/to.png    # still background from an image
#   ./build.sh --bg-gif path/to.gif   # animated background from a GIF
#   ./build.sh -i path/to.png  /  -g path/to.gif
#
# --image: the image is fit to 128x128, 1-bit B&W, baked as a single-frame
#   background. --bg-gif: each GIF frame is fit to 128x128, 1-bit B&W, baked as
#   an animated background (~10fps). Both write dc34-leds/src/background.rs.
#
# Run from anywhere; paths are resolved relative to this script.
set -euo pipefail

# --- resolve repo layout (this script lives in the dc34-leds project dir) -----
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
XOUS_DIR="$(cd "$APP_DIR/.." && pwd)/xous-core"
FW_DIR="$APP_DIR/firmware"
TARGET="riscv32imac-unknown-xous-elf"
APP_ELF="$APP_DIR/target/$TARGET/release/dc34-leds"
BG_RS="$APP_DIR/src/background.rs"
IMG2BG="$APP_DIR/img2background.py"
GIF2FRAMES="$APP_DIR/gif2frames.py"

# xous-core dev-HEAD rev that everything is pinned to (used as the CI semver
# fallback so image signing doesn't require a git tag).
XOUS_REV="5d5bbbfa95c0dcef26fe1fe9b496b7f6f31d191b"

# --- parse args ---------------------------------------------------------------
CUSTOM_IMAGE=""
CUSTOM_GIF=""
while [ $# -gt 0 ]; do
  case "$1" in
    -i|--image)
      CUSTOM_IMAGE="${2:-}"
      [ -n "$CUSTOM_IMAGE" ] || { echo "ERROR: $1 requires a path argument"; exit 1; }
      shift 2
      ;;
    -g|--bg-gif)
      CUSTOM_GIF="${2:-}"
      [ -n "$CUSTOM_GIF" ] || { echo "ERROR: $1 requires a path argument"; exit 1; }
      shift 2
      ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1' (use --image <path>, --bg-gif <path>, or --help)"; exit 1
      ;;
  esac
done

if [ -n "$CUSTOM_IMAGE" ] && [ -n "$CUSTOM_GIF" ]; then
  echo "ERROR: use only one of --image or --bg-gif"; exit 1
fi

# --- sanity checks ------------------------------------------------------------
[ -d "$APP_DIR" ]  || { echo "ERROR: missing $APP_DIR"; exit 1; }
[ -d "$XOUS_DIR" ] || { echo "ERROR: missing $XOUS_DIR"; exit 1; }

# --- optional: convert a custom background image ------------------------------
if [ -n "$CUSTOM_IMAGE" ]; then
  [ -f "$CUSTOM_IMAGE" ] || { echo "ERROR: image not found: $CUSTOM_IMAGE"; exit 1; }
  [ -f "$IMG2BG" ]      || { echo "ERROR: missing converter $IMG2BG"; exit 1; }
  command -v python3 >/dev/null || { echo "ERROR: python3 not found (needed for image conversion)"; exit 1; }
  python3 -c "import PIL" 2>/dev/null || { echo "ERROR: Pillow not installed (pip3 install Pillow)"; exit 1; }
  echo "==> [0/3] Converting custom background image: $CUSTOM_IMAGE"
  # Back up the existing bitmap once so a bad image is recoverable.
  [ -f "$BG_RS.orig" ] || cp "$BG_RS" "$BG_RS.orig"
  python3 "$IMG2BG" "$CUSTOM_IMAGE" "$BG_RS"
  echo "    baked into $BG_RS"
fi

# --- optional: convert an animated background GIF -----------------------------
if [ -n "$CUSTOM_GIF" ]; then
  [ -f "$CUSTOM_GIF" ]    || { echo "ERROR: gif not found: $CUSTOM_GIF"; exit 1; }
  [ -f "$GIF2FRAMES" ]    || { echo "ERROR: missing converter $GIF2FRAMES"; exit 1; }
  command -v python3 >/dev/null || { echo "ERROR: python3 not found (needed for gif conversion)"; exit 1; }
  python3 -c "import PIL" 2>/dev/null || { echo "ERROR: Pillow not installed (pip3 install Pillow)"; exit 1; }
  echo "==> [0/3] Converting animated background gif: $CUSTOM_GIF"
  # Back up the existing background once so it's recoverable.
  [ -f "$BG_RS.orig" ] || cp "$BG_RS" "$BG_RS.orig"
  python3 "$GIF2FRAMES" "$CUSTOM_GIF" "$BG_RS"
  echo "    baked animated background into $BG_RS"
fi

# --- rust toolchain -----------------------------------------------------------
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
command -v cargo >/dev/null || { echo "ERROR: cargo not on PATH"; exit 1; }

echo "==> [1/3] Building dc34-leds app ($TARGET)"
( cd "$APP_DIR" && cargo build --release --target "$TARGET" \
    --features board-baosec --features bao1x \
    --features oem-baosec-lite --features utralib/bao1x )

[ -f "$APP_ELF" ] || { echo "ERROR: app ELF not produced at $APP_ELF"; exit 1; }
echo "    app ELF: $APP_ELF ($(stat -f%z "$APP_ELF") bytes)"

echo "==> [2/3] Bundling signed UF2 (baosec-lite, dc34-leds as the only app)"
# The image signer derives a version from git describe; our checkout has no
# tags, so use the CI fallback path (CI/GITHUB_ACTIONS/GITHUB_SHA) which
# synthesizes a version instead of failing.
export CI=true GITHUB_ACTIONS=true GITHUB_SHA="$XOUS_REV"
( cd "$XOUS_DIR" && cargo xtask baosec-lite "${APP_ELF}~flash" \
    --no-timestamp --feature usb --kernel-feature debug-proc --no-verify )

echo "==> [3/3] Staging firmware into $FW_DIR"
mkdir -p "$FW_DIR"
for f in loader xous swap; do
  src="$XOUS_DIR/target/$TARGET/release/$f.uf2"
  [ -f "$src" ] || { echo "ERROR: bundle did not produce $src"; exit 1; }
  cp "$src" "$FW_DIR/$f.uf2"
  echo "    $f.uf2  $(stat -f%z "$FW_DIR/$f.uf2") bytes"
done

echo
echo "Build complete. Firmware staged in: $FW_DIR"
echo "Now run:  ./flash.sh    (with a badge in update mode / mounted as BAOCHIP)"
