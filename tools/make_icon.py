# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Builds the ShredLab app icon as an SVG with the clefs as paths.

Treble clef (accent) and bass clef (light) from Noto Music side by side on
a rounded dark square, in the Stage theme's colors (BG, TREBLE_COLOR and
BASS_COLOR below). The glyphs are converted to paths, so the SVG does not
depend on the font.

Usage (needs fonttools, e.g. in .venv-models):
    python tools/make_icon.py <NotoMusic-Regular.ttf> app/src-tauri/icons/app-icon.svg
    pnpm --dir app tauri icon src-tauri/icons/app-icon.svg
"""

import sys

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

SIZE = 1024
RADIUS = 224  # rounded square corner
BG = "#0B0C10"
TREBLE_COLOR = "#4D8DFF"
BASS_COLOR = "#EEF0F6"
HEIGHT_FRACTION = 0.62  # treble clef height / tile height
GAP_EM = 0.08  # space between the clefs, in em


def glyph(font, cp):
    name = font.getBestCmap()[cp]
    gs = font.getGlyphSet()
    bp = BoundsPen(gs)
    gs[name].draw(bp)
    return gs, name, bp.bounds  # (xmin, ymin, xmax, ymax) in font units


def path_for(gs, name, scale, dx, dy):
    """SVG path of a glyph, scaled and placed with its origin at (dx, dy)
    (font y axis flipped to SVG's)."""
    pen = SVGPathPen(gs)
    gs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, dx, dy)))
    return pen.getCommands()


def main(font_path, out_path):
    font = TTFont(font_path)
    upm = font["head"].unitsPerEm
    gs, treble, tb = glyph(font, 0x1D11E)
    _, bass, bb = glyph(font, 0x1D122)

    # One font size for both clefs (they keep their typeset proportions);
    # the treble clef is HEIGHT_FRACTION of the tile.
    scale = HEIGHT_FRACTION * SIZE / (tb[3] - tb[1])
    gap = GAP_EM * upm
    # Lay out in font units on a shared baseline, then center the group.
    t_x0 = -tb[0]
    b_x0 = (tb[2] - tb[0]) + gap - bb[0]
    width = (tb[2] - tb[0]) + gap + (bb[2] - bb[0])
    ymin = min(tb[1], bb[1])
    ymax = max(tb[3], bb[3])
    ox = (SIZE - width * scale) / 2
    # SVG y of the baseline so the group's bbox is centered vertically.
    base = SIZE / 2 + (ymax + ymin) / 2 * scale

    t = path_for(gs, treble, scale, ox + t_x0 * scale, base)
    b = path_for(gs, bass, scale, ox + b_x0 * scale, base)
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" '
        f'viewBox="0 0 {SIZE} {SIZE}">\n'
        f'  <rect width="{SIZE}" height="{SIZE}" rx="{RADIUS}" fill="{BG}"/>\n'
        f'  <path fill="{TREBLE_COLOR}" d="{t}"/>\n'
        f'  <path fill="{BASS_COLOR}" d="{b}"/>\n'
        "</svg>\n"
    )
    with open(out_path, "w", encoding="utf-8", newline="\n") as f:
        f.write(svg)
    print(f"wrote {out_path} (scale {scale:.4f}, treble {tb}, bass {bb})")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
