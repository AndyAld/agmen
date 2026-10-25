"""FigCraft `run` script: agmen desktop app (Zed style) and terminal UI (vim style).

Colors come from the user's Zed theme "Gruvbox Material Dark Hard"
(~/.config/zed/themes/Gruvbox Material Mix.json).
"""

import json
import os
import pathlib
import re
import sys

steps = []
links = []  # (src, dest_name, trigger_key or None)
screens = {}

UI = "SF Pro Text"
BUF = "CaskaydiaCove Nerd Font Mono"
CW = 7.62  # measured: 100 chars of BUF at 13 px = 762 px
LH = 18

BG = "#1d2021"
SURF = "#282828"
SEL = "#3c3836"
BORDER = "#3c3836"
BORDER2 = "#282828"
TEXT = "#d8bf94"
BRIGHT = "#e0c89e"
FAINT = "#928374"
DIM = "#7c6f64"
LINE = "#504945"
ACCENT = "#72b9a9"
ERR = "#ed675f"
WARN = "#dda752"
OK = "#afbf5c"
MAG = "#da7f98"
GRN = "#82be78"
ORG = "#ea894b"

ROLE = {
    "listener": ("Listener", "#a89984"),
    "coordinator": ("Coordinator", MAG),
    "planner": ("Planner", ACCENT),
    "researcher": ("Researcher", GRN),
    "engineer": ("Engineer", ORG),
    "tester": ("Tester", OK),
    "reviewer": ("Reviewer", ERR),
}
STATUS = {"working": ("●", OK), "idle": ("○", FAINT), "reading": ("◐", ACCENT)}

TREE = [
    ("andy (you)", [
        (0, "listen", "listener", "codex", "idle"),
        (1, "fx1", "engineer", "claude", "working"),
        (1, "t7k", "tester", "opencode", "working"),
        (2, "rv2", "reviewer", "grok", "reading"),
        (0, "a1", "engineer", "claude", "idle"),
    ]),
    ("sean", [(0, "a1", "engineer", "codex", "working"), (0, "plan", "planner", "gemini", "idle")]),
    ("adam", [(0, "coord", "coordinator", "claude", "working"),
              (0, "res", "researcher", "agy", "working")]),
]

# (time, handle, role or None for human, verb, text, kind) kind: msg | reply | ledger | mention
CHAT = [
    ("15:27", "andy/a1", "engineer", "DONE",
     "C014 feat/c014-release-matrix@7c8835e · PR #9 · release.yml matrix: windows, macos-26, linux",
     "msg"),
    ("15:28", "andy/a1", "engineer", "ALERT",
     "main does not compile: 4d9fdc8 committed conflict markers at src/main.rs:2-8. "
     "@sean/a1 please hold pushes to main until the fix lands.", "msg"),
    ("15:31", "andy", None, "to listen",
     "Spawn another worker to fix main and keep Sean's changes", "msg"),
    ("15:31", "andy/listen", "listener", "ANSWER",
     "git_show: Sean's 016f235 and 4d9fdc8 are on main; 4d9fdc8 left markers at "
     "src/main.rs:2-8. Delegating.", "reply"),
    ("15:31", "andy/listen", "listener", "DELEGATED",
     "andy/fx1 · Engineer · claude · sonnet · $5.00 · fix/c015-main-conflict", "reply"),
    ("15:33", "hub", None, "GRANT", "C015 to andy/fx1, lease 30 min", "ledger"),
    ("15:41", "andy/fx1", "engineer", "DONE",
     "C015 fix/c015-main-conflict@9e2a71c · cargo test 71 passed, 0 failed · keeps "
     "cylinder, document, minimap and tabs from both sides", "msg"),
    ("15:44", "andy/rv2", "reviewer", "REVIEW",
     "pass · 0 findings · traced the mod list and both callers", "msg"),
    ("15:45", "sean", None, "FYI", "@andy merging #10 after CI. Sorry about the markers.",
     "mention"),
]
VERB_COLOR = {"DONE": OK, "ALERT": ERR, "REVIEW": ERR, "DELEGATED": ACCENT, "ANSWER": TEXT,
              "GRANT": DIM, "FYI": TEXT, "to listen": ACCENT, "PROPOSAL": ACCENT,
              "DECISION": OK, "ASSIGN": MAG, "CLAIM": ORG, "to sean/plan": ACCENT, "STATE": DIM,
              "SETTINGS": ACCENT}
HUMAN_TAG = {"adam": "human · coordinator"}
ANSWER_MSG = ("15:59", "adam", None, "ANSWER",
              "Revert #10 now. @sean/a1 hotfixes it_tabs on fix/c016-it-tabs. Clearing this.",
              "reply")
SETTINGS_CHAT = [
    ("15:50", "andy", None, "to listen",
     "Give andy/fx1 edit access to L-INFRA until 18:00 and raise its budget to $8.", "msg"),
    ("15:50", "andy/listen", "listener", "SETTINGS",
     "Done. andy/fx1 can edit L-INFRA deploy/** until 18:00, and its budget is $8.00.", "reply"),
    ("15:51", "sean", None, "FYI", "@andy/listen turn off push approval for andy's agents",
     "msg"),
    ("15:51", "andy/listen", "listener", "ANSWER",
     "Only andy can change andy's settings, so I changed nothing. @andy, sean asked for this.",
     "reply"),
]
ALERT_MSG = ("15:58", "adam/coord", "coordinator", "ALERT",
             "@andy CI on main is red after #10 merged: it_tabs, 3 failed. The release tag is "
             "blocked. Revert #10, or hotfix on a branch?", "alert")

SESSION = [  # (kind, a, b) kinds: tool, out, say, cost
    ("tool", "Read", "src/main.rs"),
    ("tool", "Bash", "git show 4d9fdc8 --stat"),
    ("out", "2 files changed, 14 insertions(+), 3 deletions(-)", FAINT),
    ("say", "Both sides add modules. Keeping cylinder, document, minimap and tabs; "
            "dropping the conflict markers.", None),
    ("tool", "Edit", "src/main.rs  +6 -9"),
    ("tool", "Bash", "cargo test"),
    ("out", "test result: ok. 71 passed; 0 failed", OK),
    ("tool", "Bash", "git push -u origin fix/c015-main-conflict"),
    ("out", "ok", FAINT),
    ("cost", "turn 4 · $0.31 · 44 s", None),
    ("tool", "agmen", "done C015"),
    ("out", "recorded #1866 (feed)", ACCENT),
]



HANDLE_ROLE = {"andy/listen": "listener", "andy/fx1": "engineer", "andy/t7k": "tester",
               "andy/rv2": "reviewer", "andy/a1": "engineer", "sean/a1": "engineer",
               "sean/plan": "planner", "adam/coord": "coordinator", "adam/res": "researcher",
               "andy/coord": "coordinator", "andy/core": "engineer", "andy/app": "engineer",
               "andy/rev": "reviewer", "sean/sync": "engineer", "sean/qa": "tester",
               "sean/tiles": "researcher"}
REF_RE = re.compile(
    r"(?P<who>(?<![\w/])@?(?:andy|sean|adam)\b(?:/[a-z0-9]+)?)"
    r"|(?P<branch>\b(?:fix|feat|test|docs|plan)/[a-z0-9-]+(?:@[0-9a-f]{7})?)"
    r"|(?P<file>(?<![\w/])[\w./-]+\.(?:rs|ts|md|yml|toml|lock|xml)(?::\d+(?:-\d+)?)?)"
    r"|(?P<item>\b(?:[A-Z]-\d{1,3}|[A-Z]\d{3})\b)"
    r"|(?P<lane>\bL-[A-Z]+\b)"
    r"|(?P<pr>#\d+)"
    r"|(?P<commit>\b[0-9a-f]{7}\b)")


def ref_kind(m):
    k = m.lastgroup
    if k == "who":
        return "agent" if "/" in m.group(0) else "user"
    return k


def ref_color(ref, kind):
    if kind == "agent":
        return ROLE[HANDLE_ROLE.get(ref.lstrip("@"), "engineer")][1]
    return {"user": BRIGHT, "branch": OK, "file": ACCENT, "item": MAG, "lane": WARN,
            "pr": ACCENT, "commit": ACCENT}[kind]


def ref_target(ref, kind):
    r = ref.lstrip("@")
    if kind == "agent" and r == "andy/fx1":
        return "Desktop · agent"
    if kind == "branch" and r.startswith("fix/c015"):
        return "Desktop · git"
    if kind == "file" and "main.rs" in r:
        return "Desktop · project"
    if kind == "commit" and r in ("9e2a71c", "4d9fdc8", "016f235"):
        return "Desktop · git"
    return None


def rich_body(parent, body, size=13):
    w = frame(parent, "body", layout="horizontal", wrap=True, gap=4, crossGap=3,
              sizingH="fill", sizingV="hug", crossAlign="center")
    pos = 0
    for m in REF_RE.finditer(body):
        if m.start() < pos:
            continue
        for word in body[pos:m.start()].split():
            text(w, word, size, TEXT)
        ref, kind = m.group(0), ref_kind(m)
        end = m.end()
        tail = ""
        while end < len(body) and not body[end].isspace():
            tail += body[end]
            end += 1
        color = ref_color(ref, kind)
        holder = row(w, f"ref {ref}", gap=0)
        filled = kind in ("agent", "user", "branch", "item", "lane")
        chip = row(holder, "chip", padding=[0, 4, 0, 4], cornerRadius=3,
                   **({"fill": color + "22"} if filled else {}))
        text(chip, ref, size - 1, color, font=BUF,
             **({} if filled else {"textDecoration": "underline"}))
        if tail:
            text(holder, tail, size, TEXT)
        dest = ref_target(ref, kind)
        if dest:
            link(chip, dest)
        pos = end
    for word in body[pos:].split():
        text(w, word, size, TEXT)
    return w


def put_refs(g, r, c, ln, dim):
    if dim:
        return g.put(r, c, ln, DIM)
    pos = 0
    for m in REF_RE.finditer(ln):
        if m.start() > pos:
            c = g.put(r, c, ln[pos:m.start()], TEXT)
        ref, kind = m.group(0), ref_kind(m)
        g.refs.append((r, c, ref, kind))
        c = g.put(r, c, ref, ref_color(ref, kind), bold=kind in ("agent", "user"))
        pos = m.end()
    return g.put(r, c, ln[pos:], TEXT)


def cmd(command, params):
    steps.append({"command": command, "params": params})
    return {"$step": len(steps) - 1}


def frame(parent, name, w=None, h=None, x=0, y=0, **props):
    if "fill" not in props and "fills" not in props:
        props["fills"] = []
    p = {"name": name, "x": x, "y": y, "props": props}
    if parent is not None:
        p["parent"] = parent
    if w is not None:
        p["width"] = w
    if h is not None:
        p["height"] = h
    return cmd("frame.create", p)


def row(parent, name, gap=8, **props):
    props.setdefault("layout", "horizontal")
    props.setdefault("crossAlign", "center")
    props.setdefault("sizingH", "hug")
    props.setdefault("sizingV", "hug")
    return frame(parent, name, gap=gap, **props)


def col(parent, name, gap=8, **props):
    props.setdefault("layout", "vertical")
    props.setdefault("sizingH", "hug")
    props.setdefault("sizingV", "hug")
    return frame(parent, name, gap=gap, **props)


def fixed(ref, w=None, h=None):
    p = {"id": ref}
    if w is not None:
        p["sizingH"] = "fixed"
        p["width"] = w
    if h is not None:
        p["sizingV"] = "fixed"
        p["height"] = h
    cmd("node.set", p)


def text(parent, s, size=13, color=TEXT, font=UI, style="Regular", width=None, x=0, y=0,
         **props):
    pr = {"fontFamily": font, "fontStyle": style, "fontSize": size, "fill": color}
    pr.update(props)
    p = {"text": s, "x": x, "y": y, "parent": parent, "props": pr}
    if width is not None:
        p["width"] = width
        pr.setdefault("textAutoResize", "height")
    return cmd("text.create", p)


def wrap(parent, s, size=13, color=TEXT, **kw):
    return text(parent, s, size, color, width=100, sizingH="fill", **kw)


def rect(parent, x, y, w, h, fill, **props):
    return cmd("node.create", {"type": "rectangle", "parent": parent, "x": x, "y": y,
                               "width": w, "height": h, "props": {"fill": fill, **props}})


def dot(parent, color, d=8):
    return cmd("node.create", {"type": "ellipse", "parent": parent, "width": d, "height": d,
                               "props": {"fill": color}})


def rule(side, color=BORDER2):
    w = {"Top": 0, "Right": 0, "Bottom": 0, "Left": 0}
    w[side] = 1
    return {"stroke": color, **{f"stroke{k}Weight": v for k, v in w.items()}}


def link(src, dest, key=None):
    links.append((src, dest, key))


def wrap_words(s, width):
    out, line = [], ""
    for w in s.split(" "):
        if line and len(line) + 1 + len(w) > width:
            out.append(line)
            line = w
        else:
            line = f"{line} {w}" if line else w
    if line:
        out.append(line)
    return out


# ---------------------------------------------------------------- character grid

class Grid:
    def __init__(self, cols, rows, fg=TEXT):
        self.cols, self.rows, self.fg = cols, rows, fg
        self.cells = [[(" ", fg, None, False) for _ in range(cols)] for _ in range(rows)]
        self.vlines = []
        self.refs = []

    def vline(self, c, r0, r1, color=SEL):
        self.vlines.append((c, r0, r1, color))

    def put(self, r, c, s, fg=None, bg=None, bold=False):
        if not 0 <= r < self.rows:
            return c + len(s)
        for i, ch in enumerate(s):
            if 0 <= c + i < self.cols:
                old = self.cells[r][c + i]
                self.cells[r][c + i] = (ch, fg or self.fg, bg if bg else old[2], bold)
        return c + len(s)

    def bg(self, r, c0, c1, color):
        for c in range(max(c0, 0), min(c1, self.cols)):
            ch, fg, _, b = self.cells[r][c]
            self.cells[r][c] = (ch, fg, color, b)

    def rput(self, r, c_end, s, *args, **kw):
        return self.put(r, c_end - len(s), s, *args, **kw)

    def render(self, parent, x0, y0):
        for c, r0, r1, color in self.vlines:
            rect(parent, round(x0 + c * CW + CW / 2, 2), y0 + r0 * LH, 1, (r1 - r0) * LH, color)
        for r, cells in enumerate(self.cells):
            c = 0
            while c < self.cols:
                color = cells[c][2]
                if color is None:
                    c += 1
                    continue
                e = c
                while e < self.cols and cells[e][2] == color:
                    e += 1
                rect(parent, round(x0 + c * CW, 2), y0 + r * LH, round((e - c) * CW, 2), LH,
                     color)
                c = e
            c = 0
            while c < self.cols:
                ch, fg, _, b = cells[c]
                if ch == " ":
                    c += 1
                    continue
                e = c
                while e < self.cols:
                    ch2, fg2, _, b2 = cells[e]
                    if fg2 != fg or b2 != b:
                        break
                    if ch2 == " " and (e + 1 >= self.cols or cells[e + 1][0] == " "):
                        break
                    e += 1
                s = "".join(x[0] for x in cells[c:e]).rstrip()
                text(parent, s, 13, fg, font=BUF, style="Bold" if b else "Regular",
                     x=round(x0 + c * CW, 2), y=y0 + r * LH, lineHeight=LH)
                c = e

    def hot(self, parent, x0, y0, r, c, w, h=1):
        return rect(parent, round(x0 + c * CW, 2), y0 + r * LH, round(w * CW, 2), h * LH,
                    "#00000001")


