#!/usr/bin/env python3
"""Draw Space's app icon: a black S on a "sunrise" tile.

The S is one even-weight stroke: a bottom bar, a straight stem at 68 degrees and a top bar of
the same length, joined by tight bends, with both ends cut square. The tile is a radial gradient
from an amber core below centre through orange, red, a thin sage band and periwinkle to
violet-blue at the rim, blended in OKLab so the bands stay clean.

The icon is an Icon Composer bundle with two layers (the S in front, the gradient behind), which
macOS 26 masks to its icon shape and lights with its glass effect. That one bundle covers every
surface macOS draws the app icon on: Dock, app switcher, Finder and notifications.

Writes to crates/space-app/resources/:
  AppIcon.icon/  the Icon Composer bundle (source of truth for the bundled app)
  Assets.car     compiled from it by Xcode's actool; scripts/bundle.sh ships it
  AppIcon.png    flattened and pre-masked; the Dock icon for a bare `cargo run` binary
Needs Pillow and Xcode.  Run: python3 scripts/make_app_icon.py
"""
import json, math, os, pathlib, shutil, subprocess, tempfile
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/space-app/resources"
DEVELOPER_DIR = "/Applications/Xcode.app/Contents/Developer"
SIZE = 1024
SS = 4  # supersampling for the S edges

# ----- the S (glyph units; scaled into the tile below) ---------------------------------------

ANGLE = 68.2    # stem angle from horizontal, degrees
BAR = 14.0      # length of each bar, from its outer end to the stem's corner
HEIGHT = 28.9   # distance between the bars' centre lines
BEND = 10.6     # centre-line radius of both bends
WEIGHT = 7.3    # stroke width
MARK = 560      # the S's larger dimension in the 1024 tile, stroke included
INK = (10, 10, 11)

# ----- the tile -------------------------------------------------------------------------------

CORE = (512, 820)   # gradient centre
RADIUS = 880
BANDS = [(0.00, "#ffd36b"), (0.16, "#ff8a3d"), (0.30, "#e5452e"), (0.46, "#f0804e"),
         (0.60, "#9fc1b4"), (0.74, "#6a86e6"), (1.00, "#5b5ce8")]


def s_outline():
    """Closed outline of the S in tile pixels: the centre line offset by half the weight."""
    dx = HEIGHT / math.tan(math.radians(ANGLE))
    corners = [(0.0, HEIGHT), (BAR, HEIGHT), (BAR + dx, 0.0), (2 * BAR + dx, 0.0)]
    line = centre_line(corners, BEND, step=0.05)
    xs, ys = [p[0] for p in line], [p[1] for p in line]
    k = MARK / max(max(xs) - min(xs) + WEIGHT, max(ys) - min(ys) + WEIGHT)
    cx, cy = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2
    line = [((x - cx) * k + SIZE / 2, (y - cy) * k + SIZE / 2) for x, y in line]
    h = WEIGHT * k / 2
    left, right = [], []
    for i, (x, y) in enumerate(line):
        (ax, ay), (bx, by) = line[max(i - 1, 0)], line[min(i + 1, len(line) - 1)]
        length = math.hypot(bx - ax, by - ay) or 1
        nx, ny = -(by - ay) / length, (bx - ax) / length
        left.append((x + nx * h, y + ny * h))
        right.append((x - nx * h, y - ny * h))
    return left + right[::-1]


def centre_line(corners, radius, step):
    """Polyline through `corners` with each inner corner rounded to `radius`, densely sampled."""
    pts, start = [], corners[0]
    for a, b, c in zip(corners, corners[1:], corners[2:]):
        v1, v2 = unit(a[0] - b[0], a[1] - b[1]), unit(c[0] - b[0], c[1] - b[1])
        turn = math.acos(max(-1.0, min(1.0, v1[0] * v2[0] + v1[1] * v2[1])))
        reach = radius / math.tan(turn / 2)
        p1 = (b[0] + v1[0] * reach, b[1] + v1[1] * reach)
        p2 = (b[0] + v2[0] * reach, b[1] + v2[1] * reach)
        bis = unit(v1[0] + v2[0], v1[1] + v2[1])
        centre = (b[0] + bis[0] * radius / math.sin(turn / 2), b[1] + bis[1] * radius / math.sin(turn / 2))
        pts += segment(start, p1, step)
        a0 = math.atan2(p1[1] - centre[1], p1[0] - centre[0])
        sweep = (math.atan2(p2[1] - centre[1], p2[0] - centre[0]) - a0 + 3 * math.pi) % (2 * math.pi) - math.pi
        n = max(int(abs(sweep) * radius / step), 2)
        pts += [(centre[0] + radius * math.cos(a0 + sweep * i / n), centre[1] + radius * math.sin(a0 + sweep * i / n))
                for i in range(n)]
        start = p2
    return pts + segment(start, corners[-1], step) + [corners[-1]]


