#!/usr/bin/env python3
"""gif2frames.py - Convert an animated GIF into baked 128x128 1-bit frames.

Emits a Rust module exposing:
    pub const FRAME_COUNT: usize = N;
    pub const FRAMES: [[u32; 512]; N] = [ ... ];

Each frame uses the same packing as the background bitmap (see
img2background.py): fit to 128x128, 1-bit B&W, horizontal flip, bit=0 for
white / 1 for black, MSB-first packing, groups of 4 words written reversed.

Usage:
  gif2frames.py <input.gif> <output_rust_file>
"""
import sys
from PIL import Image, ImageSequence


def frame_to_words(frame: Image.Image) -> list:
    im = frame.convert("RGBA")
    # flatten transparency onto white
    bg = Image.new("RGBA", im.size, (255, 255, 255, 255))
    bg.paste(im, mask=im)
    im = bg.convert("RGB")
    if im.size != (128, 128):
        im = im.resize((128, 128), Image.LANCZOS)
    im = im.transpose(Image.Transpose.FLIP_LEFT_RIGHT).convert("1")
    try:
        pixels = list(im.get_flattened_data())
    except AttributeError:
        pixels = list(im.getdata())

    packed = []
    current = 0
    count = 0
    for p in pixels:
        bit = 0 if p else 1  # white->0, black->1
        current |= (bit << (31 - count))
        count += 1
        if count == 32:
            packed.append(current)
            current = 0
            count = 0
    if count > 0:
        packed.append(current)
    while len(packed) < 512:
        packed.append(0)
    return packed


def convert(ifile: str, ofile: str) -> None:
    src = Image.open(ifile)
    frames = []
    for frame in ImageSequence.Iterator(src):
        frames.append(frame_to_words(frame.copy()))
    n = len(frames)
    if n == 0:
        print("no frames found", file=sys.stderr)
        sys.exit(1)

    with open(ofile, "w") as out:
        out.write("#![cfg_attr(rustfmt, rustfmt_skip)]\n")
        out.write(f"pub const FRAME_COUNT: usize = {n};\n")
        out.write(f"pub const FRAMES: [[u32; 512]; {n}] = [\n")
        for fi, packed in enumerate(frames):
            out.write(f"  // frame {fi}\n  [\n")
            for i in range(512 // 4):
                out.write(
                    "    0x{:08x}, 0x{:08x}, 0x{:08x}, 0x{:08x},\n".format(
                        packed[i * 4 + 3],
                        packed[i * 4 + 2],
                        packed[i * 4 + 1],
                        packed[i * 4 + 0],
                    )
                )
            out.write("  ],\n")
        out.write("];\n")
    print(f"wrote {ofile}: {n} frames from {ifile}")


def main() -> None:
    if len(sys.argv) != 3:
        print("usage: gif2frames.py <input.gif> <output_rust_file>", file=sys.stderr)
        sys.exit(2)
    convert(sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    main()
