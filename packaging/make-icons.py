#!/usr/bin/env python3
"""Renders every raster image of the CloudDrop logo from the SVG sources in data/icons.

Run it again after changing the icon. Needs python3-gi with librsvg (gir1.2-rsvg-2.0)
and python3-pil. Writes:

  packaging/windows/msix/Assets/          Microsoft Store package images
  packaging/windows/msix/store-logos/     Store listing art (tile icon, box art, poster)
  win/assets/clouddrop.ico                the agent's icon (Explorer sync root, Start menu)
  win/assets/tray-*.rgba                  notification-area icons, 32x32 raw RGBA
  winapp/windows/runner/resources/app_icon.ico
  winapp/assets/icon.png                  the logo inside the Windows app
"""

import io
import re
import warnings
from pathlib import Path

import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg  # noqa: E402
from PIL import Image  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
ICON = ROOT / "data/icons/hicolor/scalable/apps/com.dhivalabs.CloudDrop.svg"
MSIX = ROOT / "packaging/windows/msix/Assets"
STORE = ROOT / "packaging/windows/msix/store-logos"

# The cloud of the status icons, at 32x32 (twice the 16px symbolic icons).
CLOUD = "M9 26 A7 7 0 0 1 8.42 12.02 A9 9 0 0 1 26.2 13.58 A6.4 6.4 0 0 1 24 26 Z"
GLYPHS = {
    "idle": "M9.2 18.2 L14.22 23.22 L23.02 14.18 L20.58 11.82 L14.18 18.38 L11.6 15.8 Z",
    "syncing": "M16 10 L23 17 L18 17 L18 22 L14 22 L14 17 L9 17 Z",
    "paused": "M10.6 12 H13.4 A0.6 0.6 0 0 1 14 12.6 V21.4 A0.6 0.6 0 0 1 13.4 22 H10.6 "
    "A0.6 0.6 0 0 1 10 21.4 V12.6 A0.6 0.6 0 0 1 10.6 12 Z M18.6 12 H21.4 A0.6 0.6 0 0 1 22 12.6 "
    "V21.4 A0.6 0.6 0 0 1 21.4 22 H18.6 A0.6 0.6 0 0 1 18 21.4 V12.6 A0.6 0.6 0 0 1 18.6 12 Z",
    "attention": "M15.4 9.6 H16.6 A1.4 1.4 0 0 1 18 11 V16.6 A1.4 1.4 0 0 1 16.6 18 H15.4 "
    "A1.4 1.4 0 0 1 14 16.6 V11 A1.4 1.4 0 0 1 15.4 9.6 Z M14 22 A2 2 0 1 1 18 22 A2 2 0 1 1 14 22 Z",
}
SKY = '<stop offset="0" stop-color="#2fb6f0"/><stop offset="1" stop-color="#3f5be0"/>'
FILLS = {"idle": "url(#sky)", "syncing": "url(#sky)", "paused": "#8a8f98", "attention": "#e66100"}


def render(svg: str, width: int, height: int | None = None) -> Image.Image:
    """Rasterises SVG text at exactly width×height pixels."""
    height = height or width

    def sized(match: re.Match) -> str:
        tag = re.sub(r'\s(width|height)="[^"]*"', "", match.group(0))
        return tag.replace("<svg", f'<svg width="{width}" height="{height}"', 1)

    svg = re.sub(r"<svg\b[^>]*>", sized, svg, count=1)
    with warnings.catch_warnings():
        # get_pixbuf is deprecated in favour of cairo rendering, which needs python3-gi-cairo.
        warnings.simplefilter("ignore", DeprecationWarning)
        pixbuf = Rsvg.Handle.new_from_data(svg.encode()).get_pixbuf()
    _, png = pixbuf.save_to_bufferv("png", [], [])
    return Image.open(io.BytesIO(png)).convert("RGBA")


def icon(size: int) -> Image.Image:
    return render(ICON.read_text(), size)


def centred(width: int, height: int, size: int, background: Image.Image | None = None) -> Image.Image:
    """The icon, `size` px square, centred on a width×height image."""
    canvas = background.copy() if background else Image.new("RGBA", (width, height))
    canvas.alpha_composite(icon(size), ((width - size) // 2, (height - size) // 2))
    return canvas


def store_background(width: int, height: int) -> Image.Image:
    return render(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}">'
        '<defs><linearGradient id="b" x1="0" y1="0" x2="1" y2="1">'
        '<stop offset="0" stop-color="#eefaff"/><stop offset="1" stop-color="#e4e6ff"/>'
        f'</linearGradient></defs><rect width="{width}" height="{height}" fill="url(#b)"/></svg>',
        width,
        height,
    )


def msix_assets() -> None:
    MSIX.mkdir(exist_ok=True)
    for old in MSIX.glob("*.png"):
        old.unlink()
    for scale in (100, 200, 400):
        f = scale / 100
        centred(round(50 * f), round(50 * f), round(50 * f)).save(MSIX / f"StoreLogo.scale-{scale}.png")
        centred(round(44 * f), round(44 * f), round(36 * f)).save(MSIX / f"Square44x44Logo.scale-{scale}.png")
        centred(round(150 * f), round(150 * f), round(96 * f)).save(MSIX / f"Square150x150Logo.scale-{scale}.png")
        centred(round(310 * f), round(150 * f), round(96 * f)).save(MSIX / f"Wide310x150Logo.scale-{scale}.png")
    # Taskbar, Start and file icons, without the coloured tile plate.
    for size in (16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256):
        for qualifiers in (f"targetsize-{size}", f"targetsize-{size}_altform-unplated"):
            icon(size).save(MSIX / f"Square44x44Logo.{qualifiers}.png")


def store_logos() -> None:
    STORE.mkdir(exist_ok=True)
    centred(300, 300, 300).save(STORE / "app-tile-icon-300.png")
    centred(1080, 1080, 560, store_background(1080, 1080)).save(STORE / "box-art-1080.png")
    poster = store_background(1440, 2160)
    poster.alpha_composite(icon(720), (360, 720))
    poster.save(STORE / "poster-art-1440x2160.png")


def ico(path: Path, sizes: tuple[int, ...]) -> None:
    images = [icon(s) for s in sizes]
    images[-1].save(path, sizes=[(s, s) for s in sizes], append_images=images[:-1])


def tray_icons() -> None:
    for name, glyph in GLYPHS.items():
        svg = (
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">'
            f'<defs><linearGradient id="sky" x1="0" y1="6" x2="0" y2="26" gradientUnits="userSpaceOnUse">{SKY}'
            "</linearGradient></defs>"
            f'<path fill="{FILLS[name]}" fill-rule="evenodd" d="{CLOUD} {glyph}"/></svg>'
        )
        (ROOT / f"win/assets/tray-{name}.rgba").write_bytes(render(svg, 32).tobytes())


def main() -> None:
    msix_assets()
    store_logos()
    ico(ROOT / "win/assets/clouddrop.ico", (16, 24, 32, 48, 64, 128, 256))
    ico(ROOT / "winapp/windows/runner/resources/app_icon.ico", (16, 20, 24, 32, 40, 48, 64, 128, 256))
    icon(256).save(ROOT / "winapp/assets/icon.png")
    tray_icons()


if __name__ == "__main__":
    main()