# ---------------------------------------------------------------- terminal screens

TCOLS, TROWS = 184, 47
TX, TY = 19, 36


def term_window(name, title):
    x, y = POS[name]
    s = frame(None, name, 1440, 900, x, y, fill=BG, clip=True)
    screens[name] = s
    bar = frame(s, "titlebar", 1440, 28, 0, 0, fill=SURF)
    for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
        cmd("node.create", {"type": "ellipse", "parent": bar, "x": 12 + i * 20, "y": 8,
                            "width": 12, "height": 12, "props": {"fill": c}})
    text(bar, title, 12, FAINT, x=620, y=6)
    return s


def tabline(g, active, alert=False):
    tabs = ["#chat", "andy/fx1", "reviewer.xml"]
    g.bg(0, 0, TCOLS, SEL)
    c = 0
    for i, t in enumerate(tabs, 1):
        label = f" {i} {t} " + ("!1 " if alert and t == "#chat" else "")
        if t == active:
            c = g.put(0, c, label, TEXT, SURF, bold=True)
        else:
            c = g.put(0, c, label, FAINT, SEL)
    g.rput(0, TCOLS, " agmen · pr-3331 ", BG, ACCENT, bold=True)


def agents_pane(g, r0, cursor=None):
    g.put(r0, 1, "AGENTS", ACCENT, bold=True)
    g.rput(r0, 39, "11", FAINT)
    r = r0 + 2
    rows = {}
    for person, items in TREE:
        g.put(r, 1, "▾ " + person, TEXT, bold=True)
        r += 1
        for indent, handle, role, harness, status in items:
            glyph, scol = STATUS[status]
            prefix = {0: "  ", 1: "  ├ ", 2: "  └ "}[indent]
            c = g.put(r, 1, prefix, LINE)
            c = g.put(r, c, glyph + " ", scol)
            g.put(r, c, handle, TEXT, bold=handle == cursor)
            name, rcol = ROLE[role]
            g.put(r, 17, name, rcol)
            g.put(r, 30, harness, FAINT)
            if handle == cursor and person.startswith("andy"):
                g.bg(r, 0, 40, SEL)
            rows[(person, handle)] = r
            r += 1
        r += 1
    g.put(40, 1, "⏎", ACCENT, bold=True)
    g.put(40, 3, "open", FAINT)
    g.put(40, 9, "o", ACCENT, bold=True)
    g.put(40, 11, "take over", FAINT)
    g.put(40, 22, "s", ACCENT, bold=True)
    g.put(40, 24, "spawn", FAINT)
    g.put(41, 1, ":", ACCENT, bold=True)
    g.put(41, 3, "command", FAINT)
    g.put(41, 12, "/", ACCENT, bold=True)
    g.put(41, 14, "search", FAINT)
    g.put(41, 22, "?", ACCENT, bold=True)
    g.put(41, 24, "keys", FAINT)
    g.put(42, 1, "f", ACCENT, bold=True)
    g.put(42, 3, "open a reference", FAINT)
    g.vline(40, 1, 45)
    return rows


def chat_pane(g, r0, c0, width, msgs=None):
    msgs = msgs or CHAT
    r = r0
    for time, handle, role, verb, body, kind in msgs:
        dim = kind == "ledger"
        if width >= 120:
            lines = wrap_words(body, width - 46)
            if kind in ("mention", "alert"):
                hl = "#dda75218" if kind == "mention" else ERR + "38"
                for k in range(len(lines)):
                    g.bg(r + k, c0, c0 + width, hl)
                g.put(r, c0, "▌", WARN if kind == "mention" else ERR)
            g.put(r, c0 + 1, "  ↳" if kind == "reply" else time, LINE if kind == "reply" else FAINT)
            g.put(r, c0 + 8, handle, DIM if dim else TEXT, bold=not dim)
            if role:
                name, rcol = ROLE[role]
                g.put(r, c0 + 21, name, rcol)
            elif not dim:
                g.put(r, c0 + 21, "human", FAINT)
            g.put(r, c0 + 33, verb, VERB_COLOR.get(verb, TEXT), bold=verb.isupper() and not dim)
            for k, ln in enumerate(lines):
                put_refs(g, r + k, c0 + 45, ln, dim)
            r += len(lines) + (0 if kind == "reply" or msgs.index((time, handle, role, verb, body,
                                                                    kind)) == 3 else 1)
        else:
            if kind == "mention":
                g.put(r, c0, "▌", WARN)
            c = g.put(r, c0 + 1, ("↳ " if kind == "reply" else "") + time + " ", FAINT)
            c = g.put(r, c, handle + " ", DIM if dim else TEXT, bold=not dim)
            if role:
                name, rcol = ROLE[role]
                c = g.put(r, c, name + " ", rcol)
            g.put(r, c, verb, VERB_COLOR.get(verb, TEXT), bold=verb.isupper() and not dim)
            lines = wrap_words(body, width - 4)
            for k, ln in enumerate(lines):
                put_refs(g, r + 1 + k, c0 + 3, ln, dim)
                if kind == "mention":
                    g.bg(r + 1 + k, c0, c0 + width, "#dda75218")
            r += len(lines) + 2
    return r


def composer_line(g, c0, width, typed=None):
    g.put(42, c0, "─" * width, SEL)
    c = g.put(43, c0 + 1, "› andy/listen ", ACCENT, bold=True)
    if typed is None:
        g.put(43, c, " i write · @ mention · ⏎ send", FAINT)


def statusline(g, c0, c1, mode, buf, info, right=None, active=True):
    g.bg(45, c0, c1, SURF)
    c = c0
    if active and mode:
        mcol = {"NORMAL": ACCENT, "INSERT": OK, "COMMAND": WARN, "HINTS": WARN,
                "TERMINAL": OK}[mode]
        c = g.put(45, c, f" {mode} ", BG, mcol, bold=True)
    c = g.put(45, c, f" {buf} ", TEXT if active else FAINT, SEL)
    g.put(45, c + 1, info, TEXT if active else FAINT, SURF)
    if right:
        g.rput(45, c1, right, FAINT, SURF)


def build_t_chat(name, command=None, alert=False, hints=False):
    s = term_window(name, "agmen · pr-3331 · 184×47")
    g = Grid(TCOLS, TROWS)
    tabline(g, "#chat", alert)
    rows = agents_pane(g, 2)
    msgs = CHAT + ([ALERT_MSG] if alert else [])
    used = chat_pane(Grid(TCOLS, 200), 0, 41, TCOLS - 41, msgs)
    g.refs = []
    chat_pane(g, 41 - used, 41, TCOLS - 41, msgs)
    fx1_label = None
    if hints:
        letters = "asdfghjklqwertyuiopzxcvbnm"
        for i, (rr, cc, ref, kind) in enumerate(g.refs[:len(letters)]):
            g.put(rr, cc - 1, letters[i], BG, WARN, bold=True)
            if ref == "andy/fx1" and fx1_label is None:
                fx1_label = (letters[i], rr, cc - 1)
    composer_line(g, 41, TCOLS - 41)
    statusline(g, 0, 40, None, "agents", "", active=False)
    g.put(45, 40, " ", SURF, SURF)
    if hints:
        statusline(g, 41, TCOLS, "HINTS", "#chat", "type a label to open it · q cancels",
                   " agents · people · branches · commits · files · items · lanes · PRs ")
        g.put(46, 0, "-- HINTS -- each yellow letter opens the reference after it", FAINT)
    elif command is None:
        statusline(g, 41, TCOLS, "NORMAL", "#chat", "9 msgs · 1 mentions you",
                   " listener codex ● idle │ andy ")
        if alert:
            g.rput(45, TCOLS, " ! 1 NEEDS YOU ", BG, ERR, bold=True)
            c = g.put(46, 0, "ALERT ", ERR, bold=True)
            g.put(46, c, "adam/coord needs you · terminal bell rang · gn jumps to it · r replies",
                  TEXT)
        else:
            g.put(46, 0, '"#chat" 9 messages, 1 mentions you', FAINT)
        g.rput(46, TCOLS, "? keys ", FAINT)
    else:
        g.bg(45, 0, TCOLS, SURF)
        c = 1
        for h in ["claude", "codex", "opencode", "gemini", "agy", "grok"]:
            if h == "codex":
                c = g.put(45, c, f" {h} ", BG, ACCENT, bold=True)
            else:
                c = g.put(45, c, f" {h} ", TEXT, SURF)
            c += 1
        g.rput(45, TCOLS, " Tab next harness · ⏎ spawn · role from --role, tier from the role ",
               FAINT, SURF)
        c = g.put(46, 0, command, TEXT)
        g.put(46, c, " ", BG, TEXT)
    g.render(s, TX, TY)
    hot_fx1 = g.hot(s, TX, TY, rows[("andy (you)", "fx1")], 0, 40)
    hot_cmd = g.hot(s, TX, TY, 41, 0, 12)
    hot_spawn = g.hot(s, TX, TY, 40, 21, 9)
    if alert:
        link(s, "Terminal · chat", "Q")
        return
    if hints:
        link(s, "Terminal · chat", "Q")
        if fx1_label:
            letter, rr, cc = fx1_label
            link(s, "Terminal · split", letter.upper())
            link(g.hot(s, TX, TY, rr, cc, 10), "Terminal · split")
        return
    if command is None:
        link(hot_fx1, "Terminal · split")
        link(hot_cmd, "Terminal · command")
        link(hot_spawn, "Terminal · command")
        link(s, "Terminal · split", "Enter")
        link(s, "Terminal · command", "S")
        link(s, "Terminal · references", "F")
        link(g.hot(s, TX, TY, 42, 0, 20), "Terminal · references")
    else:
        hot_enter = g.hot(s, TX, TY, 46, 0, len(command) + 2)
        link(hot_enter, "Terminal · chat")
        link(s, "Terminal · chat", "Enter")
        link(s, "Terminal · chat", "Q")


def session_lines(g, r, c0, width):
    for kind, a, b in SESSION:
        if kind == "tool":
            c = g.put(r, c0 + 1, "● ", ACCENT)
            c = g.put(r, c, a + " ", TEXT, bold=True)
            g.put(r, c, b[:width - (c - c0) - 1], FAINT)
            r += 1
        elif kind == "out":
            g.put(r, c0 + 3, "⎿ " + a, b)
            r += 2
        elif kind == "say":
            for ln in wrap_words(a, width - 3):
                g.put(r, c0 + 1, ln, TEXT)
                r += 1
            r += 1
        elif kind == "cost":
            g.put(r, c0 + 1, a, DIM)
            r += 2
    return r


def build_t_split(name):
    s = term_window(name, "agmen · pr-3331 · 184×47")
    g = Grid(TCOLS, TROWS)
    tabline(g, "#chat")
    agents_pane(g, 2, cursor="fx1")
    chat_pane(g, 2, 41, 70)
    g.put(42, 41, "─" * 70, SEL)
    g.put(43, 42, "› andy/listen", ACCENT, bold=True)
    g.vline(112, 1, 45)
    c0, w = 113, TCOLS - 113
    c = g.put(2, c0 + 1, "andy/fx1 ", ORG, bold=True)
    g.put(2, c, "Engineer · claude · sonnet", FAINT)
    g.put(3, c0 + 1, "C015 · fix/c015-main-conflict · $1.20/5.00 · 1h12m · 454k tok", FAINT)
    g.put(4, c0, "─" * w, SEL)
    session_lines(g, 6, c0, w)
    g.put(42, c0, "─" * w, SEL)
    c = c0 + 1
    for k, label in [("o", "take over"), ("O", "other agent"), ("m", "message"),
                     ("s", "stop"), ("b", "budget")]:
        c = g.put(43, c, k, ACCENT, bold=True)
        c = g.put(43, c + 1, label, FAINT) + 2
    statusline(g, 0, 40, None, "agents", "", active=False)
    statusline(g, 41, 112, None, "#chat", "9 msgs", active=False)
    g.put(45, 40, " ", SURF, SURF)
    g.put(45, 112, " ", SURF, SURF)
    statusline(g, 113, TCOLS, "NORMAL", "andy/fx1", "live ● working",
               " $1.20/5.00 · 1h12m · 454k tok ")
    g.put(46, 0, "-- andy/fx1 -- o opens Claude Code on this session in this split; q hands it "
                 "back", FAINT)
    g.render(s, TX, TY)
    hot_o = g.hot(s, TX, TY, 43, c0, 22)
    link(hot_o, "Terminal · take-over")
    hot_back = g.hot(s, TX, TY, 2, 41, 70, 38)
    link(hot_back, "Terminal · chat")
    link(s, "Terminal · take-over", "O")
    link(s, "Terminal · chat", "Q")


def claude_screen(g, r0, cols, rows, banner=True, compact=False):
    r = r0
    if banner:
        g.bg(r, 0, cols, SEL)
        g.put(r, 1, "agmen suspended · andy/fx1 · quit Claude Code to return", TEXT, SEL)
        r += 2
    g.put(r, 1, "$ claude --resume 3f9c2e1a-7b40-4c1e-9d6a-2f0e8c5b1a77", FAINT)
    r += 2
    g.put(r, 1, "● Resumed session · ~/agmen/worktrees/pr-3331/fix-c015-main-conflict", ORG)
    r += 2
    for kind, a, b in ([] if compact else SESSION[4:9]):
        if kind == "tool":
            c = g.put(r, 1, "● ", ACCENT)
            c = g.put(r, c, a + " ", TEXT, bold=True)
            g.put(r, c, b, FAINT)
            r += 1
        elif kind == "out":
            g.put(r, 3, "⎿ " + a, b)
            r += 2
    g.put(r, 1, "> why did you drop the old tabs import?", BRIGHT)
    r += 2
    for ln in wrap_words("● Sean's 016f235 moved the tabs module to src/ui/tabs.rs, so the old "
                         "`mod tabs;` line points at a file that no longer exists. The new import "
                         "is on line 7.", cols - 4):
        g.put(r, 1, ln, TEXT)
        r += 1
    bottom = r0 + rows - 4
    g.put(bottom, 0, "╭" + "─" * (cols - 2) + "╮", LINE)
    g.put(bottom + 1, 0, "│", LINE)
    g.put(bottom + 1, 2, ">", TEXT, bold=True)
    g.put(bottom + 1, 4, " ", BG, TEXT)
    g.put(bottom + 1, cols - 1, "│", LINE)
    g.put(bottom + 2, 0, "╰" + "─" * (cols - 2) + "╯", LINE)
    g.put(bottom + 3, 2, "? for shortcuts", FAINT)
    g.rput(bottom + 3, cols - 1, "messages here stay on this machine", FAINT)


