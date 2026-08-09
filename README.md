# DC34 Badge LED Firmware

Custom firmware for the DC34 badge. It shows a background image, animates the 10
onboard LEDs with a set of patterns, and is controlled with the badge buttons.

- Background image on the display
- Animated LED patterns on the onboard WS2812 LEDs (BIO co-processor)
- Buttons: next / previous pattern, camera, hold to power off
- Default startup pattern: **BR Runner**

## Layout

```
dc34-leds/                # this project
├── build.sh              # compile + bundle the firmware
├── flash.sh              # flash a badge (verifies the write)
├── img2background.py     # convert an image to the badge bitmap
├── src/                  # the app
└── firmware/             # built loader.uf2 / xous.uf2 / swap.uf2

xous-core/                # required sibling checkout (the OS the app bundles into)
```

## Setup

This project builds against a checkout of **xous-core** placed next to it as a
sibling directory. Clone it at the exact revision this project is pinned to:

```bash
# from the parent directory that contains dc34-leds/
git clone https://github.com/betrusted-io/xous-core.git
git -C xous-core checkout 5d5bbbfa95c0dcef26fe1fe9b496b7f6f31d191b
```

The app's `Cargo.toml` patches its Xous dependencies to `../xous-core`, and
`build.sh` runs the image bundler (`cargo xtask baosec-lite`) from there, so the
directory must be named `xous-core` and sit beside `dc34-leds/`.

You also need the Rust toolchain with the Xous target installed (xous-core
provides it):

```bash
cargo xtask install-toolkit   # run inside xous-core, installs the riscv32imac-unknown-xous-elf toolchain
```

## Build

```bash
./build.sh                      # build with the current background
./build.sh --image logo.png     # convert + bake in a custom background
```

Output UF2s land in `dc34-leds/firmware/`. Requires the toolchain from
[Setup](#setup), plus Python 3 + Pillow if you use `--image`.

## Flash

Put the badge in update mode (hold **PROG** while plugging in, until it mounts as
`BAOCHIP`), then:

```bash
sudo ./flash.sh
```

`flash.sh` mounts the badge read-write, copies the firmware, verifies every file
by checksum, and ejects. If verification fails (usually a bad USB cable) it
refuses to finish — do not boot the badge in that case; use a data cable and
retry. Press **PROG**/reset to boot after a successful flash.

## Controls

| Action | Button | Effect |
|---|---|---|
| Next pattern | `→` / `d` / `3` | Advance to the next pattern |
| Previous pattern | `←` / `↑` / `a` / `1` | Go back to the previous pattern |
| Camera | `🔥` (PROG) | Scan a QR code, then redraw |
| Power off | hold `↓` ~1.5–2s | Blank, wait ~3s (release the button), then sleep |

Wake from power-off with a button press / reset.

## Customizing

- **Background image:** `./build.sh --image your.png` (fit to 128×128, converted
  to 1-bit black & white). The original is backed up to
  `dc34-leds/src/background.rs.orig`.
- **Startup pattern:** `initial_pattern` in `dc34-leds/src/leds.rs`.
- **Pattern order:** `PATTERN_ORDER` in `dc34-leds/src/bio/lightgenes/mod.rs`.

### Adding a pattern

Patterns run on the BIO co-processor and are generated from C into the
`*.rs` files under `dc34-leds/src/bio/lightgenes/`. To add one:

1. Write the C animation (reads pin then LED count from FIFO1, then drives the
   strip) and generate its `<name>.rs` with the BIO toolchain (zig). Do not build
   the C with `SIM` defined — that shortens delays for simulation and makes the
   pattern look frozen on hardware.
2. Copy `<name>.rs` into `dc34-leds/src/bio/lightgenes/`.
3. In `dc34-leds/src/bio/lightgenes/mod.rs`: add the `PatternKind` variant, its
   `bio_code()` arm, its `from_u32()` arm, and add it to `PATTERN_ORDER`.
4. `./build.sh` and reflash.
