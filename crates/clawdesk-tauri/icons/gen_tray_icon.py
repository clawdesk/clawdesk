#!/usr/bin/env python3
"""Generate a macOS menu-bar template tray icon for ClawDesk."""
from PIL import Image, ImageDraw
import os

size = 44  # 22pt @2x for Retina

img = Image.new('RGBA', (size, size), (0, 0, 0, 0))
draw = ImageDraw.Draw(img)

center = size // 2
radius = 18
width = 4

# Outer C-shape arc
bbox = [center - radius, center - radius, center + radius, center + radius]
draw.arc(bbox, 30, 330, fill=(0, 0, 0, 255), width=width)

# Inner dot
dot_r = 5
draw.ellipse([center - dot_r, center - dot_r, center + dot_r, center + dot_r],
             fill=(0, 0, 0, 255))

here = os.path.dirname(os.path.abspath(__file__))
icon_path = os.path.join(here, 'tray-icon.png')
img.save(icon_path)
print(f'Saved {icon_path}')