def build_t_takeover(name):
    s = term_window(name, "agmen · pr-3331 · 184×47")
    g = Grid(TCOLS, TROWS)
    tabline(g, "#chat")
    agents_pane(g, 2, cursor="fx1")
    chat_pane(g, 2, 41, 70)
    g.put(42, 41, "─" * 70, SEL)
    g.put(43, 42, "› andy/listen", ACCENT, bold=True)
    g.vline(112, 1, 45)
    c0, w = 113, TCOLS - 113
    g.bg(1, c0, TCOLS, SURF)
    c = g.put(1, c0 + 1, ">_ ", ACCENT, SURF)
    c = g.put(1, c, "claude --resume 3f9c2e1a  ", TEXT, SURF)
    c = g.put(1, c, "andy/fx1  ", ORG, SURF, bold=True)
    g.put(1, c, "live · stays on this machine", ACCENT, SURF)
    statusline(g, 0, 40, None, "agents", "", active=False)
    statusline(g, 41, 112, None, "#chat", "9 msgs", active=False)
    g.put(45, 40, " ", SURF, SURF)
    g.put(45, 112, " ", SURF, SURF)
    statusline(g, 113, TCOLS, "TERMINAL", "claude · andy/fx1", "headless run paused",
               " q hands back ")
    g.put(46, 0, "-- TERMINAL -- keys go to Claude Code · Ctrl-\\ Ctrl-n returns to normal mode · "
                 "q then hands the session back to agmen", FAINT)
    g.render(s, TX, TY)
    sub = Grid(w, 42)
    claude_screen(sub, 0, w, 42, banner=False)
    sub.render(s, round(TX + c0 * CW, 2), TY + 2 * LH)
    link(g.hot(s, TX, TY, 45, c0, w), "Terminal · split")
    link(s, "Terminal · split", "Q")


# ---------------------------------------------------------------- desktop (Zed style)

def zwindow(name, project="pr-3331"):
    x, y = POS[name]
    s = frame(None, name, 1440, 900, x, y, fill=BG, layout="vertical", clip=True)
    screens[name] = s
    tb = row(s, "titlebar", gap=10, padding=[0, 12, 0, 12], sizingH="fill", **rule("Bottom"))
    fixed(tb, h=38)
    for c in ["#ff5f57", "#febc2e", "#28c840"]:
        dot(tb, c, 12)
    frame(tb, "gap", 8, 4)
    text(tb, "agmen", 13, TEXT, style="Semibold")
    text(tb, "›", 13, DIM)
    text(tb, project, 13, TEXT)
    pill = row(tb, "listener", gap=6, padding=[3, 8, 3, 8], cornerRadius=5, fill=SURF)
    dot(pill, FAINT, 7)
    text(pill, "listener: codex · idle", 12, FAINT)
    frame(tb, "spacer", 10, 4, sizingH="fill")
    av = frame(tb, "avatar", 22, 22, fill=SEL, cornerRadius=11, layout="horizontal",
               mainAlign="center", crossAlign="center")
    text(av, "A", 11, TEXT, style="Semibold")
    body = row(s, "body", gap=0, sizingH="fill", sizingV="fill", crossAlign="min")
    return s, body



# ---------------------------------------------------------------- icons
# UI icons: Zed's assets/icons SVGs from a folder named by ZED_ICONS (not bundled here).
# File icons: the Zed icon theme installed on this machine (MATERIAL_ICON_THEME, default the
# Material Icon Theme extension), read through its own mapping file.

OUT_PATH = pathlib.Path(sys.argv[1]).resolve()
ICON_CACHE = OUT_PATH.parent / "icon-cache"
ZED_ICONS = pathlib.Path(os.environ["ZED_ICONS"]) if os.environ.get("ZED_ICONS") else None
MAT_ROOT = pathlib.Path(os.environ.get(
    "MATERIAL_ICON_THEME",
    str(pathlib.Path.home() / "Library/Application Support/Zed/extensions/installed/"
                               "material-icon-theme")))
try:
    MAT = json.loads((MAT_ROOT / "icon_themes/material-icon-theme.json").read_text())["themes"][0]
except (OSError, ValueError, KeyError, IndexError):
    MAT = None


def place(parent, path, size, native):
    ref = cmd("file.import", {"path": str(path), "x": -2000, "y": -2000})
    cmd("node.reparent", {"ids": [ref], "parent": parent})
    if abs(native - size) > 0.01:
        cmd("transform.scale", {"ids": [ref], "factor": round(size / native, 4)})
    return ref


def zicon(parent, name, color, size=14):
    src = ZED_ICONS / f"{name}.svg" if ZED_ICONS else None
    if not src or not src.exists():
        return text(parent, "·", size, color)
    svg = re.sub(r'(stroke|fill)="(?!none)[^"]*"', lambda m: f'{m.group(1)}="{color}"',
                 src.read_text())
    ICON_CACHE.mkdir(exist_ok=True)
    out = ICON_CACHE / f"{name}-{color.lstrip('#')}.svg"
    out.write_text(svg)
    return place(parent, out, size, 16)


def mat_icon_path(name, is_dir=False, expanded=False):
    if not MAT:
        return None
    if is_dir:
        d = MAT["named_directory_icons"].get(name) or MAT["directory_icons"]
        rel = d["expanded" if expanded else "collapsed"]
    else:
        key = MAT["file_stems"].get(name)
        parts = name.split(".")
        for i in range(1, len(parts)):
            if key:
                break
            key = MAT["file_suffixes"].get(".".join(parts[i:]))
        entry = MAT["file_icons"].get(key) if key else None
        rel = entry["path"] if entry else "./icons/file.svg"
    path = MAT_ROOT / rel.removeprefix("./")
    return path if path.exists() else None


def ficon(parent, name, is_dir=False, expanded=False, size=16):
    path = mat_icon_path(name, is_dir, expanded)
    if not path:
        return text(parent, "▪", 10, FAINT)
    m = re.search(r'viewBox="[\d.-]+\s+[\d.-]+\s+([\d.]+)\s+[\d.]+"', path.read_text()[:600])
    return place(parent, path, size, float(m.group(1)) if m else 32.0)


def icon_spec(parent, spec, color, size=14):
    if spec.startswith("z:"):
        return zicon(parent, spec[2:], color, size)
    if spec.startswith("f:"):
        return ficon(parent, spec[2:], size=size)
    return text(parent, spec, 12, color, font=BUF)


TAB_ICONS = {"Agents": "user_group", "Project": "file_tree", "Git": "git_branch",
             "Roles": "person"}

REFS = {}


def statusbar(s, item_right="listener codex · idle", dock_toggle=False, alert=False):
    sb = row(s, "status bar", gap=14, padding=[0, 10, 0, 10], sizingH="fill", **rule("Top"))
    fixed(sb, h=28)
    icons = row(sb, "panel buttons", gap=10)
    for name in ["file_tree", "git_branch", "magnifying_glass"]:
        zicon(icons, name, FAINT, 14)
    if alert:
        nb = row(sb, "needs you", gap=5, padding=[1, 7, 1, 7], cornerRadius=4, fill=ERR)
        text(nb, "!", 12, BG, style="Bold")
        text(nb, "1 needs you", 12, BG, style="Semibold")
    for glyph, label, color in [("●", "9 online", OK), ("⚠", "2 alerts", WARN),
                                ("⎇", "4 worktrees", FAINT)]:
        r = row(sb, label, gap=5)
        text(r, glyph, 12, color)
        text(r, label, 12, FAINT)
        REFS[label] = r
    frame(sb, "spacer", 10, 4, sizingH="fill")
    text(sb, item_right, 12, FAINT)
    text(sb, "$14.20 today", 12, FAINT)
    if dock_toggle:
        t = row(sb, "listener toggle", gap=5, padding=[1, 7, 1, 7], cornerRadius=4, fill=SEL)
        text(t, "◨", 11, ACCENT)
        text(t, "Listener", 12, TEXT)
        REFS["toggle"] = t
    rb = row(sb, "right buttons", gap=10)
    for name in ["terminal", "chat", "bell"]:
        zicon(rb, name, FAINT, 14)
    gear = row(rb, "settings", gap=0)
    zicon(gear, "settings", FAINT, 14)
    link(gear, "Desktop · settings")
    pal = text(sb, "⌘⇧P", 12, ACCENT, font=BUF)
    return pal


DOCK_DEST = {"Agents": "Desktop · chat", "Project": "Desktop · project", "Git": "Desktop · git",
             "Roles": "Desktop · role file"}

PROJECT = [
    (0, "dir", "fix-c015-main-conflict", None, True), (1, "dir", ".github", None, False),
    (1, "dir", "src", None, True), (2, "file", "main.rs", "M", None),
    (2, "dir", "ui", None, True), (3, "file", "mod.rs", None, None),
    (3, "file", "tabs.rs", None, None), (2, "file", "cylinder.rs", None, None),
    (2, "file", "document.rs", None, None), (2, "file", "minimap.rs", None, None),
    (1, "dir", "tests", None, False), (1, "file", "Cargo.toml", None, None),
    (1, "file", "Cargo.lock", "M", None), (1, "file", "README.md", None, None),
]
TESTS = [(2, "file", "it_main.rs", None, None), (2, "file", "it_tabs.rs", None, None)]


def tree_row(tree, depth, label, h=24, on=False):
    r = row(tree, label, gap=6, padding=[0, 10, 0, 10 + depth * 14], sizingH="fill",
            **({"fill": SEL} if on else {}))
    fixed(r, h=h)
    return r


def scope_chip(tree):
    w = row(tree, "scope", padding=[2, 10, 8, 10], sizingH="fill")
    c = row(w, "chip", gap=6, padding=[4, 8, 4, 8], cornerRadius=5, fill=SURF, sizingH="fill")
    text(c, "●", 9, ORG)
    text(c, "fx1 · fix/c015-main-conflict", 12, TEXT, font=BUF)
    frame(c, "spacer", 4, 4, sizingH="fill")
    text(c, "▾", 10, FAINT)


def project_tree(tree, tests_open):
    scope_chip(tree)
    refs = {}
    for depth, kind, name, status, is_open in PROJECT:
        if name == "tests":
            is_open = tests_open
        r = tree_row(tree, depth, name, on=name == "main.rs")
        if kind == "dir":
            zicon(r, "chevron_down" if is_open else "chevron_right", DIM, 12)
            ficon(r, name, is_dir=True, expanded=is_open)
            text(r, name, 13, TEXT, font=BUF)
        else:
            frame(r, "indent", 12, 4)
            ficon(r, name)
            text(r, name, 13, (WARN if status == "M" else TEXT), font=BUF)
        if status:
            frame(r, "spacer", 4, 4, sizingH="fill")
            text(r, status, 12, WARN if status == "M" else OK, font=BUF, style="Bold")
        refs[name] = r
        if name == "tests" and tests_open:
            for d2, k2, n2, s2, _ in TESTS:
                r2 = tree_row(tree, d2, n2)
                frame(r2, "indent", 12, 4)
                ficon(r2, n2)
                text(r2, n2, 13, TEXT, font=BUF)
    return refs


def git_section(tree, label, count, is_open):
    r = row(tree, label, gap=6, padding=[0, 10, 0, 10], sizingH="fill")
    fixed(r, h=26)
    zicon(r, "chevron_down" if is_open else "chevron_right", DIM, 12)
    text(r, label, 11, DIM, style="Semibold", textCase="upper", letterSpacing="6%")
    c = row(r, "count", padding=[0, 6, 0, 6], cornerRadius=4, fill=SURF)
    text(c, count, 11, FAINT, font=BUF)
    return r


def git_tree(tree, changes_open):
    scope_chip(tree)
    refs = {"Changes": git_section(tree, "Changes", "2", changes_open)}
    if changes_open:
        for st, path, delta in [("M", "src/main.rs", "+2 −5"), ("M", "Cargo.lock", "+1 −1")]:
            r = tree_row(tree, 1, path, on=path == "src/main.rs")
            text(r, st, 12, WARN, font=BUF, style="Bold")
            ficon(r, path.rsplit("/", 1)[-1])
            text(r, path, 13, TEXT, font=BUF)
            frame(r, "spacer", 4, 4, sizingH="fill")
            text(r, delta, 11, FAINT, font=BUF)
    git_section(tree, "Commits", "fix/c015", True)
    for sha, msg, who in [("9e2a71c", "Resolve mod list", "fx1"),
                          ("4d9fdc8", "Merge sean/tabs", "sean/a1"),
                          ("016f235", "Move tabs to src/ui", "sean/a1")]:
        r = tree_row(tree, 1, sha)
        text(r, sha, 12, ACCENT, font=BUF)
        text(r, msg, 12, TEXT)
        frame(r, "spacer", 4, 4, sizingH="fill")
        text(r, who, 11, FAINT, font=BUF)
    git_section(tree, "Worktrees", "5", True)
    r = tree_row(tree, 1, "andy")
    zicon(r, "chevron_down", DIM, 12)
    text(r, "andy", 13, TEXT, style="Medium")
    for br, who, state, color in [("fix/c015-main-conflict", "fx1", "↑1 pushed", OK),
                                  ("test/b-415-replay-paging", "t7k", "↑1 local", WARN),
                                  ("detached @2b5aa0d", "rv2", "reading", ACCENT)]:
        r = tree_row(tree, 2, br)
        text(r, "●", 8, color)
        text(r, br, 12, TEXT, font=BUF)
        frame(r, "spacer", 4, 4, sizingH="fill")
        text(r, who, 11, FAINT, font=BUF)
    r = tree_row(tree, 1, "sean")
    zicon(r, "chevron_right", DIM, 12)
    text(r, "sean", 13, TEXT, style="Medium")
    text(r, "2", 11, FAINT, font=BUF)
    git_section(tree, "Branches", "14", False)
    return refs


