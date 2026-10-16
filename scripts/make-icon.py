"""Draws the redjelly icon (red rounded square, white jelly drop with a play
button) and writes installer/redjelly.ico plus app/ui/icon.png.

    python3 scripts/make-icon.py
"""
import math
import pathlib

from PIL import Image, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
N = 1024  # draw large, downsample for clean edges


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def draw() -> Image.Image:
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))

    # Background: diagonal red gradient in a rounded square.
    grad = Image.new("RGB", (N, N))
    top, bottom = (255, 104, 108), (176, 30, 44)
    px = grad.load()
    for y in range(N):
        for x in range(N):
            px[x, y] = lerp(top, bottom, (x + y) / (2 * N))
    mask = Image.new("L", (N, N), 0)
    ImageDraw.Draw(mask).rounded_rectangle([40, 40, N - 40, N - 40], radius=220, fill=255)
    img.paste(grad, (0, 0), mask)

    # Soft shadow under the drop.
    shadow = Image.new("L", (N, N), 0)
    d = ImageDraw.Draw(shadow)
    cx, cy, r = N // 2, 590, 250
    d.ellipse([cx - r, cy - r + 30, cx + r, cy + r + 30], fill=110)
    shadow = shadow.filter(ImageFilter.GaussianBlur(40))
    img.paste((90, 10, 20, 255), (0, 0), shadow)

    # Jelly drop: circle plus a pointed top.
    drop = Image.new("L", (N, N), 0)
    d = ImageDraw.Draw(drop)
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)
    tip_y = 150
    angle = math.asin(r / (cy - tip_y))
    left = (cx - r * math.cos(angle), cy - r * math.sin(angle))
    right = (cx + r * math.cos(angle), cy - r * math.sin(angle))
    d.polygon([(cx, tip_y), left, right], fill=255)
    img.paste((255, 255, 255, 255), (0, 0), drop)

    # Play button inside the drop.
    s = 120
    ImageDraw.Draw(img).polygon(
        [(cx - s * 0.75, cy - s), (cx - s * 0.75, cy + s), (cx + s * 1.05, cy)], fill=(214, 52, 62, 255)
    )
    return img


big = draw()
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
icon = big.resize((256, 256), Image.LANCZOS)
icon.save(ROOT / "installer" / "redjelly.ico", sizes=[(s, s) for s in sizes])
big.resize((256, 256), Image.LANCZOS).save(ROOT / "app" / "ui" / "icon.png")
print("wrote installer/redjelly.ico, app/ui/icon.png")

# Installer artwork: welcome/finish sidebar (164x314) and page header (150x57),
# both 24-bit BMP as NSIS requires.
def gradient(w, h, a, b):
    g = Image.new("RGB", (w, h))
    p = g.load()
    for y in range(h):
        for x in range(w):
            p[x, y] = lerp(a, b, (x / w + y / h) / 2)
    return g


side = gradient(164, 314, (255, 104, 108), (150, 24, 38))
logo = big.resize((112, 112), Image.LANCZOS)
side.paste(logo, ((164 - 112) // 2, 70), logo)
d = ImageDraw.Draw(side)
try:
    from PIL import ImageFont

    font = ImageFont.truetype("DejaVuSans-Bold.ttf", 24)
except OSError:
    font = None
text = "redjelly"
w = d.textlength(text, font=font)
d.text(((164 - w) / 2, 200), text, fill=(255, 255, 255), font=font)
side.save(ROOT / "installer" / "sidebar.bmp")

header = Image.new("RGB", (150, 57), (255, 255, 255))
small = big.resize((44, 44), Image.LANCZOS)
header.paste(small, (150 - 44 - 6, 6), small)
header.save(ROOT / "installer" / "header.bmp")
print("wrote installer/sidebar.bmp, installer/header.bmp")
