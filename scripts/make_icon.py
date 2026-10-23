#!/usr/bin/env python3
"""Render assets/AppIcon.svg into assets/AppIcon.icns, the app's icon.

Run it after changing the SVG; the bundle copies the .icns as it is, so
building needs neither tool. Rendering uses rsvg-convert (librsvg), or
ImageMagick when that is missing.
"""
from __future__ import annotations

import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "AppIcon.svg"
OUTPUT = ROOT / "assets" / "AppIcon.icns"

# The PNG entries of an .icns file, by pixel size. macOS picks the 1x and
# 2x images for each point size from these.
ENTRIES = (
    ("icp4", 16),
    ("icp5", 32),
    ("ic11", 32),  # 16 pt at 2x
    ("icp6", 64),
    ("ic12", 64),  # 32 pt at 2x
    ("ic07", 128),
    ("ic08", 256),
    ("ic13", 256),  # 128 pt at 2x
    ("ic09", 512),
    ("ic14", 512),  # 256 pt at 2x
    ("ic10", 1024),  # 512 pt at 2x
)


def render(svg: Path, size: int, out: Path) -> None:
    """Draws `svg` as a `size` pixel square PNG."""
    if shutil.which("rsvg-convert"):
        command = ["rsvg-convert", "-w", str(size), "-h", str(size), str(svg), "-o", str(out)]
    elif shutil.which("magick"):
        command = ["magick", "-background", "none", "-density", "384", str(svg), "-resize", f"{size}x{size}", str(out)]
    else:
        raise SystemExit("Install librsvg (rsvg-convert) or ImageMagick to render the icon.")
    subprocess.run(command, check=True)


def icns(pngs: dict[int, bytes]) -> bytes:
    """An .icns file: its header, then each entry's type, length and PNG."""
    body = b"".join(
        kind.encode("ascii") + struct.pack(">I", 8 + len(pngs[size])) + pngs[size]
        for kind, size in ENTRIES
    )
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        pngs = {}
        for size in sorted({size for _, size in ENTRIES}):
            out = Path(directory) / f"{size}.png"
            render(SOURCE, size, out)
            pngs[size] = out.read_bytes()
    OUTPUT.write_bytes(icns(pngs))
    print(OUTPUT)


if __name__ == "__main__":
    sys.exit(main())
