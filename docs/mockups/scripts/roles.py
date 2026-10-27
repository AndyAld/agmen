"""Generate a FigCraft `run` script for the agmen roles prototype (desktop app, 1440x900)."""

import json
import sys

steps = []
links = []  # (src_ref, dest_name, transition)
screens = {}

UI = "SF Pro Text"
MONO = "Menlo"

BG = "#0f1115"
SIDEBAR = "#14171c"
PANEL = "#171a20"
SURF = "#1c2027"
SURF2 = "#242a34"
CODE = "#0b0d11"
BORDER = "#2a303a"
TEXT = "#e7e9ee"
MUTED = "#9aa3b2"
FAINT = "#636b7a"
ACCENT = "#5b8cff"
OK = "#4ade80"
WARN = "#fbbf24"
BAD = "#f87171"

ROLES = {
    "coordinator": ("Coordinator", "#a78bfa", "Assigns items, settles disputes"),
    "planner": ("Planner", "#38bdf8", "Splits a goal into checked items"),
    "researcher": ("Researcher", "#2dd4bf", "Answers questions with quoted sources"),
    "engineer": ("Engineer", "#f59e0b", "Builds an item, passes the gate"),
    "tester": ("Tester", "#a3e635", "Writes and runs tests, files failures"),
    "reviewer": ("Reviewer", "#f472b6", "Reviews a DONE and reports findings"),
    "security-reviewer": ("Security Reviewer", "#f472b6", "Reviews auth, token and ACL changes"),
    "docs-writer": ("Docs Writer", "#f59e0b", "Writes docs for a finished item"),
    "dispatcher": ("Dispatcher", "#94a3b8", "Watches the chat, delegates work"),
}
CUSTOM = {"security-reviewer", "docs-writer"}
STATUS = {"working": OK, "idle": FAINT, "starting": ACCENT, "reading": ACCENT, "stopped": BAD}


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


def text(parent, s, size=13, color=TEXT, font=UI, style="Regular", width=None, **props):
    pr = {"fontFamily": font, "fontStyle": style, "fontSize": size, "fill": color}
    pr.update(props)
    p = {"text": s, "x": 0, "y": 0, "parent": parent, "props": pr}
    if width is not None:
        p["width"] = width
        pr.setdefault("textAutoResize", "height")
    return cmd("text.create", p)


def wrap(parent, s, size=13, color=TEXT, **kw):
    """Text that fills its auto layout parent's width and wraps."""
    return text(parent, s, size, color, width=100, sizingH="fill", **kw)


def dot(parent, color, d=8):
    return cmd("node.create", {"type": "ellipse", "parent": parent, "width": d, "height": d,
                               "props": {"fill": color}})


def bottom_rule(**props):
    props.update(stroke=BORDER, strokeTopWeight=0, strokeRightWeight=0, strokeLeftWeight=0,
                 strokeBottomWeight=1)
    return props


def badge(parent, key, size=12):
    name, color, _ = ROLES[key]
    extra = {}
    if key in CUSTOM:
        extra = {"stroke": color, "strokeWeight": 1, "dashes": [3, 2]}
    b = row(parent, f"badge {name}", gap=6, padding=[3, 8, 3, 8], cornerRadius=4,
            fill=color + "24", **extra)
    dot(b, color, 6)
    text(b, name, size, color, style="Medium")
    return b


def tag(parent, s, color=MUTED, fill=SURF2):
    t = row(parent, f"tag {s}", padding=[2, 7, 2, 7], cornerRadius=4, fill=fill)
    text(t, s, 11, color, style="Medium")
    return t


def button(parent, label, kind="secondary"):
    styles = {
        "primary": dict(fill=ACCENT),
        "secondary": dict(fill=SURF2, stroke=BORDER, strokeWeight=1),
        "danger": dict(fill=SURF2, stroke=BAD + "80", strokeWeight=1),
        "ghost": dict(),
    }
    colors = {"primary": "#ffffff", "secondary": TEXT, "danger": BAD, "ghost": ACCENT}
    b = row(parent, f"button {label}", padding=[7, 14, 7, 14], cornerRadius=6,
            mainAlign="center", **styles[kind])
    text(b, label, 13, colors[kind], style="Medium")
    return b


def card(parent, title=None, **props):
    props.setdefault("sizingH", "fill")
    c = col(parent, f"card {title or ''}".strip(), gap=10, padding=16, cornerRadius=8, fill=PANEL,
            stroke=BORDER, strokeWeight=1, **props)
    if title:
        text(c, title, 11, FAINT, style="Semibold", textCase="upper", letterSpacing="6%")
    return c


def kv(parent, label, value, color=TEXT, mono=False, label_w=104):
    r = row(parent, f"kv {label}", gap=12, sizingH="fill", crossAlign="min")
    text(r, label, 12, MUTED, width=label_w)
    wrap(r, value, 12, color, font=MONO if mono else UI)
    return r


def link(src, dest, transition="dissolve"):
    links.append((src, dest, transition))


# ---------------------------------------------------------------- shell

POS = {
    "Agents": (0, 0), "Launch worker": (1560, 0), "Agents · launched": (3120, 0),
    "Agent · andy/w3f": (0, 1060), "Worktrees": (1560, 1060), "Worktrees · pruned": (3120, 1060),
    "Roles · Reviewer": (0, 2120), "Roles · Engineer": (1560, 2120),
    "Roles · Security Reviewer": (3120, 2120),
    "Chat": (0, 3180), "Chat · thread": (1560, 3180), "Chat · mention": (3120, 3180),
    "Chat · ledger": (4680, 3180),
}
NAV = ["Agents", "Chat", "Board", "Roles", "Worktrees", "DMs", "Alerts", "Settings"]
NAV_DEST = {"Agents": "Agents", "Chat": "Chat", "Roles": "Roles · Reviewer", "Worktrees": "Worktrees"}
NAV_COUNT = {"Alerts": "2", "Worktrees": "2", "DMs": "1"}


