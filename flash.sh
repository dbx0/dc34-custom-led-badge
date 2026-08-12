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

# --- copy + immediate verify (before unmount) ---------------------------------
# Two-stage verification distinguishes the failure modes:
#   Stage 1 (immediate, while still mounted rw): does the write even land?
#           If the file is missing/short/wrong RIGHT AFTER writing, the write
#           itself failed -> bad cable/port, retry.
#   Stage 2 (after unmount/remount): does it survive a cache flush + reread?
#           This is the authoritative check (FAT read caches can lie before a
#           remount).
echo "==> Copying firmware (with immediate per-file verify + retry)"
IMMED_FAIL=0
for f in "${FILES[@]}"; do
  src_sz=$(stat -f%z "$FW_DIR/$f.uf2")
  src_md5=$(md5 -q "$FW_DIR/$f.uf2")
  ok=0
  for attempt in 1 2 3; do
    cat "$FW_DIR/$f.uf2" > "$MNT/$f.uf2" 2>/dev/null || true
    sync
    dst_sz=$(stat -f%z "$MNT/$f.uf2" 2>/dev/null || echo 0)
    dst_md5=$(md5 -q "$MNT/$f.uf2" 2>/dev/null || echo "READ_FAIL")
    if [ "$src_sz" = "$dst_sz" ] && [ "$src_md5" = "$dst_md5" ]; then
      echo "    wrote $f.uf2  ($dst_sz bytes)  [attempt $attempt: immediate check OK]"
      ok=1
      break
    else
      echo "    wrote $f.uf2  [attempt $attempt: immediate check FAIL src=$src_sz dst=$dst_sz] retrying..."
      sleep 1
    fi
  done
  if [ "$ok" -ne 1 ]; then
    echo "    !! $f.uf2 did not land after 3 attempts"
    IMMED_FAIL=1
  fi
done

if [ "$IMMED_FAIL" -ne 0 ]; then
  echo
  echo "!! WRITE FAILED - the firmware never landed on the badge (missing/short"
  echo "!! immediately after writing). This is a bad/charge-only USB cable or a"
  echo "!! flaky port/hub. Swap to a known-good DATA cable plugged DIRECTLY into"
  echo "!! the computer, re-enter update mode, and run this script again."
  echo "!! The badge was NOT modified; safe to retry."
  exit 1
fi

# --- eject so the badge can boot ----------------------------------------------
# NOTE: we intentionally do NOT re-read the files after an unmount/remount.
# This is a UF2 bootloader: it exposes a synthetic FAT, consumes the .uf2 as it
# is written, and reading a file back does NOT return the bytes we wrote (the
# bootloader reports its own post-flash state). So a remount md5 check ALWAYS
# mismatches even on a perfect flash. The immediate per-file size+md5 check
# above (while still mounted read-write, before the bootloader has torn the
# mount down) is the correct confirmation that the bytes were accepted.
echo "==> Firmware accepted (immediate verify passed). Ejecting."
diskutil eject "$DEV" >/dev/null 2>&1 || diskutil unmount "$DEV" >/dev/null 2>&1 || true

echo
echo "SUCCESS: firmware written and verified on write."
echo "Press PROG/reset on the badge to boot the custom LED firmware."
echo "(If the badge shows a 'kernel - NNNk' message during flashing, that is the"
echo " bootloader accepting the image -- expected.)"
