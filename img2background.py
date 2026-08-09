#!/usr/bin/env python3
"""img2background.py - Convert an image into the dc34-leds background bitmap.

Reproduces the badge's proven bitmap format (matching the original
pngtorust.py used by dc34-vault):
  - resize/fit to 128x128
  - convert to 1-bit black & white
  - horizontal flip (FLIP_LEFT_RIGHT), as the display expects
  - bit = 0 for white pixels, 1 for black
  - pack MSB-first into 32-bit words
  - emit words in groups of 4, each group written in reverse order
  - output `pub const BITMAP: [u32; 512]`

Usage:
  img2background.py <input_image> <output_rust_file>
"""
import sys
from PIL import Image


def convert(ifile: str, ofile: str) -> None:
    im = Image.open(ifile)

    # Flatten transparency onto white so alpha doesn't render as black.
    if im.mode in ("RGBA", "LA") or (im.mode == "P" and "transparency" in im.info):
        bg = Image.new("RGBA", im.size, (255, 255, 255, 255))
        bg.paste(im.convert("RGBA"), mask=im.convert("RGBA"))
        im = bg.convert("RGB")

    # Fit to 128x128 (stretch to fill; the panel is square).
    if im.size != (128, 128):
        im = im.resize((128, 128), Image.LANCZOS)

    # Match the reference pipeline: flip L-R, then 1-bit.
    im = im.transpose(Image.Transpose.FLIP_LEFT_RIGHT).convert("1")

    try:
        pixels = list(im.get_flattened_data())  # Pillow >= 12
    except AttributeError:
        pixels = list(im.getdata())  # older Pillow  (0=black, 255=white)

    packed = []
    current = 0
    count = 0
    for p in pixels:
        bit = 0 if p else 1  # white -> 0, black -> 1
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

    with open(ofile, "w") as out:
        out.write("#![cfg_attr(rustfmt, rustfmt_skip)]\n")
        out.write("pub const BITMAP: [u32; 512] = [\n")
        for i in range(512 // 4):
            out.write(
                "  0x{:08x}, 0x{:08x}, 0x{:08x}, 0x{:08x},\n".format(
                    packed[i * 4 + 3],
                    packed[i * 4 + 2],
                    packed[i * 4 + 1],
                    packed[i * 4 + 0],
                )
            )
        out.write("\n];\n")


def main() -> None:
    if len(sys.argv) != 3:
        print("usage: img2background.py <input_image> <output_rust_file>", file=sys.stderr)
        sys.exit(2)
    convert(sys.argv[1], sys.argv[2])
    print(f"wrote {sys.argv[2]} from {sys.argv[1]}")


if __name__ == "__main__":
    main()
