# /// script
# dependencies = ["pillow"]
# ///
import sys
from PIL import Image, ImageDraw, ImageFilter, ImageChops
src, out = sys.argv[1], sys.argv[2]
im = Image.open(src).convert("RGB")
# Inside the rounded square = clearly darker than the white background.
gray = im.convert("L")
inside = gray.point(lambda v: 255 if v < 200 else 0)
# Fill holes (the white glyph) by flood filling the outside from a corner.
outside = inside.copy()
ImageDraw.floodfill(outside, (0, 0), 128)
mask = outside.point(lambda v: 0 if v == 128 else 255)
box = mask.getbbox()
x0, y0, x1, _ = box
i = 8
body = im.crop((x0 + i, y0 + i, x1 - i, y0 + (x1 - x0) - i))
w, h = body.size
side = 824
body = body.resize((side, side), Image.LANCZOS)
# Clean continuous-corner mask at Apple's grid size instead of the traced edge.
scale = 4
m = Image.new("L", (side * scale, side * scale), 0)
ImageDraw.Draw(m).rounded_rectangle((0, 0, side * scale - 1, side * scale - 1), radius=int(200 * scale), fill=255)
m = m.resize((side, side), Image.LANCZOS)
canvas = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
body = body.convert("RGBA"); body.putalpha(m)
canvas.paste(body, (100, 100), body)
canvas.save(out)
print("bbox", box, "->", side)
