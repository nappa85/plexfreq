#!/usr/bin/env python3
"""Regenerate packaged PNG icons from the user-supplied root logo (Pillow)."""
from pathlib import Path
from PIL import Image

root = Path(__file__).resolve().parent.parent
with Image.open(root / "logo.png") as image:
    for size in (172, 512):
        directory = root / "app/icons" / f"{size}x{size}"
        directory.mkdir(exist_ok=True)
        icon = image.convert("RGB").resize((size, size), Image.Resampling.LANCZOS)
        icon.save(directory / "harbour-plexfreq.png", optimize=True)