def segment(p, q, step):
    n = max(int(math.dist(p, q) / step), 1)
    return [(p[0] + (q[0] - p[0]) * i / n, p[1] + (q[1] - p[1]) * i / n) for i in range(n)]


def unit(x, y):
    length = math.hypot(x, y) or 1
    return x / length, y / length


# ----- OKLab gradient -------------------------------------------------------------------------

def srgb_to_oklab(hex_colour):
    def linear(c):
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4
    r, g, b = (linear(int(hex_colour[i:i + 2], 16) / 255) for i in (1, 3, 5))
    l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    return (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s)


def oklab_to_srgb(lab):
    L, a, b = lab
    l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3
    m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3
    s = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3
    rgb = (4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
           -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
           -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s)

    def encode(c):
        c = max(0.0, min(1.0, c))
        return round(255 * (12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055))
    return tuple(encode(c) for c in rgb)


def gradient_lut(steps=2048):
    labs = [(t, srgb_to_oklab(c)) for t, c in BANDS]
    lut = []
    for i in range(steps):
        u = i / (steps - 1)
        for (t0, c0), (t1, c1) in zip(labs, labs[1:]):
            if u <= t1:
                f = (u - t0) / (t1 - t0)
                lut.append(oklab_to_srgb(tuple(x + (y - x) * f for x, y in zip(c0, c1))))
                break
    return lut


def background():
    lut = gradient_lut()
    top = len(lut) - 1
    px = [lut[min(int(math.hypot(x - CORE[0], y - CORE[1]) / RADIUS * top), top)]
          for y in range(SIZE) for x in range(SIZE)]
    img = Image.new("RGB", (SIZE, SIZE))
    img.putdata(px)
    return img.convert("RGBA")


def s_layer():
    big = Image.new("RGBA", (SIZE * SS, SIZE * SS), (0, 0, 0, 0))
    ImageDraw.Draw(big).polygon([(x * SS, y * SS) for x, y in s_outline()], fill=INK + (255,))
    return big.resize((SIZE, SIZE), Image.LANCZOS)


def premasked(flat):
    """Flattened art on the standard macOS icon grid (824 px body), for the bare-binary Dock icon."""
    body = 824
    mask = Image.new("L", (body * SS, body * SS), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, body * SS - 1, body * SS - 1], 186 * SS, fill=255)
    out = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    out.paste(flat.resize((body, body), Image.LANCZOS), (100, 100), mask.resize((body, body), Image.LANCZOS))
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    tile, mark = background(), s_layer()

    flat = tile.copy()
    flat.alpha_composite(mark)
    premasked(flat).save(OUT / "AppIcon.png")

    bundle = OUT / "AppIcon.icon"
    shutil.rmtree(bundle, ignore_errors=True)
    (bundle / "Assets").mkdir(parents=True)
    mark.save(bundle / "Assets" / "s.png")
    tile.convert("RGB").save(bundle / "Assets" / "sunrise.png")
    rim = BANDS[-1][1]
    (bundle / "icon.json").write_text(json.dumps({
        # Behind the full-bleed gradient layer; set so nothing of the system default shows.
        "fill": {"solid": "srgb:" + ",".join(f"{int(rim[i:i + 2], 16) / 255:.5f}" for i in (1, 3, 5)) + ",1.00000"},
        "groups": [
            {"layers": [{"image-name": "s.png", "name": "s"}],
             "shadow": {"kind": "neutral", "opacity": 0.35}, "specular": True},
            {"layers": [{"image-name": "sunrise.png", "name": "sunrise", "glass": False}],
             "shadow": {"kind": "none", "opacity": 0}, "specular": False},
        ],
        "supported-platforms": {"squares": ["macOS"]},
    }, indent=2) + "\n")

    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(["xcrun", "actool", str(bundle), "--compile", tmp, "--platform", "macosx",
                        "--minimum-deployment-target", "13.0", "--app-icon", "AppIcon",
                        "--output-partial-info-plist", str(pathlib.Path(tmp) / "partial.plist"),
                        "--errors", "--warnings"],
                       check=True, env=dict(os.environ, DEVELOPER_DIR=DEVELOPER_DIR), stdout=subprocess.DEVNULL)
        shutil.copy(pathlib.Path(tmp) / "Assets.car", OUT / "Assets.car")
    print("wrote AppIcon.icon, Assets.car, AppIcon.png in", OUT.relative_to(ROOT))


if __name__ == "__main__":
    main()
