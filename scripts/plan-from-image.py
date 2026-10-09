#!/usr/bin/env python3
"""Walls of a floor plan image, in cm, for Moli's drawn plan (D11).

An agent's first step when it redraws a house from a photo or a screenshot
of its plan: the walls (the plan's dark-grey fills) become rectangles in cm.
Rooms, openings and fixtures are then written by the agent (types and
format: docs/components/api.md, /api/plan), and checked on the overlay.

    python scripts/plan-from-image.py plan.png --scale 0.848 --out etage.json
        [--crop TOP BOTTOM]   rows to keep (default: drop black phone bars)
        [--dark 110]          what counts as a wall (grey under this level)

`--scale`: cm per pixel, from a dimension written on the plan (a room's
433 cm measured over 511 px: 0.848). Prints the floor's size; writes the
JSON `{ "size": [w, h], "walls": [...] }` and `<out>.png`, the walls drawn
in red over the image, to check before going further.

Needs Pillow and NumPy.
"""
import argparse
import json
import pathlib

import numpy as np
from PIL import Image, ImageDraw


def crop_rows(a):
    """The rows that are not the black bars of a phone screenshot."""
    rows = np.where(a.reshape(a.shape[0], -1).mean(axis=1) > 60)[0]
    return int(rows.min()), int(rows.max())


def wall_mask(a, dark):
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    return (abs(r - g) < 14) & (abs(g - b) < 14) & (r < dark)


def rectangles(mask, min_side=6, min_long=20):
    """Greedy cover of the mask by rectangles at least `min_side` thick."""
    m = mask.copy()
    h, w = m.shape
    out = []
    for y in range(h):
        row = m[y]
        if not row.any():
            continue
        x = 0
        while x < w:
            if not row[x]:
                x += 1
                continue
            x2 = x
            while x2 < w and row[x2]:
                x2 += 1
            y2 = y + 1
            while y2 < h and m[y2, x:x2].all():
                y2 += 1
            if x2 - x >= min_side and y2 - y >= min_side:
                if max(x2 - x, y2 - y) >= min_long:
                    out.append([x, y, x2, y2])
                m[y:y2, x:x2] = False
            else:
                m[y, x:x2] = False
            x = x2
    return out


def merge(rs):
    """Pieces of the same wall (same span, touching) become one."""
    rs = [list(r) for r in rs]
    changed = True
    while changed:
        changed = False
        for i, a in enumerate(rs):
            if a is None:
                continue
            for j, b in enumerate(rs):
                if i == j or b is None:
                    continue
                same_cols = abs(a[0] - b[0]) <= 3 and abs(a[2] - b[2]) <= 3 and b[1] <= a[3] + 2 and b[3] >= a[1] - 2
                same_rows = abs(a[1] - b[1]) <= 3 and abs(a[3] - b[3]) <= 3 and b[0] <= a[2] + 2 and b[2] >= a[0] - 2
                if same_cols or same_rows:
                    a[:] = [min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3])]
                    rs[j] = None
                    changed = True
        rs = [r for r in rs if r is not None]
    return rs


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("image")
    ap.add_argument("--scale", type=float, required=True, help="cm per pixel")
    ap.add_argument("--out", required=True)
    ap.add_argument("--crop", type=int, nargs=2, metavar=("TOP", "BOTTOM"))
    ap.add_argument("--dark", type=int, default=110)
    args = ap.parse_args()

    image = Image.open(args.image).convert("RGB")
    a = np.asarray(image).astype(int)
    top, bottom = args.crop or crop_rows(a)
    a = a[top : bottom + 1]
    walls = merge(rectangles(wall_mask(a, args.dark)))
    s = args.scale
    cm = [
        {"x": round(x * s), "y": round(y * s), "w": round((x2 - x) * s), "h": round((y2 - y) * s)}
        for x, y, x2, y2 in walls
    ]
    cm = [w for w in cm if w["w"] >= 4 and w["h"] >= 4]
    size = [round(a.shape[1] * s), round(a.shape[0] * s)]
    out = pathlib.Path(args.out)
    out.write_text(json.dumps({"size": size, "walls": cm}, indent=1) + "\n", encoding="utf-8")

    check = image.crop((0, top, image.width, bottom + 1))
    draw = ImageDraw.Draw(check)
    for x, y, x2, y2 in walls:
        draw.rectangle((x, y, x2 - 1, y2 - 1), outline=(255, 0, 0), width=3)
    check.save(out.with_suffix(".png"))
    print(f"{len(cm)} walls, floor {size[0]} × {size[1]} cm (rows {top}–{bottom}); check {out.with_suffix('.png')}")


if __name__ == "__main__":
    main()