def left_dock(body, tab="Agents", cursor=None, fx1="claude", variant=None):
    d = col(body, "left dock", gap=0, sizingV="fill", **rule("Right"))
    fixed(d, w=272)
    tabs = row(d, "dock tabs", gap=10, padding=[0, 10, 0, 10], sizingH="fill", **rule("Bottom"))
    fixed(tabs, h=34)
    refs = {}
    for t in ["Agents", "Project", "Git", "Roles"]:
        tr = row(tabs, f"tab {t}", gap=5)
        zicon(tr, TAB_ICONS[t], TEXT if t == tab else FAINT, 14)
        text(tr, t, 12, TEXT if t == tab else FAINT, style="Semibold" if t == tab else "Regular")
        refs[t] = tr
        if t != tab:
            link(tr, DOCK_DEST[t])
    frame(tabs, "spacer", 4, 4, sizingH="fill")
    hide = text(tabs, "⟨", 13, DIM)
    refs["hide"] = hide
    tree = col(d, "tree", gap=0, padding=[6, 0, 6, 0], sizingH="fill")
    if tab == "Project":
        refs.update(project_tree(tree, variant == "tests"))
    elif tab == "Git":
        refs.update(git_tree(tree, variant != "collapsed"))
    elif tab == "Agents":
        for person, items in TREE:
            r = row(tree, person, gap=6, padding=[0, 10, 0, 10], sizingH="fill")
            fixed(r, h=26)
            zicon(r, "chevron_down", DIM, 12)
            text(r, person, 13, TEXT, style="Medium")
            for indent, handle, role, harness, status in items:
                on = cursor == handle and person.startswith("andy")
                r = row(tree, handle, gap=7, padding=[0, 10, 0, 26 + (14 if indent else 0)],
                        sizingH="fill", **({"fill": SEL} if on else {}))
                fixed(r, h=26)
                name, rcol = ROLE[role]
                glyph, scol = STATUS[status]
                text(r, glyph, 10, scol)
                text(r, handle, 13, TEXT, font=BUF)
                text(r, name, 12, rcol)
                frame(r, "spacer", 4, 4, sizingH="fill")
                text(r, fx1 if (handle, person) == ("fx1", "andy (you)") else harness, 12, FAINT,
                     font=BUF)
                refs[(person, handle)] = r
    else:
        groups = [("Built-in", ["base", "listener", "coordinator", "planner", "researcher",
                                "engineer", "tester", "reviewer"]),
                  (".agmen/roles", ["security-reviewer"]), ("~/.config/agmen/roles",
                                                           ["docs-writer"])]
        for g, items in groups:
            r = row(tree, g, gap=6, padding=[0, 10, 0, 10], sizingH="fill")
            fixed(r, h=26)
            zicon(r, "chevron_down", DIM, 12)
            text(r, g, 13, TEXT, style="Medium", font=BUF if g.startswith((".", "~")) else UI)
            for k in items:
                on = k == "reviewer"
                r = row(tree, k, gap=7, padding=[0, 10, 0, 26], sizingH="fill",
                        **({"fill": SEL} if on else {}))
                fixed(r, h=26)
                color = ROLE[k][1] if k in ROLE else (ERR if k == "security-reviewer" else
                                                      ORG if k == "docs-writer" else DIM)
                ficon(r, f"{k}.xml", size=16)
                text(r, f"{k}.xml", 13, TEXT, font=BUF)
    return d, refs


def tab_bar(center, active, extra=None, badge=None):
    tb = row(center, "tab bar", gap=0, sizingH="fill", **rule("Bottom"))
    fixed(tb, h=34)
    refs = {}
    for label, icon, color in [("chat", "z:chat", ACCENT), ("andy/fx1", "z:sparkle", ORG),
                               ("reviewer.xml", "f:reviewer.xml", ERR)] + (
                                   [extra] if extra else []):
        on = label == active
        t = row(tb, f"tab {label}", gap=8, padding=[0, 14, 0, 14], sizingV="fill",
                **({"fill": SURF} if on else {}), **rule("Right"))
        icon_spec(t, icon, color, 14)
        text(t, label, 13, TEXT if on else FAINT, font=BUF if label not in ("chat", "settings")
             else UI)
        if on:
            text(t, "×", 13, DIM)
        if label == badge:
            bd = row(t, "badge", padding=[0, 5, 0, 5], cornerRadius=7, fill=ERR)
            text(bd, "1", 10, BG, style="Bold")
        refs[label] = t
    return refs


def breadcrumb(center, parts, buttons=()):
    b = row(center, "breadcrumb", gap=8, padding=[0, 14, 0, 14], sizingH="fill", **rule("Bottom"))
    fixed(b, h=32)
    for i, p in enumerate(parts):
        if i:
            text(b, "›", 12, DIM)
        text(b, p, 12, TEXT if i == len(parts) - 1 else FAINT, font=BUF)
    frame(b, "spacer", 4, 4, sizingH="fill")
    refs = {}
    for label, kind in buttons:
        bt = row(b, label, gap=6, padding=[3, 9, 3, 9], cornerRadius=5,
                 fill=ACCENT + "26" if kind == "primary" else SURF)
        text(bt, label, 12, ACCENT if kind == "primary" else TEXT)
        refs[label] = bt
    return refs


def zcomposer(center, to, hint, model=None):
    w = col(center, "composer", gap=0, padding=[8, 14, 12, 14], sizingH="fill")
    box = row(w, "box", gap=8, padding=[8, 10, 8, 10], cornerRadius=6, fill=SURF, stroke=BORDER,
              strokeWeight=1, sizingH="fill")
    if to:
        chip = row(box, "to", gap=5, padding=[2, 7, 2, 7], cornerRadius=4, fill=SEL)
        text(chip, "to", 11, FAINT)
        text(chip, to, 12, ACCENT, font=BUF)
        text(chip, "▾", 10, FAINT)
    mchip = None
    if model:
        mchip = row(box, "model", gap=5, padding=[2, 7, 2, 7], cornerRadius=4, fill=SEL,
                    stroke=ACCENT + "55", strokeWeight=1)
        text(mchip, "◆", 9, ACCENT)
        text(mchip, model, 12, TEXT, font=BUF)
        text(mchip, "▾", 10, FAINT)
    wrap(box, hint, 13, FAINT)
    text(box, "⏎", 13, DIM)
    return box, mchip


def zmessage(parent, time, handle, role, verb, body, kind, action=None, model=None):
    pad_left = 44 if kind == "reply" else 10
    extra = {}
    if kind == "mention":
        extra = dict(fill=WARN + "14", stroke=WARN, strokeTopWeight=0, strokeRightWeight=0,
                     strokeBottomWeight=0, strokeLeftWeight=2)
    if kind == "alert":
        extra = dict(fill=ERR + "1c", stroke=ERR, strokeTopWeight=0, strokeRightWeight=0,
                     strokeBottomWeight=0, strokeLeftWeight=3)
    if kind == "ledger":
        r = row(parent, "ledger", gap=10, padding=[2, 10, 2, 44], sizingH="fill")
        text(r, time, 12, DIM, font=BUF)
        text(r, f"{handle} {verb} {body}", 12, DIM, font=BUF)
        return r, None
    m = row(parent, f"msg {time} {handle}", gap=10, padding=[6, 10, 6, pad_left], sizingH="fill",
            crossAlign="min", **extra)
    color = ROLE[role][1] if role else FAINT
    av = frame(m, "avatar", 24, 24, fill=color + "33", cornerRadius=5 if role else 12,
               layout="horizontal", mainAlign="center", crossAlign="center")
    text(av, (ROLE[role][0] if role else handle)[0].upper(), 11, color if role else TEXT,
         style="Semibold")
    c = col(m, "content", gap=3, sizingH="fill")
    REFS["last_content"] = c
    h = row(c, "head", gap=8)
    name = text(h, handle, 13, BRIGHT, font=BUF, style="Bold")
    text(h, ROLE[role][0] if role else HUMAN_TAG.get(handle, "human"), 12, color)
    vc = VERB_COLOR.get(verb, TEXT)
    chip = row(h, "verb", padding=[1, 6, 1, 6], cornerRadius=4, fill=vc + "22")
    text(chip, verb, 11, vc, font=BUF)
    if kind == "alert":
        nb = row(h, "needs you", padding=[1, 6, 1, 6], cornerRadius=4, fill=ERR)
        text(nb, "NEEDS YOU", 10, BG, style="Bold")
    if model:
        mc = row(h, "model", gap=4, padding=[1, 6, 1, 6], cornerRadius=4, fill=SEL)
        text(mc, "◆", 8, ACCENT)
        text(mc, model, 11, FAINT, font=BUF)
    text(h, time, 11, DIM)
    act = None
    if action:
        act = row(h, "action", gap=5, padding=[1, 7, 1, 7], cornerRadius=4, fill=ACCENT + "22",
                  stroke=ACCENT + "55", strokeWeight=1)
        text(act, ">_", 10, ACCENT, font=BUF)
        text(act, action, 11, ACCENT)
    rich_body(c, body)
    if kind == "alert":
        br = row(c, "actions", gap=6)
        REFS["alert_actions"] = []
        for label, primary in [("Reply", True), ("Hotfix: spawn an engineer", False),
                               ("Revert #10", False), ("Snooze 15 min", False)]:
            b = row(br, label, padding=[3, 9, 3, 9], cornerRadius=5,
                    fill=ERR if primary else SURF, stroke=BORDER, strokeWeight=0 if primary else 1)
            text(b, label, 12, BG if primary else TEXT, style="Semibold" if primary else "Regular")
            REFS["alert_actions"].append(b)
    return name, act


def right_dock(body, title, sub):
    d = col(body, "right dock", gap=0, sizingV="fill", **rule("Left"))
    fixed(d, w=340)
    h = row(d, "head", gap=8, padding=[0, 14, 0, 14], sizingH="fill", **rule("Bottom"))
    fixed(h, h=34)
    text(h, title, 13, TEXT, style="Semibold")
    text(h, sub, 12, FAINT, font=BUF)
    frame(h, "spacer", 4, 4, sizingH="fill")
    btn = row(h, "collapse", padding=[2, 6, 2, 6], cornerRadius=4, fill=SURF)
    text(btn, "⟩", 13, FAINT)
    REFS["collapse"] = btn
    c = col(d, "content", gap=16, padding=[14, 14, 14, 14], sizingH="fill")
    return c


def section(parent, title, rows_):
    s = col(parent, title, gap=7, sizingH="fill")
    text(s, title, 11, DIM, style="Semibold", textCase="upper", letterSpacing="6%")
    for label, value, color in rows_:
        r = row(s, label, gap=10, sizingH="fill", crossAlign="min")
        text(r, label, 12, FAINT, width=96)
        wrap(r, value, 12, color, font=BUF)
    return s


def agent_dock(body, taken_over=False, harness="claude"):
    rd = right_dock(body, "Agent", "andy/fx1")
    section(rd, "Runs", [("Role", "Engineer (built-in)", ORG),
                         ("Harness", "Codex 0.164.0 · ACP, since 15:52" if harness == "codex"
                          else "Claude Code 2.1.310 · CLI adapter", TEXT),
                         ("Model", "Codex default (standard tier)" if harness == "codex"
                          else "sonnet (standard tier)", TEXT),
                         ("Delegated by", "andy/listen at 15:31", TEXT),
                         ("State", "taken over in chat; headless run paused" if taken_over
                          else "working", ACCENT if taken_over else OK)])
    section(rd, "Enforced", [("Tools", "sandbox workspace-write (Codex has no tool list)"
                              if harness == "codex" else "Read Edit Write Bash Grep Glob",
                              WARN if harness == "codex" else TEXT),
                             ("Edits", "lane L-CORE", TEXT),
                             ("Verbs", "CLAIM DONE PUSH_REQUEST FOUND chat", OK),
                             ("Refused", "ASSIGN RULING LEND REVIEW", ERR)])
    section(rd, "Worktree", [("Branch", "fix/c015-main-conflict", TEXT),
                             ("Base", "origin/main @4d9fdc8", TEXT),
                             ("State", "1 commit, pushed, clean", OK),
                             ("Cleanup", "removed with its branch after merge", FAINT)])
    bs = section(rd, "Budget", [("Spent", "$1.20 of $5.00", TEXT),
                                ("Wall clock", "1 h 12 min since launch · 41 min working", TEXT),
                                ("Tokens", "418k in · 36k out · 1.9M cache read", TEXT)])
    bar = frame(bs, "bar", 312, 4, fill=SEL, cornerRadius=2, sizingH="fill")
    frame(bar, "used", 75, 4, 0, 0, fill=ACCENT, cornerRadius=2)


INSTALLED = [("claude", "2.1.310", None), ("codex", "0.164.0", None),
             ("opencode", "2.1.4", None), ("gemini", "0.46.0", None),
             ("agy", "1.1.7", "update to 1.1.15+ for JSON"), ("grok", None, "not installed")]


def codex_screen(g, cols, rows):
    r = 0
    g.put(r, 1, "$ codex", FAINT)
    g.put(r, 9, "# started by agmen as andy/fx1 in ~/agmen/worktrees/pr-3331/fix-c015-main-conflict",
          DIM)
    r += 2
    g.put(r, 1, "agmen handoff brief", ACCENT, bold=True)
    r += 1
    for ln in ["item C015 · lane L-CORE · branch fix/c015-main-conflict, 1 commit, pushed",
               "done: conflict markers removed, cargo test 71 passed, DONE #1866, REVIEW pass",
               "next: when Sean merges #10, rebase if main moved and rerun cargo test",
               "earlier session: claude 3f9c2e1a, still resumable"]:
        g.put(r, 3, ln, TEXT)
        r += 1
    r += 1
    g.put(r, 1, "• main has not moved since 9e2a71c, so there is nothing to rebase yet.", TEXT)
    r += 1
    g.put(r, 1, "  Watching for the #10 merge.", TEXT)
    bottom = rows - 4
    g.put(bottom, 0, "╭" + "─" * (cols - 2) + "╮", LINE)
    g.put(bottom + 1, 0, "│", LINE)
    g.put(bottom + 1, 2, "›", TEXT, bold=True)
    g.put(bottom + 1, 4, " ", BG, TEXT)
    g.put(bottom + 1, cols - 1, "│", LINE)
    g.put(bottom + 2, 0, "╰" + "─" * (cols - 2) + "╯", LINE)
    g.rput(bottom + 3, cols - 1, "messages here stay on this machine", FAINT)


