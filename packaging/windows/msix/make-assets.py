#!/usr/bin/env python3
"""Renders the Microsoft Store package images (packaging/windows/msix/Assets) from
the app icon. Needs python3-gi with librsvg. Run again after changing the icon."""

from pathlib import Path

import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg  # noqa: E402
import cairo  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]
ICON = ROOT / "data/icons/hicolor/scalable/apps/com.dhivalabs.CloudDrop.svg"
OUT = Path(__file__).resolve().parent / "Assets"


def render(name: str, width: int, height: int, icon: int) -> None:
    """The icon, `icon` px square, centred on a transparent `width`×`height` image."""
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, width, height)
    ctx = cairo.Context(surface)
    handle = Rsvg.Handle.new_from_file(str(ICON))
    viewport = Rsvg.Rectangle()
    viewport.x, viewport.y = (width - icon) / 2, (height - icon) / 2
    viewport.width = viewport.height = icon
    handle.render_document(ctx, viewport)
    surface.write_to_png(str(OUT / name))


def main() -> None:
    OUT.mkdir(exist_ok=True)
    for old in OUT.glob("*.png"):
        old.unlink()
    for scale in (100, 200, 400):
        f = scale / 100
        render(f"StoreLogo.scale-{scale}.png", round(50 * f), round(50 * f), round(50 * f))
        render(f"Square44x44Logo.scale-{scale}.png", round(44 * f), round(44 * f), round(36 * f))
        render(f"Square150x150Logo.scale-{scale}.png", round(150 * f), round(150 * f), round(96 * f))
        render(f"Wide310x150Logo.scale-{scale}.png", round(310 * f), round(150 * f), round(96 * f))
    # Taskbar, Start and file icons, without the coloured tile plate.
    for size in (16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256):
        for qualifiers in (f"targetsize-{size}", f"targetsize-{size}_altform-unplated"):
            render(f"Square44x44Logo.{qualifiers}.png", size, size, size)


if __name__ == "__main__":
    main()
