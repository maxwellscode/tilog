#!/usr/bin/env python3
"""Records a short session of tilog and renders it as docs/demo.gif and two screenshots.

It runs the real program in a tmux server of its own (`tmux -L tilog-demo`, so no other tmux
is touched), on copies of the example logs that `examples/live.sh` keeps writing to, captures
the screen after every step, and draws each capture with Pillow.

    cargo build --release
    python3 docs/demo/make_demo.py

Needs: tmux, Python 3 with Pillow, and a monospace font (Menlo on macOS, DejaVu Sans Mono on
Linux; `--font` takes a path).
"""
import argparse, os, re, shutil, subprocess, sys, tempfile, time
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SOCKET = "tilog-demo"
COLS, ROWS = 100, 28
SOURCES = ["example_spring_boot.log", "example_nginx_access.log", "example_nodejs_json.log",
           "example_postgres.log"]

# A calm dark palette for the 16 ANSI colors, and the default colors.
BG, FG = (30, 30, 46), (205, 214, 244)
ANSI = [(69, 71, 90), (243, 139, 168), (166, 227, 161), (249, 226, 175), (137, 180, 250),
        (245, 194, 231), (148, 226, 213), (186, 194, 222), (108, 112, 134), (243, 139, 168),
        (166, 227, 161), (249, 226, 175), (137, 180, 250), (245, 194, 231), (148, 226, 213),
        (205, 214, 244)]