def embedded_terminal(center, harness="claude"):
    wrapf = col(center, "terminal block", gap=0, padding=[6, 12, 0, 12], sizingH="fill")
    fixed(wrapf, h=392)
    blk = col(wrapf, "terminal", gap=0, cornerRadius=8, fill=BG, stroke=ACCENT + "88",
              strokeWeight=1, sizingH="fill", sizingV="fill", clip=True)
    th = row(blk, "terminal head", gap=10, padding=[0, 10, 0, 12], sizingH="fill", fill=SURF)
    fixed(th, h=32)
    zicon(th, "terminal", ACCENT, 14)
    sw = row(th, "switch agent", gap=6, padding=[2, 8, 2, 8], cornerRadius=4, fill=SEL,
             stroke=BORDER, strokeWeight=1)
    text(sw, harness, 12, TEXT, font=BUF, style="Bold")
    text(sw, "▾", 10, FAINT)
    text(th, "--resume 3f9c2e1a" if harness == "claude" else "new session · handoff brief", 12,
         FAINT, font=BUF)
    text(th, "andy/fx1", 12, ORG, font=BUF)
    tg = row(th, "live", gap=5, padding=[1, 7, 1, 7], cornerRadius=4, fill=ACCENT + "22")
    text(tg, "●", 9, ACCENT)
    text(tg, "live · stays on this machine", 11, ACCENT)
    frame(th, "spacer", 4, 4, sizingH="fill")
    tab = row(th, "as tab", padding=[2, 8, 2, 8], cornerRadius=4, fill=SEL)
    text(tab, "Open as tab", 11, TEXT)
    back = row(th, "hand back", gap=5, padding=[2, 8, 2, 8], cornerRadius=4, fill=SEL)
    text(back, "Hand back", 11, TEXT)
    text(back, "×", 12, DIM)
    area = frame(blk, "screen", 100, 300, fill=BG, sizingH="fill", sizingV="fill", clip=True)
    g = Grid(104, 19)
    if harness == "claude":
        claude_screen(g, 0, 104, 19, banner=False, compact=True)
    else:
        codex_screen(g, 104, 19)
    g.render(area, 10, 8)
    return tab, back, sw


def switch_menu(s, current):
    m = col(s, "switch menu", gap=0, cornerRadius=8, fill=SURF, stroke=BORDER, strokeWeight=1,
            absolute=True, x=292, y=146, clip=True,
            effects=[{"type": "dropShadow", "color": "#000000aa", "offset": [0, 8],
                      "radius": 24}])
    fixed(m, w=440)
    h = row(m, "head", padding=[8, 12, 8, 12], sizingH="fill", **rule("Bottom", BORDER))
    text(h, "Terminal agents on this machine", 11, DIM, style="Semibold", textCase="upper",
         letterSpacing="6%")
    lst = col(m, "list", gap=0, padding=[4, 4, 4, 4], sizingH="fill")
    refs = {}
    for name, ver, note in INSTALLED:
        on = name == current
        hl = name == "codex"
        r = row(lst, name, gap=8, padding=[0, 10, 0, 10], cornerRadius=5, sizingH="fill",
                **({"fill": SEL} if hl else {}))
        fixed(r, h=30)
        text(r, "✓" if on else " ", 12, ACCENT, width=12)
        text(r, name, 13, TEXT if ver else DIM, font=BUF, style="Bold" if ver else "Regular")
        if ver:
            text(r, ver, 12, FAINT, font=BUF)
        frame(r, "spacer", 4, 4, sizingH="fill")
        if on:
            text(r, "current session", 11, ACCENT)
        elif note:
            text(r, note, 11, WARN if ver else DIM)
            btn = row(r, "add", padding=[2, 8, 2, 8], cornerRadius=4,
                      fill=ACCENT if not ver else SEL)
            text(btn, "Update…" if ver else "Add…", 11, BG if not ver else TEXT,
                 style="Semibold")
            refs[f"{name}:btn"] = btn
        refs[name] = r
    more = row(lst, "add other", gap=8, padding=[0, 10, 0, 10], cornerRadius=5, sizingH="fill")
    fixed(more, h=30)
    text(more, "+", 13, ACCENT, width=12)
    text(more, "Add a terminal agent…", 13, ACCENT)
    frame(more, "spacer", 4, 4, sizingH="fill")
    text(more, "any ACP agent, or a command", 11, FAINT)
    refs["add-other"] = more
    f = col(m, "foot", padding=[8, 12, 10, 12], sizingH="fill", **rule("Top", BORDER))
    wrap(f, "Switching starts a new session of that agent in the same worktree, as andy/fx1, "
            "with a handoff brief. The current session stays resumable. Found by checking PATH "
            "and each tool's --version.", 11, FAINT)
    return refs



ASK_CHAT = [
    ("15:52", "andy", None, "to listen",
     "Why does C014's release matrix skip linux-musl, and should C016 add it?", "msg",
     "opus · high"),
    ("15:52", "andy/listen", "listener", "ANSWER",
     "release.yml builds x86_64-unknown-linux-gnu only; 7c8835e dropped musl and its message "
     "gives no reason. Adding it is a CI change in lane L-INFRA. Want me to delegate C016 to a "
     "Tester? (git_show 7c8835e · 41k tokens · $0.31 · 18 s)", "reply", "opus · high"),
]

MODELS = [("claude 2.1.310", ["haiku", "sonnet", "opus"], "opus"),
          ("codex 0.164.0", ["configured model"], None),
          ("opencode 2.1.4", ["models from opencode models"], None),
          ("gemini 0.46.0", ["configured model"], None),
          ("agy 1.1.7", ["models from agy models"], None)]


def model_picker(s):
    m = col(s, "model picker", gap=0, cornerRadius=8, fill=SURF, stroke=BORDER, strokeWeight=1,
            absolute=True, x=380, y=318, clip=True,
            effects=[{"type": "dropShadow", "color": "#000000aa", "offset": [0, 8],
                      "radius": 24}])
    fixed(m, w=470)
    h = row(m, "head", padding=[8, 12, 8, 12], sizingH="fill", **rule("Bottom", BORDER))
    text(h, "Model for this message", 11, DIM, style="Semibold", textCase="upper",
         letterSpacing="6%")
    lst = col(m, "list", gap=0, padding=[4, 4, 4, 4], sizingH="fill")
    refs = []
    for harness, models, picked in MODELS:
        g = row(lst, harness, padding=[6, 10, 2, 10], sizingH="fill")
        text(g, harness, 11, FAINT, font=BUF)
        for md in models:
            on = md == picked
            r = row(lst, md, gap=8, padding=[0, 10, 0, 10], cornerRadius=5, sizingH="fill",
                    **({"fill": SEL} if on else {}))
            fixed(r, h=26)
            text(r, "✓" if on else " ", 12, ACCENT, width=12)
            text(r, md, 13, TEXT if not md.startswith(("models", "configured")) else FAINT,
                 font=BUF)
            refs.append(r)
    e = col(m, "effort", gap=6, padding=[8, 12, 8, 12], sizingH="fill", **rule("Top", BORDER))
    text(e, "Effort", 11, DIM, style="Semibold", textCase="upper", letterSpacing="6%")
    seg = row(e, "levels", gap=0, cornerRadius=5, stroke=BORDER, strokeWeight=1, clip=True)
    for lv in ["low", "medium", "high", "xhigh", "max"]:
        on = lv == "high"
        c = row(seg, lv, padding=[3, 12, 3, 12], **({"fill": ACCENT + "33"} if on else {}),
                **rule("Right", BORDER))
        text(c, lv, 12, ACCENT if on else FAINT, font=BUF)
        refs.append(c)
    wrap(e, "claude takes low to max and agy low to high; codex, gemini and opencode take a "
            "model only.", 11, FAINT)
    sticky = row(e, "sticky", gap=14)
    for label, on in [("this message", True), ("until I change it", False)]:
        r = row(sticky, label, gap=6)
        text(r, "◉" if on else "○", 12, ACCENT if on else FAINT)
        text(r, label, 12, TEXT if on else FAINT)
    f = col(m, "foot", padding=[8, 12, 10, 12], sizingH="fill", **rule("Top", BORDER))
    wrap(f, "Runs as andy/listen with the listener's tools: it can look things up, answer, post "
            "and delegate, but never edits. Spend counts against the listener's daily cap.",
         11, FAINT)
    return refs


def card_box(parent, name, width):
    c = col(parent, name, gap=6, padding=[10, 12, 10, 12], cornerRadius=8, fill=SURF,
            stroke=BORDER, strokeWeight=1)
    fixed(c, w=width)
    return c


def build_d_chat(name, palette=False, terminal=None, menu=False, ask=False, picker=False,
                 collapsed=False, alert=False, answered=False, settings=False,
                 add_agent=False):
    s, body = zwindow(name)
    _, dock_refs = left_dock(body, cursor="fx1" if terminal else None,
                             fx1=terminal if terminal == "codex" else "claude")
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    tabs = tab_bar(center, "chat", badge="chat" if alert else None)
    breadcrumb(center, ["pr-3331", "chat"], [("Show ledger", "plain")])
    if answered:
        bn = row(center, "answered banner", gap=10, padding=[8, 14, 8, 14], sizingH="fill",
                 fill=OK + "1f", **rule("Bottom", OK + "66"))
        ic = frame(bn, "icon", 18, 18, fill=OK, cornerRadius=9, layout="horizontal",
                   mainAlign="center", crossAlign="center")
        text(ic, "✓", 11, BG, style="Bold")
        text(bn, "Answered", 13, BRIGHT, style="Semibold")
        wrap(bn, "adam (coordinator) answered adam/coord's alert at 15:59. Cleared for everyone.",
             13, TEXT)
    if alert:
        bn = row(center, "needs you banner", gap=10, padding=[8, 14, 8, 14], sizingH="fill",
                 fill=ERR + "26", **rule("Bottom", ERR + "88"))
        ic = frame(bn, "icon", 18, 18, fill=ERR, cornerRadius=9, layout="horizontal",
                   mainAlign="center", crossAlign="center")
        text(ic, "!", 11, BG, style="Bold")
        text(bn, "Needs you", 13, BRIGHT, style="Semibold")
        wrap(bn, "adam/coord: CI on main is red after #10 merged; the release tag is blocked.",
             13, TEXT)
        ch = row(bn, "chime", gap=4, padding=[1, 7, 1, 7], cornerRadius=4, fill=SEL)
        text(ch, "♪", 11, ACCENT)
        text(ch, "chime played", 11, FAINT)
        jump = row(bn, "jump", padding=[3, 9, 3, 9], cornerRadius=5, fill=ERR)
        text(jump, "Jump to it", 12, BG, style="Semibold")
    stream = col(center, "stream", gap=2, padding=[10, 12, 6, 12], sizingH="fill",
                 sizingV="fill", mainAlign="max", clip=True)
    names, acts = {}, {}
    for m in CHAT:
        action = "Open in terminal  ⌥O" if m[1] == "andy/fx1" and not terminal else None
        names[m[1]], acts[m[1]] = zmessage(stream, *m, action=action)
    if ask:
        for m in ASK_CHAT:
            zmessage(stream, *m[:6], model=m[6])
    if answered:
        zmessage(stream, *ALERT_MSG[:5], "mention")
        zmessage(stream, *ANSWER_MSG)
    if settings:
        pending = settings == "pending"
        for i, m in enumerate(SETTINGS_CHAT):
            if i == 1 and pending:
                m = m[:4] + ("Both changes widen what andy/fx1 may do, so they wait for your "
                             "Apply.",) + m[5:]
            zmessage(stream, *m)
            if i == 1:
                cardc = card_box(REFS["last_content"], "change card", 560)
                if pending:
                    cmd("node.set", {"id": cardc, "stroke": WARN + "88"})
                hh = row(cardc, "head", gap=8, sizingH="fill")
                text(hh, "Waiting for you" if pending else "Settings changed", 12,
                     WARN if pending else BRIGHT, style="Semibold")
                text(hh, "requested by andy · widens access and budget" if pending else
                     "requested by andy · applied by agmend", 11, FAINT)
                for who, what, before, after in [
                        ("andy/fx1", "lanes", "L-CORE", "L-CORE, L-INFRA deploy/** until 18:00"),
                        ("andy/fx1", "budget", "$5.00", "$8.00")]:
                    rr = row(cardc, what, gap=8, sizingH="fill")
                    text(rr, who, 12, ORG, font=BUF)
                    text(rr, what, 12, FAINT, width=52)
                    text(rr, before, 12, ERR, font=BUF, textDecoration="strikethrough")
                    text(rr, "→", 12, DIM)
                    text(rr, after, 12, OK, font=BUF)
                br = row(cardc, "buttons", gap=6)
                for label in (["Apply", "Cancel"] if pending else ["Undo", "Audit log"]):
                    primary = label == "Apply"
                    b = row(br, label, padding=[3, 9, 3, 9], cornerRadius=5,
                            fill=ACCENT if primary else SEL)
                    text(b, label, 12, BG if primary else TEXT,
                         style="Semibold" if primary else "Regular")
                    link(b, "Desktop · settings applied" if primary else "Desktop · chat")
    if alert:
        zmessage(stream, *ALERT_MSG)
        for b in REFS["alert_actions"]:
            link(b, "Desktop · chat")
        toast = col(s, "notification", gap=4, padding=[10, 12, 10, 12], cornerRadius=10,
                    fill=SURF, stroke=ERR + "88", strokeWeight=1, absolute=True, x=1064, y=150,
                    effects=[{"type": "dropShadow", "color": "#000000aa", "offset": [0, 8],
                              "radius": 24}])
        fixed(toast, w=360)
        th = row(toast, "head", gap=8, sizingH="fill")
        ic = frame(th, "icon", 18, 18, fill=ERR, cornerRadius=5, layout="horizontal",
                   mainAlign="center", crossAlign="center")
        text(ic, "!", 11, BG, style="Bold")
        text(th, "agmen", 12, FAINT)
        frame(th, "spacer", 4, 4, sizingH="fill")
        text(th, "now", 11, DIM)
        text(toast, "adam/coord needs you", 13, BRIGHT, style="Semibold")
        wrap(toast, "CI on main is red after #10 merged. Revert #10, or hotfix on a branch?",
             12, TEXT)
        text(toast, "♪ urgent chime · also sent to your phone", 11, FAINT)
    if terminal:
        tab_ref, back_ref, sw_ref = embedded_terminal(center, terminal)
        link(tab_ref, "Desktop · agent")
        link(back_ref, "Desktop · chat")
        if not menu:
            link(sw_ref, "Desktop · switch agent")
    _, mchip = zcomposer(center, "andy/listen",
                         "Ask or tell your listener · @ to mention someone else · ⏎ send",
                         model="opus · high" if (ask or picker) else "codex · default")
    if mchip and not picker:
        link(mchip, "Desktop · model picker")
    if picker:
        for r in model_picker(s):
            link(r, "Desktop · ask")
    if terminal:
        agent_dock(body, taken_over=True, harness=terminal)
    elif not collapsed:
        rd = right_dock(body, "Listener", "andy/listen")
        section(rd, "Runs", [("Harness", "Codex 0.164.0 over ACP", TEXT),
                             ("Role", "Listener (built-in)", TEXT),
                             ("Can use", "Read Grep Glob git_log git_show git_diff git_status",
                              OK),
                             ("Never", "Edit Write Bash", ERR)])
        section(rd, "Policy", [("Takes work from", "andy, coordinators", TEXT),
                               ("Per worker", "$5.00", TEXT),
                               ("Workers today", "$11.20 of $20.00 · 3 of 4 running", TEXT),
                               ("Listener today", "$0.42 of $2.00", TEXT),
                           ("Listener tokens", "96k in · 8k out · up 6 h 40 min", TEXT)])
        section(rd, "Recent", ([("15:50", "waiting for Apply: andy/fx1 lanes and budget"
                                if settings == "pending" else
                                "changed andy/fx1 lanes and budget (Undo)",
                                WARN if settings == "pending" else ACCENT)]
                               if settings else []) +
                [("15:31", "delegated andy/fx1 · Engineer · claude", TEXT),
                               ("14:06", "delegated andy/t7k · Tester · opencode", TEXT),
                               ("13:52", "answered: is C012 merged? (git_log)", FAINT)])
        btn = row(rd, "change", padding=[5, 10, 5, 10], cornerRadius=5, fill=SURF,
                  stroke=BORDER, strokeWeight=1)
        text(btn, "Change listener…", 12, TEXT)
    pal = statusbar(s, "andy/fx1 · taken over" if terminal else "listener codex · idle",
                    dock_toggle=collapsed, alert=alert)
    if not (terminal or palette or ask or picker or collapsed or alert or answered or settings):
        link(REFS["2 alerts"], "Desktop · needs you")
    if collapsed:
        link(REFS["toggle"], "Desktop · chat")
        link(s, "Desktop · chat", "Cmd+R")
    elif not terminal and not palette and not answered and not settings:
        link(REFS["collapse"], "Desktop · listener collapsed")
        link(s, "Desktop · listener collapsed", "Cmd+R")
    link(dock_refs[("andy (you)", "fx1")], "Desktop · agent")
    link(names["andy/fx1"], "Desktop · agent")
    link(tabs["andy/fx1"], "Desktop · agent")
    link(tabs["reviewer.xml"], "Desktop · role file")
    if acts.get("andy/fx1"):
        link(acts["andy/fx1"], "Desktop · chat + terminal")
    if terminal:
        if menu:
            refs = switch_menu(s, terminal)
            for k, r in refs.items():
                if k.endswith(":btn") or k == "add-other":
                    link(r, "Desktop · add agent")
                elif k in ("grok",):
                    continue
                else:
                    link(r, "Desktop · chat + codex" if k == "codex" else
                         "Desktop · chat + terminal")
        if add_agent:
            add_agent_dialog(s)
        return
    if alert:
        link(s, "Desktop · needs you (answered)", "delay:6000")
    if ask or picker or collapsed or alert or answered or settings:
        return
    if not palette:
        link(pal, "Desktop · palette")
        link(s, "Desktop · palette", "Cmd+Shift+P")
        link(s, "Desktop · chat + terminal", "Alt+O")
        return
    pop = col(s, "command palette", gap=0, cornerRadius=8, fill=SURF, stroke=BORDER,
              strokeWeight=1, absolute=True, x=420, y=64, clip=True,
              effects=[{"type": "dropShadow", "color": "#000000aa", "offset": [0, 10],
                        "radius": 30}])
    fixed(pop, w=600)
    inp = row(pop, "input", gap=0, padding=[10, 14, 10, 14], sizingH="fill", **rule("Bottom",
                                                                                    BORDER))
    zicon(inp, "magnifying_glass", FAINT, 14)
    frame(inp, "gap", 8, 4)
    text(inp, "spawn", 14, TEXT)
    frame(inp, "caret", 2, 18, fill=ACCENT)
    items = [("agmen:", "spawn", "worker…", "⌘N", "Desktop · chat"),
             ("agmen:", "spawn", "worker with codex", "", "Desktop · chat"),
             ("agmen:", "spawn", "worker with claude", "", "Desktop · chat"),
             ("agmen:", "spawn", "worker with grok", "", "Desktop · chat"),
             ("agmen: open agent in terminal here", "", "", "⌥O", "Desktop · chat + terminal"),
             ("agmen: set listener…", "", "", "", "Desktop · chat"),
             ("worktrees: prune merged", "", "", "", "Desktop · chat")]
    lst = col(pop, "list", gap=0, padding=[4, 4, 4, 4], sizingH="fill")
    for i, (a, b, c, key, dest) in enumerate(items):
        r = row(lst, f"item {i}", gap=4, padding=[0, 10, 0, 10], cornerRadius=5, sizingH="fill",
                **({"fill": SEL} if i == 0 else {}))
        fixed(r, h=30)
        text(r, a, 13, TEXT)
        if b:
            text(r, b, 13, ACCENT, style="Semibold")
        if c:
            text(r, c, 13, TEXT)
        frame(r, "spacer", 4, 4, sizingH="fill")
        if key:
            text(r, key, 12, FAINT, font=BUF)
        link(r, dest)