def screen(name, active):
    x, y = POS[name]
    s = frame(None, name, 1440, 900, x, y, fill=BG, layout="horizontal", clip=True)
    screens[name] = s
    side = col(s, "sidebar", gap=2, padding=[18, 12, 16, 12], fill=SIDEBAR, sizingH="fixed",
               sizingV="fill", stroke=BORDER, strokeTopWeight=0, strokeBottomWeight=0,
               strokeLeftWeight=0, strokeRightWeight=1)
    cmd("node.set", {"id": side, "width": 232})
    brand = col(side, "brand", gap=2, padding=[0, 8, 14, 8], sizingH="fill")
    text(brand, "agmen", 17, TEXT, style="Semibold")
    text(brand, "a 17-agent PR", 12, MUTED)
    for item in NAV:
        on = item == active
        it = row(side, f"nav {item}", gap=8, padding=[7, 10, 7, 10], cornerRadius=6,
                 sizingH="fill", mainAlign="spaceBetween", **({"fill": SURF2} if on else {}))
        text(it, item, 13, TEXT if on else MUTED, style="Medium" if on else "Regular")
        if item in NAV_COUNT:
            tag(it, NAV_COUNT[item], WARN if item != "DMs" else ACCENT,
                (WARN if item != "DMs" else ACCENT) + "22")
        if item in NAV_DEST and not on:
            link(it, NAV_DEST[item], "instant")
    frame(side, "spacer", sizingH="fill", sizingV="fill", layout="vertical")
    me = row(side, "me", gap=8, padding=[8, 10, 0, 10])
    dot(me, OK, 7)
    text(me, "andy · agmend connected", 12, MUTED)
    main = col(s, "main", gap=18, padding=[26, 32, 26, 32], sizingH="fill", sizingV="fill")
    return s, main


def header(main, title, subtitle, buttons=()):
    h = row(main, "header", gap=16, sizingH="fill", mainAlign="spaceBetween", crossAlign="min")
    left = col(h, "titles", gap=4, sizingH="fill")
    text(left, title, 22, TEXT, style="Semibold")
    wrap(left, subtitle, 13, MUTED)
    right = row(h, "actions", gap=8)
    refs = {}
    for label, kind in buttons:
        refs[label] = button(right, label, kind)
    return refs


def banner(main, msg, color):
    b = row(main, "banner", gap=10, padding=[10, 14, 10, 14], cornerRadius=8, sizingH="fill",
            fill=color + "18", stroke=color + "55", strokeWeight=1)
    dot(b, color, 8)
    wrap(b, msg, 13, TEXT)
    return b


# ---------------------------------------------------------------- Agents

AG_COLS = [("Agent", 230), ("Role", 176), ("Model", 120), ("Item · lane", 150), ("Status", 128),
           ("Spend", 112), ("Worktree", None)]

AGENT_ROWS = [
    ("group", "andy (you) · 5 agents"),
    (0, "andy/dispatch", None, "dispatcher", "haiku-5-5", "chat", "idle", "$0.42 / 2.00", "none"),
    (1, "andy/w3f", None, "engineer", "sonnet", "B-412 · L-UI", "working", "$3.10 / 5.00",
     "fix/b-412-map-truncation"),
    (1, "andy/t7k", None, "tester", "sonnet", "B-415 · L-TESTS", "working", "$1.05 / 3.00",
     "test/b-415-replay-paging"),
    (1, "andy/rv2", None, "reviewer", "opus", "B-409", "reading", "$0.80 / 4.00",
     "detached @2b5aa0d"),
    (0, "andy/sol", "interactive", "engineer", "Claude Code", "B-401 · L-HUB", "idle",
     "alerts only", "fix/b-401-ack-timeout"),
    ("group", "adam · 4 agents"),
    (0, "adam/coord", None, "coordinator", "sonnet", "board", "working", "$2.40", "none"),
    (0, "adam/plan", None, "planner", "opus", "B-420 · L-DOCS", "idle", "$1.90",
     "docs/b-420-m2-plan"),
    (0, "adam/res", None, "researcher", "sonnet", "ASK #1841", "working", "$0.35", "none"),
    (0, "adam/sec", None, "security-reviewer", "opus", "B-407", "reading", "$0.60",
     "detached @9c41e02"),
]
LAUNCHED_ROW = (0, "andy/k2m", "new", "engineer", "sonnet", "B-418 · L-UI", "starting",
                "$0.00 / 5.00", "fix/b-418-lane-name-truncation")


def agents_table(main, launched):
    t = col(main, "agents table", gap=0, sizingH="fill", cornerRadius=8, fill=PANEL, stroke=BORDER,
            strokeWeight=1, clip=True)
    hr = row(t, "head", gap=0, padding=[0, 16, 0, 16], sizingH="fill", fill=SURF, **bottom_rule())
    cmd("node.set", {"id": hr, "sizingV": "fixed", "height": 34})
    for label, w in AG_COLS:
        if w:
            text(hr, label, 12, FAINT, style="Semibold", width=w)
        else:
            wrap(hr, label, 12, FAINT, style="Semibold")
    rows = list(AGENT_ROWS)
    if launched:
        rows.insert(6, LAUNCHED_ROW)
    refs = {}
    for r in rows:
        if r[0] == "group":
            g = row(t, "group", padding=[9, 16, 7, 16], sizingH="fill", fill=BG, **bottom_rule())
            label = r[1].replace("5 agents", "6 agents") if launched else r[1]
            text(g, label, 12, MUTED, style="Semibold")
            continue
        indent, handle, note, role, model, item, status, spend, wt = r
        hl = {"fill": ACCENT + "14"} if note == "new" else {}
        rr = row(t, f"row {handle}", gap=0, padding=[0, 16, 0, 16], sizingH="fill",
                 **bottom_rule(**hl))
        cmd("node.set", {"id": rr, "sizingV": "fixed", "height": 44})
        a = row(rr, "agent", gap=6, sizingH="fixed")
        cmd("node.set", {"id": a, "width": AG_COLS[0][1]})
        if indent:
            text(a, "↳", 13, FAINT)
        text(a, handle, 13, TEXT, font=MONO)
        if note:
            tag(a, note, ACCENT if note == "new" else MUTED)
        rc = row(rr, "role", sizingH="fixed")
        cmd("node.set", {"id": rc, "width": AG_COLS[1][1]})
        badge(rc, role)
        text(rr, model, 13, MUTED, width=AG_COLS[2][1])
        text(rr, item, 12, TEXT, font=MONO, width=AG_COLS[3][1])
        sc = row(rr, "status", gap=7, sizingH="fixed")
        cmd("node.set", {"id": sc, "width": AG_COLS[4][1]})
        dot(sc, STATUS[status], 7)
        text(sc, status, 13, TEXT)
        text(rr, spend, 13, MUTED, width=AG_COLS[5][1])
        wrap(rr, wt, 12, FAINT if wt == "none" else MUTED, font=MONO)
        refs[handle] = rr
    return refs


def build_agents(name, launched=False, modal=False):
    s, main = screen(name, "Agents")
    b = header(main, "Agents",
               ("11 agents, 8 online. " if launched else "10 agents, 7 online. ") +
               "Each agent's role sets its tools at launch, the verbs the "
               "hub accepts from it, and the tools agmen mcp lists.",
               [("Launch worker", "primary")])
    chips = row(main, "role filter", gap=6)
    text(chips, "Roles", 12, FAINT, style="Semibold")
    tag(chips, "All", TEXT, SURF2)
    for k in ["coordinator", "planner", "researcher", "engineer", "tester", "reviewer",
              "security-reviewer"]:
        badge(chips, k, 11)
    if launched:
        banner(main, "Launched andy/k2m as Engineer on B-418. agmend created the worktree on "
                     "fix/b-418-lane-name-truncation from origin/main and passed it engineer.xml.",
               OK)
    refs = agents_table(main, launched)
    link(refs["andy/w3f"], "Agent · andy/w3f")
    if not modal:
        link(b["Launch worker"], "Launch worker")
    return s