def color256(n):
    if n < 16:
        return ANSI[n]
    if n < 232:
        n -= 16
        level = [0, 95, 135, 175, 215, 255]
        return (level[n // 36], level[n // 6 % 6], level[n % 6])
    v = 8 + (n - 232) * 10
    return (v, v, v)

# ------------------------------------------------------------------ tmux
def tmux(*args):
    return subprocess.run(["tmux", "-L", SOCKET, *args], capture_output=True, text=True)

def keys(*names, literal=False):
    tmux("send-keys", "-t", "demo", *(["-l"] if literal else []), *names)

def capture():
    return tmux("capture-pane", "-t", "demo", "-p", "-e").stdout

# ------------------------------------------------------------------ ANSI to cells
SGR = re.compile(r"\x1b\[([0-9;]*)m")

def parse(screen):
    """Rows of cells (char, fg, bg, bold, dim) from the text of `tmux capture-pane -e`."""
    rows = []
    fg, bg, bold, dim, reverse = None, None, False, False, False
    for line in screen.split("\n")[:ROWS]:
        cells, pos = [], 0
        for m in SGR.finditer(line):
            cells += [(c, fg, bg, bold, dim, reverse) for c in line[pos:m.start()]]
            pos = m.end()
            codes = [int(c) if c else 0 for c in m.group(1).split(";")]
            i = 0
            while i < len(codes):
                c = codes[i]
                if c == 0: fg, bg, bold, dim, reverse = None, None, False, False, False
                elif c == 1: bold = True
                elif c == 2: dim = True
                elif c == 22: bold = dim = False
                elif c == 7: reverse = True
                elif c == 27: reverse = False
                elif c == 39: fg = None
                elif c == 49: bg = None
                elif 30 <= c <= 37: fg = ANSI[c - 30]
                elif 90 <= c <= 97: fg = ANSI[c - 90 + 8]
                elif 40 <= c <= 47: bg = ANSI[c - 40]
                elif 100 <= c <= 107: bg = ANSI[c - 100 + 8]
                elif c in (38, 48) and i + 1 < len(codes):
                    if codes[i + 1] == 5 and i + 2 < len(codes):
                        col = color256(codes[i + 2]); i += 2
                    elif codes[i + 1] == 2 and i + 4 < len(codes):
                        col = tuple(codes[i + 2:i + 5]); i += 4
                    else:
                        col = None
                    if c == 38: fg = col
                    else: bg = col
                i += 1
        cells += [(c, fg, bg, bold, dim, reverse) for c in line[pos:]]
        cells += [(" ", None, None, False, False, False)] * (COLS - len(cells))
        rows.append(cells[:COLS])
    rows += [[(" ", None, None, False, False, False)] * COLS] * (ROWS - len(rows))
    return rows

def blend(a, b, t):
    return tuple(int(x * (1 - t) + y * t) for x, y in zip(a, b))

class Renderer:
    def __init__(self, font_path, size):
        self.regular = ImageFont.truetype(font_path, size)
        try:
            self.bold = ImageFont.truetype(font_path, size, index=1)
        except Exception:
            self.bold = self.regular
        self.cw = round(self.regular.getlength("M"))
        asc, desc = self.regular.getmetrics()
        self.ch = asc + desc + 2
        self.pad, self.bar = 16, 34
        self.size = (COLS * self.cw + 2 * self.pad, ROWS * self.ch + 2 * self.pad + self.bar)

    def draw(self, screen):
        img = Image.new("RGB", self.size, (24, 24, 37))
        d = ImageDraw.Draw(img)
        for i, c in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
            d.ellipse([16 + i * 22, 11, 28 + i * 22, 23], fill=c)
        d.text((self.size[0] // 2, 17), "tilog", fill=(108, 112, 134), font=self.regular, anchor="mm")
        y0 = self.bar + self.pad
        for r, row in enumerate(parse(screen)):
            for c, (ch, fg, bg, bold, dim, reverse) in enumerate(row):
                f, b = fg or FG, bg or BG
                if reverse:
                    f, b = b, f
                x, y = self.pad + c * self.cw, y0 + r * self.ch
                if b != BG:
                    d.rectangle([x, y, x + self.cw, y + self.ch], fill=b)
                if ch != " ":
                    if dim:
                        f = blend(f, b, 0.5)
                    d.text((x, y), ch, fill=f, font=self.bold if bold else self.regular)
        # the window's background behind the cells
        return img

# ------------------------------------------------------------------ the session
def record(binary):
    work = tempfile.mkdtemp(prefix="tilog-demo-")
    for name in SOURCES:
        shutil.copy(os.path.join(ROOT, "examples", name), work)
    writer = subprocess.Popen(
        [os.path.join(ROOT, "examples", "live.sh"), "90", "--rate", "7", "--dir", work, "-q"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    tmux("kill-server")
    tmux("new-session", "-d", "-s", "demo", "-x", str(COLS), "-y", str(ROWS),
         "-c", work, f"{binary} " + " ".join(SOURCES))
    frames = []  # (screen, seconds)

    def snap(seconds, wait=0.35):
        time.sleep(wait)
        frames.append((capture(), seconds))

    try:
        time.sleep(2.5)
        snap(2.2)                                   # the overview: four sources, all live
        keys("1"); snap(1.3)                        # one source on its own
        keys("&"); snap(0.5, 0.2)
        for ch in "ERROR":
            keys(ch, literal=True); snap(0.18, 0.1)
        keys("Enter"); snap(2.0, 0.8)               # a filter tile next to the live log
        keys("n"); snap(2.4, 0.5)                   # n: the main pane shows the first match...
        keys("n"); snap(2.4, 0.5)                   # ...and the next one, in its place
        keys("Tab"); keys("g"); snap(0.8, 0.4)
        keys(":"); snap(0.4, 0.2)
        for ch in "fo":
            keys(ch, literal=True); snap(0.25, 0.1)  # the command menu narrows down
        snap(2.0, 0.3)
        keys("Escape"); keys("q"); snap(1.5, 0.5)    # back to the overview
    finally:
        writer.terminate()
        tmux("kill-server")
        shutil.rmtree(work, ignore_errors=True)
    return frames

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(ROOT, "target", "release", "tilog"))
    ap.add_argument("--out", default=os.path.join(ROOT, "docs"))
    ap.add_argument("--size", type=int, default=15)
    ap.add_argument("--font", default=None)
    args = ap.parse_args()
    font = args.font
    for candidate in ([font] if font else []) + [
            "/System/Library/Fonts/Menlo.ttc", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"]:
        if candidate and os.path.exists(candidate):
            font = candidate
            break
    else:
        sys.exit("no monospace font found: pass --font")
    frames = record(args.bin)
    r = Renderer(font, args.size)
    images = [r.draw(screen) for screen, _ in frames]

    # one palette for all frames, so the colors do not flicker
    sheet = Image.new("RGB", (images[0].width // 3, images[0].height // 3 * len(images)))
    for i, im in enumerate(images):
        sheet.paste(im.resize((im.width // 3, im.height // 3)), (0, i * (im.height // 3)))
    palette = sheet.quantize(colors=48, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    small = [im.quantize(palette=palette, dither=Image.Dither.NONE) for im in images]
    os.makedirs(args.out, exist_ok=True)
    small[0].save(os.path.join(args.out, "demo.gif"), save_all=True, append_images=small[1:],
                  duration=[int(s * 1000) for _, s in frames], loop=0, optimize=True,
                  disposal=1)
    # two stills for the README: the overview, and a filter with the main pane following it
    images[0].save(os.path.join(args.out, "screenshot-overview.png"), optimize=True)
    images[min(len(images) - 1, 9)].save(os.path.join(args.out, "screenshot-filter.png"), optimize=True)
    print("wrote docs/demo.gif and two screenshots,", len(frames), "frames")

main()