def build_d_agent(name):
    s, body = zwindow(name)
    left_dock(body, cursor="fx1")
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    tabs = tab_bar(center, "andy/fx1")
    btns = breadcrumb(center, ["andy/fx1", "C015", "fix/c015-main-conflict"],
                      [("Open in terminal  ⌥O", "primary"), ("Stop", "plain")])
    pane = col(center, "session", gap=8, padding=[14, 18, 10, 18], sizingH="fill",
               sizingV="fill", clip=True)
    for kind, a, b in SESSION:
        if kind == "tool":
            card = row(pane, f"tool {a}", gap=8, padding=[6, 10, 6, 10], cornerRadius=6,
                       sizingH="fill", fill=SURF, stroke=BORDER, strokeWeight=1)
            text(card, "▸", 11, DIM)
            text(card, a, 12, ACCENT, font=BUF, style="Bold")
            wrap(card, b, 12, FAINT, font=BUF)
        elif kind == "out":
            text(pane, "   ⎿ " + a, 12, b, font=BUF)
        elif kind == "say":
            wrap(pane, a, 13, TEXT)
        elif kind == "cost":
            text(pane, a, 11, DIM)
    zcomposer(center, None, "Message andy/fx1 · sent as a DM, recorded by the hub")
    agent_dock(body)
    statusbar(s, "andy/fx1 · working")
    link(tabs["chat"], "Desktop · chat")
    link(tabs["reviewer.xml"], "Desktop · role file")
    link(btns["Open in terminal  ⌥O"], "Desktop · chat + terminal")
    link(s, "Desktop · chat + terminal", "Alt+O")


XML_LINES = [
    '<!-- built-in role, ships with agmen -->',
    '<role name="reviewer" extends="base" kind="agent" tier="deep">',
    '  <purpose>',
    '    Review one DONE: read its diff, trace each path it changes,',
    '    and report findings. You review; you do not fix.',
    '  </purpose>',
    '  <tools allow="Read Grep Glob Bash" />',
    '  <verbs allow="REVIEW ASK ANSWER FYI DM" />',
    '  <worktree mode="detached" at="done.commit" remove="after-review" />',
    '  <rules>',
    '    <rule>Scope the review by execution paths, not by changed files.</rule>',
    '    <rule>Give each finding path:line, what breaks, and the input that',
    '          triggers it. Order by severity, worst first.</rule>',
    '    <rule>Say which paths you traced and which you left.</rule>',
    '    <rule>In Bash, run only commands that write nothing.</rule>',
    '    <rule>If nothing is wrong, say so in one line.</rule>',
    '  </rules>',
    '  <done-when>You posted one REVIEW whose reply_to is the DONE.</done-when>',
    '  <report>REVIEW {verdict: pass | changes, findings}</report>',
    '</role>',
]
TOKEN = re.compile(r'(<!--.*?-->)|(</?[\w-]+)|(\s[\w-]+=)|("[^"]*")|(/?>)|([^<"]+)')


def build_d_role(name):
    s, body = zwindow(name)
    _, dock_refs = left_dock(body, tab="Roles")
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    tabs = tab_bar(center, "reviewer.xml")
    breadcrumb(center, ["roles", "built-in", "reviewer.xml"],
               [("Duplicate to .agmen/roles", "primary")])
    ed = frame(center, "editor", 100, 100, fill=BG, sizingH="fill", sizingV="fill", clip=True)
    g = Grid(110, 40)
    for i, line in enumerate(XML_LINES):
        r = i + 1
        if i == 10:
            g.bg(r, 0, 110, SURF)
        g.rput(r, 4, str(i + 1), TEXT if i == 10 else LINE)
        c = 6
        for m in TOKEN.finditer(line):
            tok = m.group(0)
            if m.group(1):
                color = DIM
            elif m.group(2):
                color = ACCENT
            elif m.group(3):
                color = WARN
            elif m.group(4):
                color = OK
            elif m.group(5):
                color = ACCENT
            else:
                color = TEXT
            c = g.put(r, c, tok, color)
    g.put(11, 6 + len(XML_LINES[10]), " ", BG, ACCENT)
    g.render(ed, 0, 8)
    rd = right_dock(body, "Role", "reviewer")
    section(rd, "Enforced by agmen", [("Kind", "agent", TEXT),
                                      ("Tools", "Read Grep Glob Bash", TEXT),
                                      ("Bash", "read-only by rule only; not enforceable", WARN),
                                      ("Edits", "none", TEXT),
                                      ("Verbs", "REVIEW ASK ANSWER FYI DM", OK),
                                      ("Refused", "CLAIM DONE PUSH_REQUEST ASSIGN RULING", ERR),
                                      ("Tier", "deep", TEXT)])
    section(rd, "Used by", [("andy/rv2", "grok · reading B-409", TEXT),
                            ("adam/sec", "security-reviewer extends it", FAINT)])
    section(rd, "Mention", [("@reviewers", "role:reviewer on the wire · humans and "
                                           "coordinators", TEXT)])
    statusbar(s, "reviewer.xml · XML")
    link(tabs["chat"], "Desktop · chat")
    link(tabs["andy/fx1"], "Desktop · agent")



MAIN_RS = [
    ("mod app;", None), ("mod cylinder;", None), ("mod document;", None),
    ("mod minimap;", "del"), ("mod ui;", "add"), ("", None), ("use ui::tabs::Tabs;", "add"),
    ("", None), ("fn main() -> anyhow::Result<()> {", None),
    ("    let tabs = Tabs::default();", None), ("    app::run(tabs)", None), ("}", None),
]
DIFF = [
    ("@@ -1,13 +1,11 @@", "hunk"), (" mod app;", None), ("-<<<<<<< HEAD", "del"),
    (" mod cylinder;", None), (" mod document;", None), ("-=======", "del"),
    (" mod minimap;", None), ("-mod tabs;", "del"), ("->>>>>>> 4d9fdc8", "del"),
    ("+mod ui;", "add"), (" ", None), ("-use tabs::Tabs;", "del"), ("+use ui::tabs::Tabs;", "add"),
    (" ", None), (" fn main() -> anyhow::Result<()> {", None),
]
RUST = re.compile(r'(\b(?:mod|use|fn|let|pub)\b)|(\b[A-Z]\w*\b)|(::|[{}();<>\-=.,&]+)|(\w+)|(\s+)')


def put_rust(g, r, c, line):
    for m in RUST.finditer(line):
        tok = m.group(0)
        color = (ERR if m.group(1) else WARN if m.group(2) else BRIGHT if m.group(3)
                 else TEXT)
        c = g.put(r, c, tok, color)
    return c


def build_d_tree(name, tab, variant=None):
    s, body = zwindow(name)
    _, refs = left_dock(body, tab=tab, cursor="fx1", variant=variant)
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    if tab == "Project":
        tabs = tab_bar(center, "main.rs", ("main.rs", "f:main.rs", ORG))
        btns = breadcrumb(center, ["andy/fx1", "src", "main.rs"], [("Diff", "plain")])
    else:
        tabs = tab_bar(center, "main.rs (diff)", ("main.rs (diff)", "z:diff", WARN))
        btns = breadcrumb(center, ["andy/fx1", "changes", "src/main.rs"],
                          [("Open file", "plain")])
    ed = frame(center, "editor", 100, 100, fill=BG, sizingH="fill", sizingV="fill", clip=True)
    g = Grid(150, 40)
    if tab == "Project":
        for i, (line, mark) in enumerate(MAIN_RS):
            r = i + 1
            g.rput(r, 4, str(i + 1), LINE)
            if mark == "add":
                g.bg(r, 5, 6, OK)
            put_rust(g, r, 7, line)
        g.put(4, 5, "▾", ERR)
    else:
        for i, (line, kind) in enumerate(DIFF):
            r = i + 1
            if kind == "hunk":
                g.put(r, 2, line, ACCENT)
                continue
            if kind == "add":
                g.bg(r, 0, 150, OK + "22")
            elif kind == "del":
                g.bg(r, 0, 150, ERR + "22")
            g.put(r, 2, line[0], OK if kind == "add" else ERR if kind == "del" else LINE,
                  bold=True)
            put_rust(g, r, 4, line[1:])
    g.render(ed, 0, 8)
    statusbar(s, "andy/fx1 · fix/c015-main-conflict", dock_toggle=True)
    link(REFS["toggle"], "Desktop · chat")
    link(tabs["chat"], "Desktop · chat")
    link(tabs["andy/fx1"], "Desktop · agent")
    if tab == "Project":
        link(refs["tests"], "Desktop · project" if variant == "tests" else
             "Desktop · project (tests open)")
        link(btns["Diff"], "Desktop · git")
    else:
        link(refs["Changes"], "Desktop · git" if variant == "collapsed" else
             "Desktop · git (collapsed)")
        link(btns["Open file"], "Desktop · project")


TEAM = [("coordinator", "Coordinator", "coord", "claude · sonnet", "board", "andy", 10),
        ("engineer", "Engineer", "core", "codex", "L-CORE", "andy", 20),
        ("engineer", "Engineer", "app", "claude · sonnet", "L-APP", "andy", 20),
        ("reviewer", "Reviewer", "rev", "claude · opus", "all", "andy", 15),
        ("engineer", "Engineer", "sync", "opencode", "L-SYNC", "sean", 20),
        ("tester", "Tester", "qa", "gemini", "L-TESTS", "sean", 10),
        ("researcher", "Tile Researcher", "tiles", "agy", "none", "sean", 10)]

