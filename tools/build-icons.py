#!/usr/bin/env python3
"""Regenerate packaged PNG icons from the user-supplied root logo (Pillow)."""
from pathlib import Path
from PIL import Image

root = Path(__file__).resolve().parent.parent
with Image.open(root / "logo.png") as image:
    # 86/108/128/172 are the Sailfish launcher sizes Harbour validation expects.
    for size in (86, 108, 128, 172, 512):
        directory = root / "app/icons" / f"{size}x{size}"
        directory.mkdir(exist_ok=True)
        icon = image.convert("RGBA").resize((size, size), Image.Resampling.LANCZOS)
        icon.save(directory / "harbour-plexfreq.png", optimize=True)
