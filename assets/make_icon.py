"""Generates assets/icon.ico: white magnifier on a blue rounded square."""
from pathlib import Path
from PIL import Image, ImageDraw

S = 1024  # draw large, downsample for smooth edges
BG, FG = (0, 95, 184, 255), (255, 255, 255, 255)

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
d.rounded_rectangle((32, 32, S - 32, S - 32), radius=220, fill=BG)
cx, cy, r, w = 440, 440, 210, 90
d.ellipse((cx - r, cy - r, cx + r, cy + r), outline=FG, width=w)
d.line((cx + 150, cy + 150, 790, 790), fill=FG, width=w + 30)
d.ellipse((790 - 60, 790 - 60, 790 + 60, 790 + 60), fill=FG)

out = Path(__file__).with_name("icon.ico")
img.save(out, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (256, 256)])
print(out, out.stat().st_size, "bytes")