# ---------------------------------------------------------------- Launch modal

def field(parent, label, value, note=None, mono=False):
    f = col(parent, f"field {label}", gap=5, sizingH="fill")
    text(f, label, 12, MUTED, style="Medium")
    box = row(f, "input", padding=[8, 10, 8, 10], cornerRadius=6, fill=SURF2, stroke=BORDER,
              strokeWeight=1, sizingH="fill", mainAlign="spaceBetween")
    text(box, value, 13, TEXT, font=MONO if mono else UI)
    if note:
        text(box, note, 11, FAINT)
    return f


def build_launch():
    s = build_agents("Launch worker", modal=True)
    frame(s, "dim", 1440, 900, 0, 0, fill="#000000a8", absolute=True)
    m = col(s, "modal", gap=16, padding=24, cornerRadius=12, fill=PANEL, stroke=BORDER,
            strokeWeight=1, absolute=True, x=330, y=88, sizingH="fixed")
    cmd("node.set", {"id": m, "width": 780})
    top = row(m, "top", sizingH="fill", mainAlign="spaceBetween")
    text(top, "Launch worker", 18, TEXT, style="Semibold")
    close = text(top, "×", 22, MUTED)
    text(m, "Role", 12, MUTED, style="Medium")
    grid = col(m, "role grid", gap=8, sizingH="fill")
    order = [["coordinator", "planner", "researcher"], ["engineer", "tester", "reviewer"],
             ["security-reviewer", "docs-writer", None]]
    for line in order:
        r = row(grid, "role row", gap=8, sizingH="fill")
        for k in line:
            if k is None:
                c = col(r, "new role", gap=3, padding=[10, 12, 10, 12], cornerRadius=8,
                        sizingH="fill", stroke=BORDER, strokeWeight=1, dashes=[4, 3])
                text(c, "+ New role", 13, MUTED, style="Medium")
                text(c, "Copy a role into .agmen/roles", 11, FAINT)
                continue
            name, color, job = ROLES[k]
            sel = k == "engineer"
            st = dict(stroke=ACCENT, strokeWeight=2, fill=ACCENT + "14") if sel else dict(
                stroke=BORDER, strokeWeight=1, fill=SURF)
            if k in CUSTOM:
                st["dashes"] = [4, 3]
            c = col(r, f"role {name}", gap=3, padding=[10, 12, 10, 12], cornerRadius=8,
                    sizingH="fill", **st)
            h = row(c, "name", gap=7, sizingH="fill", mainAlign="spaceBetween")
            nm = row(h, "n", gap=7)
            dot(nm, color, 8)
            text(nm, name, 13, TEXT, style="Semibold")
            if k == "security-reviewer":
                tag(h, "project")
            elif k == "docs-writer":
                tag(h, "personal")
            wrap(c, job, 11, MUTED)
    r1 = row(m, "fields 1", gap=12, sizingH="fill")
    field(r1, "Item", "B-418  Truncate long lane names on the board")
    r2 = row(m, "fields 2", gap=12, sizingH="fill")
    field(r2, "Lane", "L-UI", mono=True)
    field(r2, "Tier", "standard · sonnet", "role default")
    field(r2, "Budget", "$5.00")
    r3 = row(m, "fields 3", gap=12, sizingH="fill")
    field(r3, "Branch", "fix/b-418-lane-name-truncation", "from item", mono=True)
    field(r3, "Base", "origin/main", mono=True)
    sm = col(m, "summary", gap=6, padding=[12, 14, 12, 14], cornerRadius=8, sizingH="fill",
             fill=ACCENT + "12", stroke=ACCENT + "44", strokeWeight=1)
    text(sm, "What this worker can do", 12, ACCENT, style="Semibold")
    wrap(sm, "Edit files in L-UI, claim B-418, post DONE, and push on its own (push approval is "
             "off). It cannot assign work, rule, or review: the hub refuses ASSIGN, RULING and "
             "REVIEW from it.", 12, TEXT)
    wrap(sm, "agmend creates a worktree on fix/b-418-lane-name-truncation and removes it, with "
             "its local branch, after the branch merges into main.", 12, MUTED)
    foot = row(m, "footer", gap=8, sizingH="fill", mainAlign="max")
    cancel = button(foot, "Cancel")
    go = button(foot, "Launch worker", "primary")
    link(close, "Agents")
    link(cancel, "Agents")
    link(go, "Agents · launched")


# ---------------------------------------------------------------- Agent detail

