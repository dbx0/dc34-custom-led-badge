#!/usr/bin/env bash
# flash.sh - Flash the staged dc34-leds firmware onto a badge in update mode.
#
# Prereqs:
#   - Run ./build.sh first (produces dc34-leds/firmware/{loader,xous,swap}.uf2).
#   - Put the badge in UPDATE MODE: unplug, hold PROG (button nearest USB),
#     plug in while holding, until the Mac mounts it as "BAOCHIP".
#   - Use a KNOWN-GOOD DATA cable + a direct USB port. A charge-only cable or a
#     flaky link causes silent write corruption (this script will catch it and
#     refuse to finish, but you'll just have to redo it).
#
# Usage:
#   sudo ./flash.sh          # sudo is required for mount_msdos
#
# What it does:
#   1. finds the BAOCHIP FAT partition (device node)
#   2. unmounts the macOS read-only (fskit) mount and re-mounts read-write
#      with the legacy mount_msdos driver
#   3. copies the 3 UF2s with plain byte copies (no xattrs)
#   4. VERIFIES each file by size + md5 after a full unmount/remount round-trip
#      (the only trustworthy check; FAT read caches can lie before a remount)
#   5. ejects the volume so you can press PROG/reset to boot
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FW_DIR="$ROOT/firmware"
MNT="/Volumes/BAOCHIP"
FILES=(loader xous swap)

# --- must be root (mount_msdos needs it) --------------------------------------
if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: must run as root for mount_msdos. Re-run:  sudo $0"
  exit 1
fi

# --- firmware present? --------------------------------------------------------
for f in "${FILES[@]}"; do
  [ -f "$FW_DIR/$f.uf2" ] || { echo "ERROR: missing $FW_DIR/$f.uf2 -- run ./build.sh first"; exit 1; }
done

# --- locate the BAOCHIP partition device --------------------------------------
# diskutil prints e.g. "BAOCHIP ... disk4s1"; grab the identifier.
DEV="$(diskutil list 2>/dev/null | awk '/BAOCHIP/ {print "/dev/"$NF}' | head -1)"
if [ -z "$DEV" ]; then
  echo "ERROR: no BAOCHIP volume found."
  echo "  Put the badge in update mode (hold PROG while plugging in) and ensure"
  echo "  you are using a DATA cable on a direct USB port, then re-run."
  exit 1
fi
echo "==> Found badge at $DEV"

# --- remount read-write with the legacy msdos driver --------------------------
echo "==> Remounting $DEV read-write"
diskutil unmount "$DEV" >/dev/null 2>&1 || true
mkdir -p "$MNT"
if ! /sbin/mount_msdos "$DEV" "$MNT" 2>/dev/null; then
  echo "ERROR: mount_msdos failed. The device may have re-enumerated."
  echo "  Try: diskutil unmount $DEV ; then re-run this script."
  exit 1
fi

# confirm writable
if ! touch "$MNT/.wt" 2>/dev/null; then
  echo "ERROR: $MNT mounted read-only; cannot write. Re-run."
  exit 1
fi
rm -f "$MNT/.wt"
echo "    mounted read-write at $MNT"

# --- copy (plain byte copy, no xattrs) ----------------------------------------
echo "==> Copying firmware"
for f in "${FILES[@]}"; do
  cat "$FW_DIR/$f.uf2" > "$MNT/$f.uf2"
  echo "    wrote $f.uf2"
done
sync

# --- verify via full unmount/remount round-trip (defeats FAT read cache) ------
echo "==> Verifying (unmount, remount, compare md5)"
diskutil unmount "$DEV" >/dev/null 2>&1 || true
sleep 1
# fskit will auto-mount read-only, which is fine for reading back
diskutil mount "$DEV" >/dev/null 2>&1 || true
sleep 1

FAIL=0
for f in "${FILES[@]}"; do
  src_sz=$(stat -f%z "$FW_DIR/$f.uf2")
  dst_sz=$(stat -f%z "$MNT/$f.uf2" 2>/dev/null || echo 0)
  src_md5=$(md5 -q "$FW_DIR/$f.uf2")
  dst_md5=$(md5 -q "$MNT/$f.uf2" 2>/dev/null || echo "READ_FAIL")
  if [ "$src_sz" = "$dst_sz" ] && [ "$src_md5" = "$dst_md5" ]; then
    echo "    OK   $f.uf2  ($dst_sz bytes)"
  else
    echo "    FAIL $f.uf2  src=$src_sz/$src_md5  dst=$dst_sz/$dst_md5"
    FAIL=1
  fi
done

if [ "$FAIL" -ne 0 ]; then
  echo
  echo "!! VERIFICATION FAILED - firmware on the badge is NOT good."
  echo "!! DO NOT reset/boot the badge. This is almost always a bad USB cable"
  echo "!! or port corrupting the write. Swap to a known-good data cable on a"
  echo "!! direct port, re-enter update mode, and run this script again."
  exit 1
fi

# --- eject so the badge can boot ----------------------------------------------
echo "==> Verified good. Ejecting."
diskutil eject "$DEV" >/dev/null 2>&1 || true

echo
echo "SUCCESS: firmware flashed and verified."
echo "Press PROG/reset on the badge to boot the custom LED firmware."
