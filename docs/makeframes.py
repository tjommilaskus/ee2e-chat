#!/usr/bin/env python3
"""Turn two `script -T` recordings into side-by-side SVG frames.

The screen is reconstructed by replaying the captured bytes through a minimal
terminal -- cursor positioning, erases, and SGR colour -- up to a given moment,
which is what makes the result a recording rather than a mock-up.
"""
import codecs
import html
import re
import sys

COLS, ROWS = 60, 22

# Matches the palette the application actually sets, so the frames look like the
# real thing rather than an approximation of it.
DEFAULT_FG = "#2EE64D"
BG = "#0A0E16"
PANEL = "#141B2A"


class Screen:
    def __init__(self, cols=COLS, rows=ROWS):
        self.cols, self.rows = cols, rows
        self.clear()

    def clear(self):
        self.cells = [[(" ", DEFAULT_FG) for _ in range(self.cols)] for _ in range(self.rows)]
        self.r = self.c = 0
        self.fg = DEFAULT_FG
        self.pending = ""

    def feed(self, data):
        # Chunks are split by write timing, not by anything in the stream, so an
        # escape sequence can straddle two of them. Processing each chunk alone
        # would print the tail of a split sequence as literal text.
        data = self.pending + data
        self.pending = ""
        # Held back only when the tail is a *prefix* of a sequence: an ESC with
        # nothing after it, or a CSI still collecting parameters. Testing for
        # "does not look complete" instead would treat a bare "\x1b[" as
        # finished and print the rest of the sequence as text.
        cut = data.rfind("\x1b")
        if cut != -1 and re.fullmatch(r"\x1b(\[[0-9;?]*|[()]?)", data[cut:]):
            self.pending = data[cut:]
            data = data[:cut]
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", data[i:])
                if not m:
                    m2 = re.match(r"\x1b[()][A-Za-z0-9]", data[i:])
                    i += m2.end() if m2 else 1
                    continue
                self._csi(m.group(1), m.group(2))
                i += m.end()
                continue
            if ch == "\r":
                self.c = 0
            elif ch == "\n":
                self.r += 1
                self.c = 0
            elif ch == "\b":
                self.c = max(0, self.c - 1)
            elif ch >= " ":
                if 0 <= self.r < self.rows and 0 <= self.c < self.cols:
                    self.cells[self.r][self.c] = (ch, self.fg)
                self.c += 1
            i += 1

    def _csi(self, params, cmd):
        nums = [int(p) for p in params.split(";") if p.isdigit()]
        if cmd == "H":
            p = [int(x) if x else 1 for x in params.split(";")] if params else [1, 1]
            self.r = (p[0] - 1) if p else 0
            self.c = (p[1] - 1) if len(p) > 1 else 0
        elif cmd == "J":
            self.clear()
        elif cmd == "K":
            if 0 <= self.r < self.rows:
                for x in range(self.c, self.cols):
                    self.cells[self.r][x] = (" ", DEFAULT_FG)
        elif cmd == "m":
            if len(nums) >= 5 and nums[0] == 38 and nums[1] == 2:
                self.fg = "#%02X%02X%02X" % (nums[2], nums[3], nums[4])
            elif not nums or nums == [0]:
                self.fg = DEFAULT_FG

    def lines(self):
        out = []
        for row in self.cells:
            runs, cur, colour = [], "", row[0][1]
            for ch, fg in row:
                if fg != colour:
                    runs.append((cur, colour))
                    cur, colour = ch, fg
                else:
                    cur += ch
            runs.append((cur, colour))
            out.append(runs)
        return out


def replay(out_path, time_path, until):
    """Replay a recording up to `until` seconds and return the screen."""
    data = open(out_path, "rb").read()
    screen = Screen()
    # Incremental, because a multi-byte character can straddle a chunk boundary
    # just as an escape sequence can. Decoding each chunk on its own turns the
    # box-drawing glyphs the interface is made of into replacement characters.
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    clock, offset = 0.0, 0
    for line in open(time_path):
        parts = line.split()
        if len(parts) != 2:
            continue
        delay, count = float(parts[0]), int(parts[1])
        clock += delay
        if clock > until:
            break
        screen.feed(decoder.decode(data[offset:offset + count]))
        offset += count
    return screen


CW, CH = 8.5, 18          # character cell, in points
PAD, GAP, TOP = 14, 26, 34


def svg(screens, labels):
    panel_w = COLS * CW + PAD * 2
    width = panel_w * 2 + GAP * 3
    height = ROWS * CH + TOP + PAD * 2

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0f}" height="{height:.0f}" '
        f'viewBox="0 0 {width:.0f} {height:.0f}">',
        f'<rect width="100%" height="100%" fill="{BG}"/>',
        '<style>text{font-family:"Noto Sans Mono","DejaVu Sans Mono",monospace;'
        'font-size:14px;white-space:pre}</style>',
    ]

    for idx, (screen, label) in enumerate(zip(screens, labels)):
        x0 = GAP + idx * (panel_w + GAP)
        parts.append(
            f'<rect x="{x0:.0f}" y="{TOP - 6:.0f}" width="{panel_w:.0f}" '
            f'height="{ROWS * CH + PAD:.0f}" rx="6" fill="{PANEL}"/>'
        )
        parts.append(
            f'<text x="{x0 + PAD:.0f}" y="{TOP - 14:.0f}" fill="#7A8899">{label}</text>'
        )
        for r, runs in enumerate(screen.lines()):
            y = TOP + PAD + (r + 1) * CH - 5
            x = x0 + PAD
            for text, colour in runs:
                if text.strip():
                    parts.append(
                        f'<text x="{x:.1f}" y="{y:.1f}" fill="{colour}">'
                        f'{html.escape(text)}</text>'
                    )
                x += len(text) * CW
    parts.append("</svg>")
    return "\n".join(parts)


if __name__ == "__main__":
    base, offset_b, start, stop, step = sys.argv[1], float(sys.argv[2]), \
        float(sys.argv[3]), float(sys.argv[4]), float(sys.argv[5])

    n = 0
    t = start
    while t <= stop:
        a = replay(f"{base}/alice.out", f"{base}/alice.time", t)
        # Bob started later, so his own clock runs behind the shared one.
        b = replay(f"{base}/bob.out", f"{base}/bob.time", max(0.0, t - offset_b))
        with open(f"{base}/frames/f{n:04d}.svg", "w") as fh:
            fh.write(svg([a, b], ["alice", "bob"]))
        n += 1
        t += step
    print(n)