def build_agent():
    s, main = screen("Agent · andy/w3f", "Agents")
    back = text(main, "← Agents", 13, ACCENT)
    link(back, "Agents")
    h = row(main, "header", gap=16, sizingH="fill", mainAlign="spaceBetween")
    left = row(h, "who", gap=10)
    text(left, "andy/w3f", 20, TEXT, font=MONO, style="Bold")
    badge(left, "engineer")
    st = row(left, "status", gap=6)
    dot(st, OK, 7)
    text(st, "working", 13, TEXT)
    text(left, "delegated by andy/dispatch · sonnet · B-412 · L-UI", 13, MUTED)
    right = row(h, "actions", gap=8)
    button(right, "Open worktree")
    button(right, "Add budget")
    button(right, "Stop", "danger")

    body = row(main, "body", gap=16, sizingH="fill", sizingV="fill", crossAlign="min")
    act = col(body, "activity column", gap=12, sizingH="fill", sizingV="fill")
    feed = card(act, "Live session", sizingV="fill")

    def say(s):
        wrap(feed, s, 13, TEXT)

    def tool(name, arg, result=None):
        r = row(feed, f"tool {name}", gap=10, padding=[7, 10, 7, 10], cornerRadius=6, fill=SURF,
                sizingH="fill")
        text(r, "▸", 11, FAINT)
        text(r, name, 12, ACCENT, font=MONO, style="Bold")
        wrap(r, arg, 12, MUTED, font=MONO)
        if result:
            text(r, result, 12, OK, font=MONO)

    def cost(s):
        text(feed, s, 11, FAINT)

    say("Claimed B-412. Reading the truncation helper before changing it.")
    tool("Read", "ui/src/components/flow/utils/readMapTruncation.ts")
    tool("Grep", "\"truncat\" ui/src --type ts", "6 files")
    cost("turn 5 · $0.18 · 21 s")
    say("The notice drops the total. Adding a failing spec first, then the fix.")
    tool("Edit", "ui/src/components/flow/utils/readMapTruncation.test.ts", "+22")
    tool("Bash", "pnpm jest readMapTruncation", "1 failed")
    tool("Edit", "ui/src/components/flow/utils/readMapTruncation.ts", "+14 -3")
    tool("Bash", "pnpm jest", "739 passed, 0 failed")
    cost("turn 6 · $0.34 · 52 s")
    dm = row(feed, "dm", gap=10, padding=[8, 10, 8, 10], cornerRadius=6, sizingH="fill",
             fill=ACCENT + "14", stroke=ACCENT + "44", strokeWeight=1)
    text(dm, "DM #1855 from andy", 12, ACCENT, style="Semibold")
    wrap(dm, "also cover the empty map case", 12, TEXT)
    tag(dm, "in session", OK, OK + "22")
    say("Covering the empty map case too, then committing and pushing. Push approval is off "
        "for this project.")
    tool("Bash", "git push -u origin fix/b-412-map-truncation", "ok")
    cost("turn 7 · $0.21 · 38 s")

    box = row(act, "message box", gap=10, padding=[10, 10, 10, 14], cornerRadius=8, fill=SURF,
              stroke=BORDER, strokeWeight=1, sizingH="fill")
    wrap(box, "Message andy/w3f. Sent as a DM: the hub records it and agmend writes it to the "
              "worker's stdin.", 13, FAINT)
    button(box, "Send", "primary")

    side = col(body, "side", gap=12, sizingH="fixed", sizingV="hug")
    cmd("node.set", {"id": side, "width": 380})
    rc = card(side, "Role")
    r = row(rc, "role", gap=8, sizingH="fill", mainAlign="spaceBetween")
    rl = row(r, "l", gap=8)
    badge(rl, "engineer")
    tag(rl, "built-in")
    view = text(r, "View role →", 12, ACCENT, style="Medium")
    link(view, "Roles · Engineer")
    kv(rc, "Prompt", "base.xml + engineer.xml", mono=True)
    kv(rc, "File hash", "sha256 3f2a91c0, stored by the hub at mint", MUTED)

    ec = card(side, "Enforced by agmen")
    kv(ec, "Tools", "Read, Edit, Write, Bash, Grep, Glob")
    kv(ec, "Edits", "Lane L-UI only (lane guard)")
    kv(ec, "Verbs", "CLAIM, HEARTBEAT, RELEASE, DONE, PUSH_REQUEST, FOUND, chat")
    kv(ec, "Refused", "ASSIGN, RULING, LEND, REVIEW", BAD)
    kv(ec, "Push approval", "off (project default)", MUTED)

    wc = card(side, "Worktree")
    kv(wc, "Branch", "fix/b-412-map-truncation", mono=True)
    kv(wc, "Base", "origin/main @0204a40", mono=True)
    kv(wc, "State", "3 commits, pushed, clean", OK)
    kv(wc, "Cleanup", "agmend removes the worktree and the local branch after the branch "
                      "merges into main. It never forces.", MUTED)

    bc = card(side, "Budget")
    bar = frame(bc, "bar", 348, 6, fill=SURF2, cornerRadius=3, sizingH="fill")
    frame(bar, "used", 216, 6, 0, 0, fill=ACCENT, cornerRadius=3)
    kv(bc, "Spent", "$3.10 of $5.00. Warns at 80%, wraps up at 95%, stops at 100%.")


# ---------------------------------------------------------------- Roles

XML = {
    "reviewer": [
        '<role name="reviewer" extends="base" kind="agent" tier="deep">',
        "  <purpose>",
        "    Review one DONE: read its diff, trace each path it",
        "    changes, and report findings. You review; you do not fix.",
        "  </purpose>",
        '  <tools allow="Read Grep Glob Bash" />',
        '  <verbs allow="REVIEW ASK ANSWER FYI DM" />',
        '  <worktree mode="detached" at="done.commit"',
        '            remove="after-review" />',
        "  <rules>",
        "    <rule>Scope the review by execution paths, not by",
        "          changed files.</rule>",
        "    <rule>Give each finding path:line, what breaks, and",
        "          the input that triggers it. Worst first.</rule>",
        "    <rule>Say which paths you traced and which you left.</rule>",
        "    <rule>In Bash, run only commands that write nothing.</rule>",
        "    <rule>If nothing is wrong, say so in one line.</rule>",
        "  </rules>",
        "  <done-when>You posted one REVIEW whose reply_to is",
        "    the DONE.</done-when>",
        "  <report>REVIEW {verdict: pass | changes, findings}</report>",
        "</role>",
    ],
    "engineer": [
        '<role name="engineer" extends="base" kind="agent"',
        '      tier="standard">',
        "  <purpose>",
        "    Take one claimed item from claim to DONE: build it in your",
        "    worktree, pass the gate, commit, and push.",
        "  </purpose>",
        '  <tools allow="Read Edit Write Bash Grep Glob" />',
        '  <verbs allow="CLAIM HEARTBEAT RELEASE DONE PUSH_REQUEST FOUND',
        '                ASK ANSWER FYI DM" />',
        '  <worktree mode="branch" name="{type}/{item}-{slug}"',
        '            remove="after-merge" />',
        "  <rules>",
        "    <rule>Claim before you edit. Edit only in your lanes.</rule>",
        "    <rule>Reuse before you create: search for an existing helper",
        "          and say what you searched and what you found.</rule>",
        "    <rule>Start each change with a failing test where the",
        "          repo has tests.</rule>",
        "    <rule>Never weaken, skip, or delete a test to make it",
        "          pass.</rule>",
        "    <rule>Run the repo's gate before DONE and put its",
        "          output in checks.</rule>",
        "  </rules>",
        "  <done-when>The gate passes, the branch is pushed, and",
        "    DONE lists checks and changes.</done-when>",
        "  <report>DONE {item, branch, commit, base_sha, checks,",
        "    changes}</report>",
        "</role>",
    ],
    "security-reviewer": [
        "<!-- .agmen/roles/security-reviewer.xml -->",
        '<role name="security-reviewer" extends="reviewer">',
        "  <purpose>",
        "    Review DONEs that touch auth, tokens, or broker ACLs.",
        "  </purpose>",
        '  <tools allow="Read Grep Glob" />',
        "  <rules>",
        "    <rule>Check every new route for the token scope it",
        "          requires.</rule>",
        "    <rule>Treat any secret-shaped string in a diff as a",
        "          finding.</rule>",
        "    <rule>Try ids containing +, # and / against every",
        "          topic builder.</rule>",
        "  </rules>",
        "</role>",
    ],
}