PLAN_CHAT = [
    ("15:40", "andy", None, "to sean/plan",
     "Greenfield: Ridgeline, offline trail maps for iOS and Android on a Rust core. Can you draft "
     "the team and the first specs?", "msg"),
    ("15:41", "sean/plan", "planner", "PROPOSAL",
     "Plan v1 is on the right: 7 agents, 3 specs, 4 lanes, about $105/day. Open question: who "
     "owns map tiles? The license decides whether we self-host them.", "msg"),
    ("15:43", "sean", None, "FYI",
     "Run tile research on my machine with agy. I'll take sync too, on opencode.", "msg"),
    ("15:44", "sean/plan", "planner", "PROPOSAL",
     "Plan v2: tiles (new tile-researcher role) and sync run on sean's machine; tile-sources.md "
     "added for the researcher to fill in.", "reply"),
    ("15:47", "andy", None, "to sean/plan",
     "Approved ridgeline-design.md. Make the reviewer deep tier and give core to codex.", "msg"),
    ("15:48", "sean/plan", "planner", "PROPOSAL",
     "Plan v3: reviewer on opus, core on codex. Waiting on sean's approval of the design spec "
     "and his 3 agents.", "reply"),
]
PLAN_READY = [("15:52", "sean", None, "DECISION",
               "Approved ridgeline-design.md and my 3 agents ($40/day).", "msg")]
PLAN_RUN = [
    ("15:55", "andy", None, "DECISION", "Kick off plan v3.", "msg"),
    ("15:55", "hub", None, "STATE",
     "lanes L-CORE L-APP L-SYNC L-TESTS and items R-1 to R-12 created from docs/plans/m0.md",
     "ledger"),
    ("15:55", "andy/coord", "coordinator", "ASSIGN",
     "R-1 to @andy/core, R-2 to @andy/app, R-3 to @sean/sync, R-4 to @sean/qa", "msg"),
    ("15:56", "andy/core", "engineer", "DM",
     "@sean/sync the tile cache API lands in core first. I'll post its signature before you "
     "build on it, so we don't both write one.", "msg"),
    ("15:56", "sean/sync", "engineer", "ANSWER",
     "Agreed. I'll stub sync against your trait and swap in the real one when R-1 is DONE.",
     "reply"),
    ("15:57", "sean/tiles", "researcher", "FYI",
     "Starting the tile licensing survey for tile-sources.md.", "msg"),
]


def plan_left(body, running):
    d = col(body, "left dock", gap=0, sizingV="fill", **rule("Right"))
    fixed(d, w=272)
    tabs = row(d, "dock tabs", gap=10, padding=[0, 10, 0, 10], sizingH="fill", **rule("Bottom"))
    fixed(tabs, h=34)
    for t in ["Agents", "Project", "Git", "Roles"]:
        tr = row(tabs, f"tab {t}", gap=5)
        zicon(tr, TAB_ICONS[t], TEXT if t == "Agents" else FAINT, 14)
        text(tr, t, 12, TEXT if t == "Agents" else FAINT,
             style="Semibold" if t == "Agents" else "Regular")
    tree = col(d, "tree", gap=0, padding=[6, 0, 6, 0], sizingH="fill")
    git_section(tree, "Planning room", "4", True)
    for handle, role, harness, status in [("andy", None, "you", "idle"),
                                          ("sean", None, "human", "working"),
                                          ("sean/plan", "planner", "claude · opus", "working"),
                                          ("andy/listen", "listener", "codex", "idle")]:
        r = tree_row(tree, 1, handle, h=26)
        glyph, scol = STATUS[status]
        text(r, glyph, 10, scol)
        text(r, handle, 13, TEXT, font=BUF)
        if role:
            text(r, ROLE[role][0], 12, ROLE[role][1])
        frame(r, "spacer", 4, 4, sizingH="fill")
        text(r, harness, 11, FAINT, font=BUF)
    git_section(tree, "Ridgeline team", "7" if running else "planned", running)
    if running:
        for person in ["andy", "sean"]:
            r = tree_row(tree, 1, person)
            zicon(r, "chevron_down", DIM, 12)
            text(r, person, 13, TEXT, style="Medium")
            for key, rname, handle, harness, lane, owner, cost in TEAM:
                if owner != person:
                    continue
                r = tree_row(tree, 2, handle, h=26)
                text(r, "●", 10, OK)
                text(r, handle, 13, TEXT, font=BUF)
                text(r, rname, 12, ROLE[key][1])
                frame(r, "spacer", 4, 4, sizingH="fill")
                text(r, harness.split(" ")[0], 11, FAINT, font=BUF)


def plan_tabs(center, active):
    tb = row(center, "tab bar", gap=0, sizingH="fill", **rule("Bottom"))
    fixed(tb, h=34)
    refs = {}
    for label, icon, color in [("plan", "z:chat", ACCENT),
                               ("ridgeline-design.md", "f:ridgeline-design.md", WARN)]:
        on = label == active
        t = row(tb, f"tab {label}", gap=8, padding=[0, 14, 0, 14], sizingV="fill",
                **({"fill": SURF} if on else {}), **rule("Right"))
        icon_spec(t, icon, color, 14)
        text(t, label, 13, TEXT if on else FAINT, font=BUF if label != "plan" else UI)
        refs[label] = t
    return refs


def plan_panel(body, state):
    d = col(body, "plan panel", gap=0, sizingV="fill", **rule("Left"))
    fixed(d, w=470)
    h = row(d, "head", gap=8, padding=[0, 14, 0, 14], sizingH="fill", **rule("Bottom"))
    fixed(h, h=34)
    text(h, "Plan", 13, TEXT, style="Semibold")
    text(h, "ridgeline · v3", 12, FAINT, font=BUF)
    frame(h, "spacer", 4, 4, sizingH="fill")
    label, color = {"draft": ("waiting on 2 approvals", WARN), "ready": ("ready to kick off", OK),
                    "running": ("running · 7 agents", ACCENT)}[state]
    st = row(h, "state", padding=[1, 7, 1, 7], cornerRadius=4, fill=color + "26")
    text(st, label, 11, color)
    c = col(d, "content", gap=14, padding=[12, 14, 12, 14], sizingH="fill")

    def head(t):
        text(c, t, 11, DIM, style="Semibold", textCase="upper", letterSpacing="6%")

    head("Goal")
    wrap(c, "Offline trail maps for iOS and Android on a shared Rust core. First milestone: a "
            "downloaded region renders with no network.", 12, TEXT)
    head("Team · 7 agents")
    tbl = col(c, "team", gap=0, sizingH="fill", cornerRadius=6, stroke=BORDER, strokeWeight=1,
              clip=True)
    for key, rname, handle, harness, lane, owner, cost in TEAM:
        r = row(tbl, handle, gap=6, padding=[0, 8, 0, 8], sizingH="fill", **rule("Bottom"))
        fixed(r, h=26)
        if state == "running":
            text(r, "●", 9, OK)
        dot(r, ROLE[key][1], 7)
        text(r, rname, 12, ROLE[key][1], width=100)
        text(r, handle, 12, TEXT, font=BUF, width=40)
        text(r, harness, 11, FAINT, font=BUF, width=100)
        text(r, lane, 11, TEXT, font=BUF, width=56)
        text(r, owner, 11, FAINT, width=32)
        frame(r, "spacer", 4, 4, sizingH="fill")
        text(r, f"${cost}/d", 11, FAINT, font=BUF)
    wrap(c, "$105/day: andy $65, sean $40. Each agent runs on its owner's machine with that "
            "person's keys, and each person approves their own share.", 11, FAINT)
    head("Specs")
    refs = {}
    for f, stt in [("docs/specs/ridgeline-design.md",
                    "approved" if state != "draft" else "andy ✓ · sean ○"),
                   ("docs/specs/tile-sources.md", "draft, researcher fills in"),
                   ("docs/plans/m0.md", "approved"),
                   (".agmen/roles/tile-researcher.xml", "approved")]:
        r = row(c, f, gap=8, sizingH="fill")
        text(r, f, 12, ACCENT if f.endswith("design.md") else TEXT, font=BUF)
        frame(r, "spacer", 4, 4, sizingH="fill")
        text(r, stt, 11, OK if stt == "approved" else WARN)
        refs[f] = r
    head("Lanes")
    wrap(c, "L-CORE core/** · L-APP app/** · L-SYNC sync/** · L-TESTS tests/**", 12, TEXT,
         font=BUF)
    head("Before kick-off")
    for t, ok in [("Repo ridgeline exists on the git host", True),
                  ("Lanes don't overlap", True),
                  ("Specs approved by andy and sean", state != "draft"),
                  ("andy approves 4 agents, $65/day", True),
                  ("sean approves 3 agents, $40/day", state != "draft")]:
        r = row(c, t, gap=8)
        text(r, "✓" if ok else "○", 12, OK if ok else WARN)
        text(r, t, 12, TEXT if ok else FAINT)
    if state == "running":
        btn = row(c, "running", gap=8, padding=[7, 12, 7, 12], cornerRadius=6, fill=SURF,
                  stroke=BORDER, strokeWeight=1)
        text(btn, "Pause all", 13, TEXT)
    else:
        on = state == "ready"
        btn = row(c, "kick off", gap=8, padding=[8, 14, 8, 14], cornerRadius=6,
                  fill=ACCENT if on else SURF, stroke=BORDER, strokeWeight=0 if on else 1)
        text(btn, "Kick off · 7 agents" if on else "Kick off · waiting on sean", 13,
             BG if on else FAINT, style="Semibold")
    refs["button"] = btn
    return refs


def build_d_plan(name, state):
    s, body = zwindow(name, "ridgeline")
    plan_left(body, state == "running")
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    tabs = plan_tabs(center, "plan")
    breadcrumb(center, ["ridgeline", "plan"], [])
    stream = col(center, "stream", gap=2, padding=[10, 12, 6, 12], sizingH="fill",
                 sizingV="fill", mainAlign="max", clip=True)
    msgs = PLAN_CHAT + (PLAN_READY if state != "draft" else []) + (
        PLAN_RUN if state == "running" else [])
    for m in msgs:
        zmessage(stream, *m)
    zcomposer(center, "sean/plan",
              "Plan with sean/plan · it runs on sean's machine with his keys · ⏎ send")
    refs = plan_panel(body, "ready" if state == "confirm" else state)
    statusbar(s, "ridgeline · planning" if state != "running" else "ridgeline · 7 agents")
    link(tabs["ridgeline-design.md"], "Desktop · spec")
    link(refs["docs/specs/ridgeline-design.md"], "Desktop · spec")
    if state == "ready":
        link(refs["button"], "Desktop · kick off")
    if state == "confirm":
        m = col(s, "modal dim", gap=0, absolute=True, x=0, y=0, fill="#000000a0")
        fixed(m, 1440, 900)
        md = col(s, "kick off", gap=12, padding=20, cornerRadius=10, fill=SURF, stroke=BORDER,
                 strokeWeight=1, absolute=True, x=410, y=150,
                 effects=[{"type": "dropShadow", "color": "#000000aa", "offset": [0, 10],
                           "radius": 30}])
        fixed(md, w=620)
        text(md, "Kick off ridgeline (plan v3)", 17, BRIGHT, style="Semibold")
        for who, agents, money, status in [
                ("andy's machine (you)", "coord, core, app, rev", "$65/day of your $80 cap",
                 "you approve now"),
                ("sean's machine", "sync, qa, tiles", "$40/day of sean's $60 cap",
                 "sean approved 15:52")]:
            b = col(md, who, gap=3, padding=[10, 12, 10, 12], cornerRadius=6, fill=BG,
                    sizingH="fill")
            r = row(b, "h", gap=8, sizingH="fill")
            text(r, who, 13, TEXT, style="Semibold")
            frame(r, "spacer", 4, 4, sizingH="fill")
            text(r, "✓ " + status, 12, OK)
            text(b, f"{agents} · {money}", 12, FAINT, font=BUF)
        text(md, "What happens", 11, DIM, style="Semibold", textCase="upper", letterSpacing="6%")
        for t in ["The specs are committed to docs/specs/ on main before anyone starts.",
                  "The hub creates 4 lanes and items R-1 to R-12 from docs/plans/m0.md.",
                  "Each person's agmend launches only their own agents, each in a new worktree "
                  "on a {type}/{item}-{slug} branch.",
                  "andy/coord assigns the first items. From then on the agents coordinate with "
                  "each other in this project's chat, and anything that needs you arrives as "
                  "an alert."]:
            r = row(md, t[:20], gap=8, sizingH="fill", crossAlign="min")
            text(r, "•", 12, ACCENT)
            wrap(r, t, 12, TEXT)
        ft = row(md, "footer", gap=8, sizingH="fill", mainAlign="max")
        cancel = row(ft, "cancel", padding=[6, 12, 6, 12], cornerRadius=6, fill=BG,
                     stroke=BORDER, strokeWeight=1)
        text(cancel, "Cancel", 13, TEXT)
        go = row(ft, "go", padding=[6, 14, 6, 14], cornerRadius=6, fill=ACCENT)
        text(go, "Kick off 7 agents", 13, BG, style="Semibold")
        link(cancel, "Desktop · plan ready")
        link(go, "Desktop · running")


MD = """# Ridgeline design
Status: v3 · drafted by sean/plan · approved by andy

## 1. Intent
Offline trail maps for iOS and Android. A downloaded region renders
with no network, and a hike records to a GPX track.

## 2. Goals
- Render a downloaded region offline in under 1 s on a 2023 phone.
- One Rust core for tiles, routing and sync; thin native UIs.
- Sync tracks between devices when online.

## 3. Non-goals
- Turn-by-turn navigation in v1.
- A web app.

## 4. Lanes
| Lane    | Paths    | Agent     | Runs on |
| L-CORE  | core/**  | andy/core | andy    |
| L-APP   | app/**   | andy/app  | andy    |
| L-SYNC  | sync/**  | sean/sync | sean    |
| L-TESTS | tests/** | sean/qa   | sean    |

## 5. Open questions
- Tile source and license: tile-sources.md, sean/tiles.""".split("\n")


