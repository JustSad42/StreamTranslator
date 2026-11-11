"""Draws the app icon (assets/icon.ico) and the installer artwork. Run by build.ps1."""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ASSETS = Path(__file__).resolve().parent / "assets"
ASSETS.mkdir(exist_ok=True)
S = 1024
A, B = (145, 70, 255), (34, 211, 238)     # Twitch purple -> cyan


def gradient(w, h):
    g = Image.new("RGBA", (w, h))
    px = g.load()
    for y in range(h):
        for x in range(w):
            t = (x / w + y / h) / 2
            px[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(A, B)) + (255,)
    return g


def font(size, bold=True):
    for name in (("segoeuib.ttf" if bold else "segoeui.ttf"), "arialbd.ttf", "arial.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default()


# icon: rounded gradient square, speech bubble tail, "Aa"
icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
mask = Image.new("L", (S, S), 0)
md = ImageDraw.Draw(mask)
md.rounded_rectangle((60, 60, S - 60, S - 170), radius=200, fill=255)
md.polygon([(250, S - 200), (470, S - 200), (220, S - 50)], fill=255)
icon.paste(gradient(S, S), (0, 0), mask)
d = ImageDraw.Draw(icon)
f = font(400)
box = d.textbbox((0, 0), "Aa", font=f)
d.text(((S - (box[2] - box[0])) / 2 - box[0], (S - 110 - (box[3] - box[1])) / 2 - box[1]), "Aa", font=f, fill="white")
icon.save(ASSETS / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

# installer artwork (Inno Setup sizes, 100% and 200%)
for scale in (1, 2):
    w, h = 164 * scale, 314 * scale
    side = Image.new("RGB", (w, h), (12, 12, 18))
    glow = gradient(w, h).convert("RGB")
    side = Image.blend(side, glow, 0.35)
    ic = icon.resize((96 * scale, 96 * scale), Image.LANCZOS)
    side.paste(ic, ((w - ic.width) // 2, 70 * scale), ic)
    sd = ImageDraw.Draw(side)
    t = "StreamTranslator"
    ft = font(15 * scale)
    tb = sd.textbbox((0, 0), t, font=ft)
    sd.text(((w - (tb[2] - tb[0])) / 2, 190 * scale), t, font=ft, fill="white")
    sfx = "@2x" if scale == 2 else ""
    side.save(ASSETS / f"wizard-side{sfx}.bmp")
    small = Image.new("RGB", (55 * scale, 58 * scale), (255, 255, 255))
    ic2 = icon.resize((48 * scale, 48 * scale), Image.LANCZOS)
    small.paste(ic2, ((small.width - ic2.width) // 2, (small.height - ic2.height) // 2), ic2)
    small.save(ASSETS / f"wizard-small{sfx}.bmp")
print("Icon and installer art written to", ASSETS)