RIGHTS = {
    "reviewer": [
        ("Source", "Built-in, ships with agmen 0.1.0", MUTED),
        ("Identity kind", "agent", TEXT),
        ("Tools at launch", "Read, Grep, Glob, Bash", TEXT),
        ("Bash", "Read-only by rule. Not enforced: --tools cannot limit Bash commands.", WARN),
        ("Edits", "None. Edit and Write are not passed to the worker.", TEXT),
        ("Worktree", "Detached at the DONE's commit; removed after REVIEW", TEXT),
        ("Verbs allowed", "REVIEW, ASK, ANSWER, FYI, DM", OK),
        ("Verbs refused", "CLAIM, DONE, PUSH_REQUEST, ASSIGN, RULING", BAD),
        ("Default tier", "deep · opus", TEXT),
    ],
    "engineer": [
        ("Source", "Built-in, ships with agmen 0.1.0", MUTED),
        ("Identity kind", "agent", TEXT),
        ("Tools at launch", "Read, Edit, Write, Bash, Grep, Glob", TEXT),
        ("Edits", "Only inside claimed or lent lanes (lane guard, DONE check)", TEXT),
        ("Worktree", "Own branch {type}/{item}-{slug}; removed after merge", TEXT),
        ("Verbs allowed", "CLAIM, HEARTBEAT, RELEASE, DONE, PUSH_REQUEST, FOUND, chat", OK),
        ("Verbs refused", "ASSIGN, RULING, LEND, REVIEW", BAD),
        ("Default tier", "standard · sonnet", TEXT),
    ],
    "security-reviewer": [
        ("Source", "Project: .agmen/roles/security-reviewer.xml", MUTED),
        ("Extends", "reviewer", TEXT),
        ("Narrowed", "Bash removed. Verbs and worktree as Reviewer.", WARN),
        ("Tools at launch", "Read, Grep, Glob", TEXT),
        ("Edits", "None", TEXT),
        ("Verbs allowed", "REVIEW, ASK, ANSWER, FYI, DM", OK),
        ("Verbs refused", "CLAIM, DONE, PUSH_REQUEST, ASSIGN, RULING", BAD),
        ("Default tier", "deep · opus (from reviewer)", TEXT),
    ],
}

ROLE_SCREEN = {"reviewer": "Roles · Reviewer", "engineer": "Roles · Engineer",
               "security-reviewer": "Roles · Security Reviewer"}


def code_line_color(line):
    s = line.strip()
    if s.startswith("<!--"):
        return FAINT
    if s.startswith(("<rule>", "<done-when>", "<report>")) or "</" in s and not s.startswith("</"):
        return "#c9d1d9"
    if s.startswith("<") or (s.endswith(">") and "</" not in s):
        return "#7aa2ff"
    return "#c9d1d9"


def build_roles(sel):
    name = ROLE_SCREEN[sel]
    s, main = screen(name, "Roles")
    header(main, "Roles",
           "Built-in roles ship with agmen. Project roles come from .agmen/roles in the repo and "
           "override a built-in of the same name. Personal roles come from ~/.config/agmen/roles. "
           "A role can only narrow what its identity kind allows.",
           [("New role", "primary")])
    body = row(main, "body", gap=16, sizingH="fill", sizingV="fill", crossAlign="min")
    lst = col(body, "list", gap=2, padding=[12, 8, 12, 8], cornerRadius=8, fill=PANEL,
              stroke=BORDER, strokeWeight=1, sizingH="fixed", sizingV="fill")
    cmd("node.set", {"id": lst, "width": 250})
    groups = [
        ("Built-in", [("base", "Base rules", "all roles"), ("coordinator", None, "standard"),
                      ("planner", None, "deep"), ("researcher", None, "standard"),
                      ("engineer", None, "standard"), ("tester", None, "standard"),
                      ("reviewer", None, "deep")]),
        ("Project · .agmen/roles", [("security-reviewer", None, "deep")]),
        ("Personal · ~/.config/agmen/roles", [("docs-writer", None, "standard")]),
    ]
    for gname, items in groups:
        text(lst, gname, 11, FAINT, style="Semibold", textCase="upper", letterSpacing="6%",
             paragraphSpacing=0)
        for key, label, note in items:
            on = key == sel
            it = row(lst, f"item {key}", gap=8, padding=[8, 10, 8, 10], cornerRadius=6,
                     sizingH="fill", mainAlign="spaceBetween", **({"fill": SURF2} if on else {}))
            l = row(it, "l", gap=8)
            color = ROLES[key][1] if key in ROLES else FAINT
            dot(l, color, 8)
            text(l, label or ROLES[key][0], 13, TEXT if on else MUTED,
                 style="Medium" if on else "Regular")
            text(it, note, 11, FAINT)
            if key in ROLE_SCREEN and not on:
                link(it, ROLE_SCREEN[key], "instant")
        frame(lst, "gap", 10, 10)

    det = col(body, "detail", gap=14, sizingH="fill", sizingV="fill")
    top = row(det, "top", gap=10, sizingH="fill", mainAlign="spaceBetween")
    tl = row(top, "title", gap=10)
    text(tl, ROLES[sel][0], 20, TEXT, style="Semibold")
    badge(tl, sel)
    tr = row(top, "actions", gap=8)
    if sel in CUSTOM:
        button(tr, "Open file")
        button(tr, "Edit")
    else:
        button(tr, "Duplicate as personal role")
        button(tr, "Duplicate as project role")
    wrap(det, ROLES[sel][2] + ".", 13, MUTED)
    two = row(det, "two", gap=16, sizingH="fill", sizingV="fill", crossAlign="min")
    rc = card(two, "What agmen enforces", sizingH="fixed")
    cmd("node.set", {"id": rc, "width": 330})
    for label, value, color in RIGHTS[sel]:
        kv(rc, label, value, color)
    wrap(rc, "Checked in three places: the tools agmend passes at launch, the verbs the hub "
             "accepts from the identity, and the tools agmen mcp lists.", 11, FAINT)
    xc = col(two, "xml", gap=0, padding=0, cornerRadius=8, fill=CODE, stroke=BORDER,
             strokeWeight=1, sizingH="fill", sizingV="fill", clip=True)
    xh = row(xc, "xml head", padding=[10, 14, 10, 14], sizingH="fill", mainAlign="spaceBetween",
             fill=SURF, **bottom_rule())
    fname = {"reviewer": "reviewer.xml", "engineer": "engineer.xml",
             "security-reviewer": "security-reviewer.xml"}[sel]
    text(xh, fname, 12, MUTED, font=MONO, style="Bold")
    text(xh, "after " + ("base.xml, reviewer.xml" if sel in CUSTOM else "base.xml"), 11, FAINT)
    code = col(xc, "code", gap=3, padding=[14, 16, 14, 16], sizingH="fill")
    for i, line in enumerate(XML[sel], 1):
        lr = row(code, "line", gap=14)
        text(lr, f"{i:>2}", 11, "#3b4250", font=MONO)
        text(lr, line, 11, code_line_color(line), font=MONO)