def build_d_spec(name):
    s, body = zwindow(name, "ridgeline")
    plan_left(body, False)
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    tabs = plan_tabs(center, "ridgeline-design.md")
    breadcrumb(center, ["ridgeline", "docs", "specs", "ridgeline-design.md"],
               [("Approved ✓", "primary"), ("Request changes", "plain")])
    ed = frame(center, "editor", 100, 100, fill=BG, sizingH="fill", sizingV="fill", clip=True)
    g = Grid(120, 40)
    for i, line in enumerate(MD):
        r = i + 1
        g.rput(r, 4, str(i + 1), LINE)
        if line.startswith("#"):
            g.put(r, 6, line, ACCENT, bold=True)
        elif line.startswith("- "):
            g.put(r, 6, "-", WARN)
            g.put(r, 8, line[2:], TEXT)
        elif line.startswith("|"):
            for k, part in enumerate(line.split("|")):
                pass
            c = 6
            for ch in line:
                c = g.put(r, c, ch, LINE if ch == "|" else TEXT)
        else:
            g.put(r, 6, line, FAINT if line.startswith("Status") else TEXT)
    g.render(ed, 0, 8)
    rd = right_dock(body, "Review", "ridgeline-design.md")
    section(rd, "Approvals", [("andy", "✓ approved 15:47", OK),
                              ("sean", "○ waiting", WARN)])
    section(rd, "Comments", [("sean/plan", "§2: '1 s' is a target until R-1 measures tile "
                                           "decode on a real phone.", TEXT),
                             ("andy", "Fine, keep it as the target.", TEXT),
                             ("sean/plan", "§5: tiles decides self-hosting; the plan stays "
                                           "valid either way.", TEXT)])
    statusbar(s, "ridgeline · planning")
    link(tabs["plan"], "Desktop · plan ready")


ADD_PROMPT = [
    "Install xAI's Grok Build CLI (the `grok` command) on this machine",
    "from xAI's official instructions at github.com/xai-org/grok-build.",
    "Do not use the community npm package @vibe-kit/grok-cli; it also",
    "installs a `grok` command. When it is installed, run `grok --version`",
    "and `agmen agents scan`, and tell me both outputs.",
]


def add_agent_dialog(s):
    dim = col(s, "dim", gap=0, absolute=True, x=0, y=0, fill="#000000a0")
    fixed(dim, 1440, 900)
    md = col(s, "add agent", gap=12, padding=20, cornerRadius=10, fill=SURF, stroke=BORDER,
             strokeWeight=1, absolute=True, x=380, y=120,
             effects=[{"type": "dropShadow", "color": "#000000aa", "offset": [0, 10],
                       "radius": 30}])
    fixed(md, w=680)
    h = row(md, "title", gap=10, sizingH="fill")
    text(h, "Add Grok Build", 17, BRIGHT, style="Semibold")
    text(h, "xAI's terminal coding agent · `grok` not found on PATH", 12, FAINT)
    text(md, "Install it yourself", 11, DIM, style="Semibold", textCase="upper",
         letterSpacing="6%")
    r = row(md, "docs", gap=8)
    text(r, "Follow xAI's official instructions:", 13, TEXT)
    lk = row(r, "link", padding=[0, 4, 0, 4], cornerRadius=3)
    text(lk, "github.com/xai-org/grok-build ↗", 12, ACCENT, font=BUF, textDecoration="underline")
    text(md, "Or give this prompt to an agent you already run", 11, DIM, style="Semibold",
         textCase="upper", letterSpacing="6%")
    box = col(md, "prompt", gap=2, padding=[10, 12, 10, 12], cornerRadius=6, fill=BG,
              stroke=BORDER, strokeWeight=1, sizingH="fill")
    for ln in ADD_PROMPT:
        text(box, ln, 12, TEXT, font=BUF)
    cp = row(md, "copy row", gap=8)
    copy = row(cp, "copy", gap=6, padding=[4, 10, 4, 10], cornerRadius=5, fill=SEL)
    text(copy, "⧉", 12, ACCENT)
    text(copy, "Copy prompt", 12, TEXT)
    text(cp, "paste it into Claude Code, Codex or any agent with a shell; your listener has none",
         11, FAINT)
    text(md, "Then", 11, DIM, style="Semibold", textCase="upper", letterSpacing="6%")
    wrap(md, "agmen looks for `grok` on PATH, runs `grok --version`, and checks that "
             "`grok agent stdio` starts over ACP. It appears in this list once both pass.",
         12, TEXT)
    wrap(md, "agmen runs no installers itself: install steps change, and the prompt names the "
             "official source so an agent does not pick up a look-alike package.", 11, FAINT)
    ft = row(md, "footer", gap=8, sizingH="fill", mainAlign="max")
    cancel = row(ft, "cancel", padding=[6, 12, 6, 12], cornerRadius=6, fill=BG, stroke=BORDER,
                 strokeWeight=1)
    text(cancel, "Cancel", 13, TEXT)
    again = row(ft, "check", padding=[6, 14, 6, 14], cornerRadius=6, fill=ACCENT)
    text(again, "Check again", 13, BG, style="Semibold")
    link(cancel, "Desktop · switch agent")
    link(again, "Desktop · switch agent")


def seg(parent, options, picked):
    sg = row(parent, "segmented", gap=0, cornerRadius=5, stroke=BORDER, strokeWeight=1, clip=True)
    for o in options:
        on = o == picked
        c = row(sg, o, padding=[3, 10, 3, 10], **({"fill": ACCENT + "33"} if on else {}),
                **rule("Right", BORDER))
        text(c, o, 12, ACCENT if on else FAINT)


def dropdown(parent, value, width=300, mono=False):
    d = row(parent, "dropdown", gap=8, padding=[5, 10, 5, 10], cornerRadius=5, fill=BG,
            stroke=BORDER, strokeWeight=1, mainAlign="spaceBetween")
    fixed(d, w=width)
    text(d, value, 13, TEXT, font=BUF if mono else UI)
    zicon(d, "chevron_down", FAINT, 12)
    return d


def stepper(parent, value):
    st = row(parent, "size", gap=0, cornerRadius=5, stroke=BORDER, strokeWeight=1, clip=True)
    for lbl in ["−", value, "+"]:
        c = row(st, lbl, padding=[3, 9, 3, 9], **({"fill": BG} if lbl == value else {}))
        text(c, lbl, 12, TEXT if lbl == value else FAINT, font=BUF)


def setting(parent, label, hint):
    r = row(parent, label, gap=16, padding=[12, 0, 12, 0], sizingH="fill", crossAlign="min",
            **rule("Bottom"))
    lc = col(r, "label", gap=3)
    fixed(lc, w=230)
    text(lc, label, 13, BRIGHT, style="Medium")
    wrap(lc, hint, 11, FAINT)
    ctl = col(r, "control", gap=8, sizingH="fill")
    return ctl


def build_d_settings(name):
    s, body = zwindow(name)
    left_dock(body)
    center = col(body, "center", gap=0, sizingH="fill", sizingV="fill")
    tabs = tab_bar(center, "settings", ("settings", "z:settings", FAINT))
    breadcrumb(center, ["settings", "appearance"], [("Open settings.toml", "plain")])
    main = row(center, "settings body", gap=0, sizingH="fill", sizingV="fill", crossAlign="min")
    cats = col(main, "categories", gap=2, padding=[12, 8, 12, 8], sizingV="fill",
               **rule("Right"))
    fixed(cats, w=190)
    for c, icon in [("General", "settings"), ("Appearance", "sparkle"), ("Listener", "chat"),
                    ("Agents", "user_group"), ("Roles", "person"), ("Notifications", "bell"),
                    ("Terminal", "terminal"), ("Keys", "pencil")]:
        on = c == "Appearance"
        r = row(cats, c, gap=8, padding=[6, 10, 6, 10], cornerRadius=5, sizingH="fill",
                **({"fill": SEL} if on else {}))
        zicon(r, icon, TEXT if on else FAINT, 14)
        text(r, c, 13, TEXT if on else FAINT, style="Medium" if on else "Regular")
    form = col(main, "form", gap=0, padding=[8, 24, 8, 24], sizingH="fill")
    text(form, "Appearance", 18, BRIGHT, style="Semibold")
    ctl = setting(form, "Follow Zed", "Use the theme, icon theme and fonts from your Zed "
                                      "settings. Turn off to choose them here.")
    t = row(ctl, "toggle", gap=8)
    sw = frame(t, "switch", 30, 18, fill=ACCENT, cornerRadius=9, layout="horizontal", padding=2,
               crossAlign="center", mainAlign="max")
    cmd("node.create", {"type": "ellipse", "parent": sw, "width": 14, "height": 14,
                        "props": {"fill": "#ffffff"}})
    text(t, "On · Zed found at /Applications/Zed.app", 12, TEXT)
    ctl = setting(form, "Theme", "Any Zed theme: yours from ~/.config/zed/themes, or one from "
                                 "Zed's theme extensions.")
    r = row(ctl, "theme", gap=10)
    dropdown(r, "Gruvbox Material Dark Hard")
    seg(r, ["Light", "Dark", "System"], "System")
    text(ctl, "Browse Zed themes…", 12, ACCENT)
    ctl = setting(form, "Icon theme", "File and folder icons. Zed's installed icon themes, "
                                      "agmen's own (Lucide), or your system's.")
    dropdown(ctl, "Material Icon Theme (from Zed)")
    pv = row(ctl, "preview", gap=10, padding=[6, 10, 6, 10], cornerRadius=5, fill=BG)
    for n, is_dir in [("src", True), ("tests", True), ("main.rs", False), ("Cargo.toml", False),
                      ("README.md", False), ("Cargo.lock", False), ("reviewer.xml", False),
                      ("release.yml", False)]:
        ficon(pv, n, is_dir=is_dir, size=18)
    ctl = setting(form, "UI font", "Panels, chat text and menus.")
    r = row(ctl, "ui font", gap=10)
    dropdown(r, "SF Pro Text")
    stepper(r, "15")
    ctl = setting(form, "Code font", "Files, diffs, handles, branches and code in chat.")
    r = row(ctl, "buffer font", gap=10)
    dropdown(r, "CaskaydiaCove Nerd Font Mono", mono=True)
    stepper(r, "13")
    seg(r, ["Ligatures on", "off"], "Ligatures on")
    ctl = setting(form, "Terminal font", "Embedded terminals and take-over.")
    r = row(ctl, "terminal font", gap=10)
    dropdown(r, "Same as code font")
    stepper(r, "13")
    ctl = setting(form, "Chat density", "Spacing between messages.")
    seg(ctl, ["Comfortable", "Compact"], "Comfortable")
    wrap(form, "Saved to ~/.config/agmen/settings.toml. The terminal UI reads the same file; "
               "there, :set theme=… and :set icons=nerd|none change it, and the font is your "
               "terminal's.", 11, FAINT, paragraphSpacing=0)
    statusbar(s, "settings")
    link(tabs["chat"], "Desktop · chat")
    link(tabs["andy/fx1"], "Desktop · agent")

# ---------------------------------------------------------------- build

POS = {
    "Desktop · chat": (0, 0), "Desktop · agent": (1560, 0),
    "Desktop · chat + terminal": (3120, 0), "Desktop · switch agent": (7800, 0),
    "Desktop · chat + codex": (9360, 0), "Desktop · model picker": (10920, 0),
    "Desktop · ask": (12480, 0), "Desktop · listener collapsed": (14040, 0),
    "Desktop · project": (15600, 0), "Desktop · project (tests open)": (17160, 0),
    "Desktop · git": (18720, 0), "Desktop · git (collapsed)": (20280, 0),
    "Desktop · needs you": (21840, 0), "Terminal · needs you": (6240, 1060),
    "Desktop · needs you (answered)": (23400, 0), "Desktop · settings from chat": (24960, 0),
    "Desktop · add agent": (26520, 0), "Desktop · settings applied": (28080, 0),
    "Desktop · settings": (29640, 0),
    "Desktop · plan": (0, 2120), "Desktop · spec": (1560, 2120),
    "Desktop · plan ready": (3120, 2120), "Desktop · kick off": (4680, 2120),
    "Desktop · running": (6240, 2120), "Terminal · references": (7800, 1060),
    "Desktop · palette": (4680, 0), "Desktop · role file": (6240, 0),
    "Terminal · chat": (0, 1060), "Terminal · split": (1560, 1060),
    "Terminal · command": (3120, 1060), "Terminal · take-over": (4680, 1060),
}

build_d_chat("Desktop · chat")
build_d_agent("Desktop · agent")
build_d_chat("Desktop · chat + terminal", terminal="claude")
build_d_chat("Desktop · switch agent", terminal="claude", menu=True)
build_d_chat("Desktop · add agent", terminal="claude", menu=True, add_agent=True)
build_d_chat("Desktop · chat + codex", terminal="codex")
build_d_chat("Desktop · model picker", picker=True)
build_d_chat("Desktop · ask", ask=True)
build_d_chat("Desktop · listener collapsed", collapsed=True)
build_d_tree("Desktop · project", "Project")
build_d_tree("Desktop · project (tests open)", "Project", "tests")
build_d_tree("Desktop · git", "Git")
build_d_tree("Desktop · git (collapsed)", "Git", "collapsed")
build_d_chat("Desktop · needs you", alert=True)
build_d_chat("Desktop · needs you (answered)", answered=True)
build_d_chat("Desktop · settings from chat", settings="pending")
build_d_chat("Desktop · settings applied", settings="applied")
build_d_settings("Desktop · settings")
build_t_chat("Terminal · needs you", alert=True)
build_t_chat("Terminal · references", hints=True)
build_d_plan("Desktop · plan", "draft")
build_d_spec("Desktop · spec")
build_d_plan("Desktop · plan ready", "ready")
build_d_plan("Desktop · kick off", "confirm")
build_d_plan("Desktop · running", "running")
build_d_chat("Desktop · palette", palette=True)
build_d_role("Desktop · role file")
build_t_chat("Terminal · chat")
build_t_split("Terminal · split")
build_t_chat("Terminal · command", command=":spawn codex --role tester fix the flaky replay test")
build_t_takeover("Terminal · take-over")

for src, dest, key in links:
    if key and key.startswith("delay:"):
        trig = {"type": "afterDelay", "ms": int(key.split(":")[1])}
    else:
        trig = {"type": "keyPress", "key": key} if key else {"type": "onClick"}
    cmd("proto.interaction.add", {"id": src, "trigger": trig,
                                  "action": {"type": "navigate", "dest": screens[dest]}})

cmd("proto.flow.add", {"start": screens["Desktop · chat"], "name": "Desktop (Zed)"})
cmd("proto.flow.add", {"start": screens["Terminal · chat"], "name": "Terminal (vim)"})
cmd("proto.flow.add", {"start": screens["Desktop · needs you"], "name": "Needs you (desktop)"})
cmd("proto.flow.add", {"start": screens["Terminal · needs you"], "name": "Needs you (terminal)"})
cmd("proto.flow.add", {"start": screens["Desktop · plan"], "name": "Plan a greenfield project"})
cmd("proto.flow.add", {"start": screens["Desktop · settings from chat"],
                       "name": "Change settings from chat"})

json.dump(steps, open(sys.argv[1], "w"))
print(f"{len(steps)} steps, {len(links)} links", file=sys.stderr)
