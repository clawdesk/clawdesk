#!/usr/bin/env python3
"""Generate Tauri app icons from logo.svg.

Requires: rsvg-convert (librsvg), Pillow, iconutil (macOS).
Run from the icons/ directory:
    python3 gen_icons.py
"""
import subprocess, os, sys

ROOT = os.path.dirname(os.path.abspath(__file__))
SVG = os.path.join(ROOT, "..", "..", "..", "logo.svg")

if not os.path.exists(SVG):
    print(f"Error: logo.svg not found at {SVG}")
    sys.exit(1)

def rsvg(size, out):
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), SVG, "-o", out], check=True)

# PNGs for Tauri bundle
rsvg(32, os.path.join(ROOT, "32x32.png"))
rsvg(128, os.path.join(ROOT, "128x128.png"))
rsvg(256, os.path.join(ROOT, "128x128@2x.png"))
rsvg(1024, os.path.join(ROOT, "icon-master.png"))

# Windows ICO (7 sizes)
from PIL import Image
master = Image.open(os.path.join(ROOT, "128x128@2x.png")).convert("RGBA")
sizes = [(16,16), (24,24), (32,32), (48,48), (64,64), (128,128), (256,256)]
ico_imgs = [master.resize(s, Image.LANCZOS) for s in sizes]
ico_imgs[-1].save(os.path.join(ROOT, "icon.ico"), format="ICO", append_images=ico_imgs[:-1], sizes=sizes)

# macOS ICNS via iconutil
iconset = os.path.join(ROOT, "icon.iconset")
os.makedirs(iconset, exist_ok=True)
for name, sz in [
    ("icon_16x16", 16), ("icon_16x16@2x", 32),
    ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256),
    ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024),
]:
    rsvg(sz, os.path.join(iconset, f"{name}.png"))
subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(ROOT, "icon.icns")], check=True)

import shutil
shutil.rmtree(iconset)

print("Icons generated from logo.svg:")
for f in sorted(os.listdir(ROOT)):
    if f.endswith((".png", ".ico", ".icns")):
        sz = os.path.getsize(os.path.join(ROOT, f))
        print(f"  {f:24s} {sz:>8,} bytes")