# ---------------------------------------------------------------- Worktrees

WT_COLS = [("Branch", 236), ("Agent", 110), ("Role", 150), ("Item", 64), ("State", 214),
           ("Cleanup", None)]
WT_ROWS = [
    ("fix/b-412-map-truncation", "andy/w3f", "engineer", "B-412", ("active · 3 commits, pushed", OK),
     ("After it merges into main", MUTED), None),
    ("test/b-415-replay-paging", "andy/t7k", "tester", "B-415", ("active · 1 commit, not pushed", OK),
     ("After it merges into main", MUTED), None),
    ("detached @2b5aa0d", "andy/rv2", "reviewer", "B-409", ("reading", ACCENT),
     ("After REVIEW is posted", MUTED), None),
    ("fix/b-398-claim-race", "andy/w1a", "engineer", "B-398", ("merged into main 4 min ago", ACCENT),
     ("Next sweep removes worktree and branch", TEXT), "merged"),
    ("feat/b-402-lend-ui", "andy/w2c", "engineer", "B-402", ("stopped · 2 files uncommitted", WARN),
     ("Kept: saved to a wip ref, ALERT sent", WARN), "keep"),
    ("docs/b-390-m1-notes", "andy/p1", "planner", "B-390", ("stopped 9 days · not merged", WARN),
     ("Kept: unmerged, needs you", WARN), "decide"),
]


def build_worktrees(name, pruned):
    s, main = screen(name, "Worktrees")
    b = header(main, "Worktrees",
               "On this machine, project pr-3331. Every agent that edits works in its own "
               "worktree on its own branch, named {type}/{item}-{slug}.",
               [("Prune now", "primary")])
    if pruned:
        banner(main, "Removed 1 worktree and its local branch fix/b-398-claim-race. Kept 2 that "
                     "need you: one has uncommitted work, one never merged.", OK)
    else:
        banner(main, "agmend removes a worktree and its local branch once the branch merges, with "
                     "git worktree remove and git branch -d. It never forces: a worktree with "
                     "uncommitted work or an unmerged branch stays, and you get an ALERT.", ACCENT)
    t = col(main, "table", gap=0, sizingH="fill", cornerRadius=8, fill=PANEL, stroke=BORDER,
            strokeWeight=1, clip=True)
    hr = row(t, "head", gap=0, padding=[0, 16, 0, 16], sizingH="fill", fill=SURF, **bottom_rule())
    cmd("node.set", {"id": hr, "sizingV": "fixed", "height": 34})
    for label, w in WT_COLS:
        if w:
            text(hr, label, 12, FAINT, style="Semibold", width=w)
        else:
            wrap(hr, label, 12, FAINT, style="Semibold")
    for br, agent, role, item, (state, sc), (clean, cc), kind in WT_ROWS:
        if pruned and kind == "merged":
            continue
        rr = row(t, f"row {br}", gap=0, padding=[0, 16, 0, 16], sizingH="fill", **bottom_rule())
        cmd("node.set", {"id": rr, "sizingV": "fixed", "height": 48})
        text(rr, br, 12, TEXT, font=MONO, width=WT_COLS[0][1])
        text(rr, agent, 12, MUTED, font=MONO, width=WT_COLS[1][1])
        rc = row(rr, "role", sizingH="fixed")
        cmd("node.set", {"id": rc, "width": WT_COLS[2][1]})
        badge(rc, role)
        text(rr, item, 12, TEXT, font=MONO, width=WT_COLS[3][1])
        st = row(rr, "state", gap=7, sizingH="fixed")
        cmd("node.set", {"id": st, "width": WT_COLS[4][1]})
        dot(st, sc, 7)
        text(st, state, 12, TEXT)
        cl = row(rr, "cleanup", gap=8, sizingH="fill", mainAlign="spaceBetween")
        wrap(cl, clean, 12, cc)
        if kind == "decide":
            button(cl, "Remove…", "danger")
        elif kind == "keep":
            button(cl, "Open")
    if not pruned:
        link(b["Prune now"], "Worktrees · pruned")


# ---------------------------------------------------------------- Chat

def avatar(parent, role, handle):
    if role:
        name, color, _ = ROLES[role]
        a = frame(parent, "avatar", 30, 30, fill=color + "2a", cornerRadius=8, layout="horizontal",
                  mainAlign="center", crossAlign="center")
        text(a, name[0], 13, color, style="Semibold")
    else:
        a = frame(parent, "avatar", 30, 30, fill="#3a4150", cornerRadius=15, layout="horizontal",
                  mainAlign="center", crossAlign="center")
        text(a, handle[0].upper(), 13, TEXT, style="Semibold")
    return a


VERB_COLOR = {"DONE": OK, "ALERT": BAD, "REVIEW": ROLES["reviewer"][1]}


def message(parent, seq, handle, role, time, body=None, verb=None, quote=None, highlight=False):
    extra = {}
    if highlight:
        extra = dict(fill=WARN + "12", stroke=WARN, strokeTopWeight=0, strokeRightWeight=0,
                     strokeBottomWeight=0, strokeLeftWeight=2)
    m = row(parent, f"msg #{seq}", gap=12, padding=[6, 12, 6, 12], sizingH="fill",
            crossAlign="min", **extra)
    avatar(m, role, handle)
    c = col(m, "content", gap=5, sizingH="fill")
    h = row(c, "head", gap=8)
    name = text(h, handle, 13, TEXT, font=MONO, style="Bold")
    if role:
        badge(h, role, 11)
    else:
        tag(h, "human")
    if verb:
        vc = VERB_COLOR.get(verb, MUTED)
        tag(h, verb, vc, vc + "22" if verb in VERB_COLOR else SURF2)
    text(h, f"{time} · #{seq}", 11, FAINT)
    if quote:
        q = row(c, "quote", gap=6)
        text(q, "↪", 12, FAINT)
        text(q, quote, 12, MUTED)
    if body:
        wrap(c, body, 13, TEXT)
    return m, c, name


def card_box(parent, name, width):
    c = col(parent, name, gap=6, padding=[10, 12, 10, 12], cornerRadius=8, fill=SURF,
            stroke=BORDER, strokeWeight=1, sizingH="fixed")
    cmd("node.set", {"id": c, "width": width})
    return c


def done_card(parent, width, item, ref, checks, changes):
    c = card_box(parent, "DONE card", width)
    r = row(c, "title", gap=10)
    text(r, item, 12, TEXT, font=MONO, style="Bold")
    text(r, ref, 12, MUTED, font=MONO)
    cr = row(c, "checks", gap=8)
    dot(cr, OK, 7)
    text(cr, checks, 12, OK)
    for st, path in changes:
        r = row(c, "change", gap=10)
        text(r, st, 12, WARN if st == "M" else OK, font=MONO, style="Bold")
        text(r, path, 12, MUTED, font=MONO)


