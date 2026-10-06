#!/usr/bin/env python3
"""Generate the PWA icon set (plans/pwa S1) from the shipped app icon.

Source: src-tauri/icons/icon.png — the 512 px rounded Dusk Wood square with
the #e84f3b mark and transparent corners. That is the icon the desktop app
ships; the older ikenga/design/ikenga-logo*.svg files are exploratory marks
and are deliberately not used.

Output (public/icons/, committed — this script is not a build step):
  icon-192.png, icon-512.png  purpose "any": straight resizes of the source
  maskable-512.png            purpose "maskable": full-bleed background, the
                              mark scaled to ~70 % so it sits inside the 80 %
                              safe zone every launcher mask keeps
  apple-touch-icon.png        180 px, full-bleed (iOS applies its own mask)
  badge-72.png                the mark as a white alpha silhouette (Android
                              status-bar badge)
  favicon-32.png

Needs Pillow (`python3 -m pip install pillow`). No network access.

    python3 scripts/pwa/make-icons.py
"""

from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "src-tauri" / "icons" / "icon.png"
OUT = ROOT / "public" / "icons"

# Dusk Wood dark --bg-base, hsl(28 18% 4%) — also the source icon's own
# background pixel, so a full-bleed canvas is seamless with it.
BG = (12, 10, 8, 255)
MARK = (232, 79, 59)
MASKABLE_MARK_FRACTION = 0.70


def resized(img: Image.Image, size: int) -> Image.Image:
    return img.resize((size, size), Image.Resampling.LANCZOS)


def full_bleed(src: Image.Image) -> Image.Image:
    """The source square with its transparent corners filled with BG."""
    canvas = Image.new("RGBA", src.size, BG)
    canvas.alpha_composite(src)
    return canvas


def mark_alpha(src: Image.Image) -> Image.Image:
    """Coverage of the mark per pixel (0 on the background, 255 on the mark).

    The mark is the only saturated colour on the icon, so the red channel's
    distance from the background red is a clean anti-aliased coverage map.
    """
    r, _g, _b, a = src.split()
    span = MARK[0] - BG[0]

    def cov(v: int) -> int:
        return max(0, min(255, round((v - BG[0]) * 255 / span)))

    coverage = r.point(cov)
    # Transparent corners carry no mark.
    return Image.composite(coverage, Image.new("L", src.size, 0), a)


def mark_only(src: Image.Image) -> Image.Image:
    """The mark in its own colour on a transparent canvas, cropped to its box."""
    alpha = mark_alpha(src)
    layer = Image.new("RGBA", src.size, MARK + (0,))
    layer.putalpha(alpha)
    return layer.crop(alpha.getbbox())


def maskable(src: Image.Image, size: int) -> Image.Image:
    canvas = Image.new("RGBA", (size, size), BG)
    mark = mark_only(src)
    scale = (size * MASKABLE_MARK_FRACTION) / max(mark.size)
    w, h = round(mark.width * scale), round(mark.height * scale)
    mark = mark.resize((w, h), Image.Resampling.LANCZOS)
    canvas.alpha_composite(mark, ((size - w) // 2, (size - h) // 2))
    return canvas


def badge(src: Image.Image, size: int) -> Image.Image:
    alpha = mark_alpha(src)
    box = alpha.getbbox()
    alpha = alpha.crop(box)
    # Square the crop so the silhouette is not stretched.
    side = max(alpha.size)
    square = Image.new("L", (side, side), 0)
    square.paste(alpha, ((side - alpha.width) // 2, (side - alpha.height) // 2))
    inner = round(size * 0.84)
    square = square.resize((inner, inner), Image.Resampling.LANCZOS)
    out = Image.new("RGBA", (size, size), (255, 255, 255, 0))
    white = Image.new("RGBA", (inner, inner), (255, 255, 255, 255))
    white.putalpha(square)
    out.alpha_composite(white, ((size - inner) // 2, (size - inner) // 2))
    return out


def main() -> None:
    src = Image.open(SRC).convert("RGBA")
    if src.size != (512, 512):
        src = resized(src, 512)
    OUT.mkdir(parents=True, exist_ok=True)

    outputs = {
        "icon-192.png": resized(src, 192),
        "icon-512.png": src.copy(),
        "maskable-512.png": maskable(src, 512),
        "apple-touch-icon.png": resized(full_bleed(src), 180).convert("RGB"),
        "badge-72.png": badge(src, 72),
        "favicon-32.png": resized(src, 32),
    }
    for name, img in outputs.items():
        img.save(OUT / name, optimize=True)
        print(f"wrote {OUT / name} {img.size[0]}x{img.size[1]}")


if __name__ == "__main__":
    main()