def review_card(parent, width, verdict, findings, traced):
    c = card_box(parent, "REVIEW card", width)
    r = row(c, "verdict", gap=8)
    if verdict == "changes":
        tag(r, "changes requested", WARN, WARN + "22")
    else:
        tag(r, "pass", OK, OK + "22")
    text(r, f"{len(findings)} findings", 12, MUTED)
    for sev, loc, what in findings:
        fr = row(c, "finding", gap=8, sizingH="fill", crossAlign="min")
        tag(fr, sev, BAD if sev == "high" else MUTED, BAD + "22" if sev == "high" else SURF2)
        fc = col(fr, "f", gap=2, sizingH="fill")
        text(fc, loc, 12, TEXT, font=MONO)
        wrap(fc, what, 12, MUTED)
    wrap(c, traced, 11, FAINT)


def divider(parent, label, color=FAINT):
    d = row(parent, f"divider {label}", gap=10, padding=[10, 12, 6, 12], sizingH="fill")
    frame(d, "line", 10, 1, fill=color + "55", sizingH="fill")
    text(d, label, 11, color, style="Semibold")
    frame(d, "line", 10, 1, fill=color + "55", sizingH="fill")


def toggle(parent, on, label):
    t = row(parent, "toggle", gap=8)
    sw = frame(t, "switch", 30, 18, fill=ACCENT if on else SURF2, cornerRadius=9,
               layout="horizontal", padding=2, crossAlign="center",
               mainAlign="max" if on else "min", stroke=BORDER, strokeWeight=0 if on else 1)
    cmd("node.create", {"type": "ellipse", "parent": sw, "width": 14, "height": 14,
                        "props": {"fill": "#ffffff"}})
    text(t, label, 13, TEXT if on else MUTED)
    return t


LEDGER = [
    (1839, "14:03", "andy/w3f", "CLAIM", "B-412"),
    (1840, "14:03", "hub", "GRANT", "B-412 to andy/w3f, lease 30 min"),
    (1841, "14:04", "andy/t7k", "RELEASE", "B-399, blocked on B-402"),
    (1842, "14:05", "adam/coord", "LEND", "L-INFRA deploy/** to andy/w3f until 16:00"),
]


def build_stream(parent, card_w, ledger_open):
    refs = {}
    _, c, _ = message(parent, 1838, "andy", None, "14:02",
                      "Stand by: deploy is frozen until the M1 tag. Finish your current step, "
                      "then hold.")
    m = row(c, "mentioned", gap=8)
    tag(m, "@all", ACCENT, ACCENT + "22")
    text(m, "mentioned 11 · in 10 agent sessions", 11, FAINT)
    if ledger_open:
        for seq, time, who, verb, detail in LEDGER:
            lr = row(parent, f"ledger #{seq}", gap=10, padding=[3, 12, 3, 54], sizingH="fill")
            text(lr, f"#{seq}", 11, "#4a5262", font=MONO)
            text(lr, time, 11, FAINT, font=MONO)
            text(lr, who, 12, MUTED, font=MONO)
            text(lr, verb, 12, ACCENT, font=MONO, style="Bold")
            text(lr, detail, 12, MUTED, font=MONO)
        hide = row(parent, "hide", padding=[2, 12, 6, 54])
        refs["hide"] = text(hide, "Hide ledger events", 12, ACCENT)
    else:
        lr = row(parent, "ledger folded", gap=8, padding=[3, 12, 3, 54], sizingH="fill")
        text(lr, "4 ledger events hidden: CLAIM, GRANT, RELEASE, LEND", 12, FAINT)
        refs["show"] = text(lr, "Show", 12, ACCENT, style="Medium")
    _, c, _ = message(parent, 1843, "adam/coord", "coordinator", "14:06",
                      "B-418 to @andy/dispatch: truncate long lane names on the board.",
                      verb="ASSIGN")
    t = row(c, "thread", gap=8)
    text(t, "1 reply", 12, ACCENT, style="Medium")
    text(t, "andy/dispatch: DELEGATED to andy/k2m (Engineer, sonnet, $5.00)", 12, FAINT)
    _, c, _ = message(parent, 1846, "adam/res", "researcher", "14:09",
                      "Yes. 0.25.0 broke websocket connections; 0.25.1 is the release to pin.",
                      verb="ANSWER", quote="andy: is rumqttc 0.25.1 safe for WSS?")
    text(c, "github.com/bytebeamio/rumqtt/blob/main/rumqttc/CHANGELOG.md", 11, ACCENT, font=MONO)
    _, c, name = message(parent, 1849, "andy/w3f", "engineer", "14:21",
                         "Truncation notice shows shown/total; regression spec added.",
                         verb="DONE")
    refs["w3f"] = name
    done_card(c, card_w, "B-412", "fix/b-412-map-truncation@2b5aa0d",
              "ui jest: 739 passed, 0 failed",
              [("M", "ui/src/components/flow/utils/readMapTruncation.ts"),
               ("A", "ui/src/components/flow/utils/readMapTruncation.test.ts")])
    _, c, _ = message(parent, 1851, "andy/rv2", "reviewer", "14:27", verb="REVIEW",
                      quote="andy/w3f: DONE B-412")
    review_card(c, card_w, "changes", [
        ("high", "readMapTruncation.ts:41",
         "An empty map gives total 0, so the notice reads 0/0. Input: a map with no nodes."),
        ("low", "readMapTruncation.test.ts:12", "The spec never covers total below the limit."),
    ], "Traced readMapTruncation from both callers. Left: the export path.")
    t = row(c, "thread", gap=8)
    refs["thread"] = text(t, "2 replies", 12, ACCENT, style="Medium")
    text(t, "last 14:40 · andy/rv2: pass", 12, FAINT)
    divider(parent, "New · 1 mentions you", WARN)
    message(parent, 1857, "adam/coord", "coordinator", "14:44",
            "@andy B-402's worktree has uncommitted work and andy/w2c is stopped. Restart it, "
            "or should I reassign B-402?", verb="ASK", highlight=True)
    return refs


def composer(main, typed=None):
    box = col(main, "composer", gap=6, sizingH="fill")
    r = row(box, "input", gap=8, padding=[9, 10, 9, 10], cornerRadius=8, fill=SURF,
            stroke=ACCENT if typed else BORDER, strokeWeight=1, sizingH="fill")
    chip = row(r, "chip @all", gap=6, padding=[3, 8, 3, 8], cornerRadius=4, fill=ACCENT + "22")
    text(chip, "@all", 12, ACCENT, font=MONO, style="Bold")
    text(chip, "×", 13, ACCENT)
    if typed:
        text(r, typed, 13, TEXT, font=MONO)
        frame(r, "caret", 1, 16, fill=ACCENT)
        frame(r, "spacer", 10, 10, sizingH="fill")
    else:
        wrap(r, "Message the room", 13, FAINT)
    button(r, "Send", "primary")
    text(box, "Enter sends · Shift+Enter adds a line · @ mentions. Humans start with @all; remove "
              "the chip to post without waking agents.", 11, FAINT)
    return r


MENTIONS = [
    ("Groups", [("@all", None, "everyone · 11"), ("@agents", None, "10 agents"),
                ("@humans", None, "andy, adam")]),
    ("Lanes", [("@L-UI", None, "held by andy/w3f"), ("@L-HUB", None, "held by andy/sol")]),
    ("People and agents", [("@adam", None, "human"), ("@andy/rv2", "reviewer", None),
                           ("@andy/w3f", "engineer", None), ("@adam/coord", "coordinator", None),
                           ("@adam/res", "researcher", None)]),
]


def build_chat(name, thread=False, mention=False, ledger=False):
    s, main = screen(name, "Chat")
    h = row(main, "header", gap=16, sizingH="fill", mainAlign="spaceBetween", crossAlign="min")
    left = col(h, "titles", gap=4, sizingH="fill")
    text(left, "Chat", 22, TEXT, style="Semibold")
    wrap(left, "The project feed. A message enters an agent's session only when it mentions that "
               "agent, is a DM to it, or assigns it work; everything else waits in the room.",
         13, MUTED)
    tg = toggle(h, ledger, "Show ledger events")
    link(tg, "Chat" if ledger else "Chat · ledger", "instant")

    body = row(main, "body", gap=16, sizingH="fill", sizingV="fill", crossAlign="min")
    stream = col(body, "stream", gap=2, sizingH="fill", sizingV="fill",
                 mainAlign="min" if ledger else "max",
                 clip=True)
    refs = build_stream(stream, 560 if thread else 640, ledger)
    link(refs["w3f"], "Agent · andy/w3f")
    link(refs["thread"], "Chat · thread")
    if "show" in refs:
        link(refs["show"], "Chat · ledger", "instant")
    if "hide" in refs:
        link(refs["hide"], "Chat", "instant")

    if thread:
        tp = col(body, "thread", gap=10, padding=16, cornerRadius=8, fill=PANEL, stroke=BORDER,
                 strokeWeight=1, sizingH="fixed", sizingV="fill")
        cmd("node.set", {"id": tp, "width": 440})
        th = row(tp, "head", sizingH="fill", mainAlign="spaceBetween")
        tl = row(th, "t", gap=8)
        text(tl, "Thread", 15, TEXT, style="Semibold")
        text(tl, "REVIEW #1851 · 3 messages", 12, FAINT)
        close = text(th, "×", 22, MUTED)
        link(close, "Chat")
        _, c, _ = message(tp, 1851, "andy/rv2", "reviewer", "14:27", verb="REVIEW")
        review_card(c, 330, "changes", [
            ("high", "readMapTruncation.ts:41", "An empty map gives total 0, so the notice "
                                                 "reads 0/0."),
            ("low", "readMapTruncation.test.ts:12", "No spec for total below the limit."),
        ], "Left: the export path.")
        _, c, _ = message(tp, 1853, "andy/w3f", "engineer", "14:35",
                          "Fixed the empty map case and added the spec.", verb="DONE")
        done_card(c, 330, "B-412", "@7c1d2e0", "ui jest: 741 passed, 0 failed", [])
        _, c, _ = message(tp, 1855, "andy/rv2", "reviewer", "14:40", verb="REVIEW")
        review_card(c, 330, "pass", [], "Traced both callers and the empty map path.")
        frame(tp, "spacer", 10, 10, sizingH="fill", sizingV="fill")
        note = col(tp, "note", gap=4, padding=[10, 12, 10, 12], cornerRadius=6, fill=SURF,
                   sizingH="fill")
        wrap(note, "Replies between agents add no mentions. A REVIEW reaches the DONE's author "
                   "through its pending queue, the way an ASSIGN reaches its assignee.", 11, MUTED)
        rr = row(tp, "reply", gap=8, padding=[9, 10, 9, 12], cornerRadius=8, fill=SURF,
                 stroke=BORDER, strokeWeight=1, sizingH="fill")
        wrap(rr, "Reply in thread as andy", 13, FAINT)
        button(rr, "Send", "primary")

    inp = composer(main, "@" if mention else None)
    if not mention:
        link(inp, "Chat · mention", "instant")

    if mention:
        pop = col(main, "mentions", gap=2, padding=8, cornerRadius=10, fill=PANEL, stroke=BORDER,
                  strokeWeight=1, absolute=True, x=32, y=372, sizingH="fixed",
                  effects=[{"type": "dropShadow", "color": "#00000099", "offset": [0, 8],
                            "radius": 24}])
        cmd("node.set", {"id": pop, "width": 440})
        for gname, items in MENTIONS:
            text(pop, gname, 11, FAINT, style="Semibold", textCase="upper", letterSpacing="6%")
            for label, role, note in items:
                on = label == "@andy/rv2"
                it = row(pop, f"m {label}", gap=10, padding=[6, 8, 6, 8], cornerRadius=6,
                         sizingH="fill", mainAlign="spaceBetween", **({"fill": SURF2} if on else {}))
                text(it, label, 13, TEXT, font=MONO)
                if role:
                    badge(it, role, 11)
                else:
                    text(it, note, 12, FAINT)
                link(it, "Chat", "instant")
        wrap(pop, "Humans and coordinators can use groups and lanes. Agents can mention up to 8 "
                  "individuals and nothing else.", 11, FAINT)


# ---------------------------------------------------------------- build

build_agents("Agents")
build_launch()
build_agents("Agents · launched", launched=True)
build_agent()
build_worktrees("Worktrees", False)
build_worktrees("Worktrees · pruned", True)
for k in ["reviewer", "engineer", "security-reviewer"]:
    build_roles(k)
build_chat("Chat")
build_chat("Chat · thread", thread=True)
build_chat("Chat · mention", mention=True)
build_chat("Chat · ledger", ledger=True)

for src, dest, tr in links:
    p = {"id": src, "trigger": {"type": "onClick"}, "action": {"type": "navigate",
                                                                "dest": screens[dest]}}
    if tr != "instant":
        p["transition"] = {"type": tr}
        p["durationMs"] = 160
        p["easing"] = "easeOut"
    cmd("proto.interaction.add", p)

cmd("proto.flow.add", {"start": screens["Agents"], "name": "Agent roles"})
cmd("proto.flow.add", {"start": screens["Chat"], "name": "Chat"})

json.dump(steps, open(sys.argv[1], "w"), indent=0)
print(f"{len(steps)} steps, {len(links)} links", file=sys.stderr)
