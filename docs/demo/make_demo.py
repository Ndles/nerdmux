#!/usr/bin/env python3
"""Record the nerdmux demo GIF with key captions.

Every scene is recorded by vhs as its own clip against one long-running
demo server, then a caption strip naming the keys is added below each clip
and the clips are joined into docs/demo/nerdmux.gif. Recording a scene on
its own keeps each caption exactly in step with its keys.

Run from the repository root:   python3 docs/demo/make_demo.py
Only one scene:                  python3 docs/demo/make_demo.py --only 3
See docs/demo/README.md for what it needs.
"""

import argparse
import os
import shutil
import subprocess
import sys
import textwrap
from pathlib import Path

REPO = Path(__file__).resolve().parents[1].parent
OUT = REPO / "docs/demo/nerdmux.gif"
WORK = Path("/tmp/nerdmux-demo")  # demo server, shell, project and clips
SOCK = WORK / "sock"
PROJECT = WORK / "shop"  # WORK is the demo's $HOME, so this shows as ~/shop
REAL_HOME = Path.home()

WIDTH, HEIGHT, FONT_SIZE = 1280, 720, 16
TYPING_MS = 45
STRIP = 84  # caption panel height in pixels, margins included
FONT_NAME = "JetBrainsMono Nerd Font Mono"

THEME = (
    '{ "background": "#15120F", "foreground": "#E6C58F", "cursor": "#E8A23A", '
    '"selection": "#3A322A", "black": "#2A241E", "red": "#D0705E", "green": "#A3B57A", '
    '"yellow": "#E3B062", "blue": "#7E9CB8", "magenta": "#B48CA8", "cyan": "#7FB4A8", '
    '"white": "#D9CCB4", "brightBlack": "#8A7D6C", "brightRed": "#E08A78", '
    '"brightGreen": "#B9C990", "brightYellow": "#EDC67F", "brightBlue": "#9AB4CC", '
    '"brightMagenta": "#C9A3BE", "brightCyan": "#9CCBBE", "brightWhite": "#F2E8D5" }'
)

LEADER = 'Alt+" "'


def type_(text):
    return f"Type `{text}`"


# ----- the storyboard ---------------------------------------------------------
#
# A scene is one clip. `session` is the session the scene starts in (None:
# the plain shell outside nerdmux, as in the first scene). Each step is
# (keys shown as badges, what they do, vhs commands). vhs cannot press Alt
# with an arrow or Alt+Shift with a letter, so those use the leader form,
# and the captions show what is actually pressed.

SCENES = [
    dict(session=None, steps=[
        (["nerdmux new -s web"], "start a session", [
            "Sleep 700ms", type_("nerdmux new -s web"), "Sleep 500ms", "Enter", "Sleep 1.2s",
        ]),
    ]),
    dict(session="web", steps=[
        (["nvim app.py"], "programs run as they always do", [
            type_("nvim app.py"), "Sleep 300ms", "Enter", "Sleep 2.2s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Enter"], "new pane", [
            "Alt+Enter", "Sleep 900ms", type_("python3 app.py"), "Enter", "Sleep 1.8s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "-"], "split down", [
            LEADER, type_("-"), "Sleep 900ms",
            type_("curl -s localhost:8000/orders"), "Enter", "Sleep 2s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "←"], "move the focus (Alt+arrows also work)", [
            LEADER, "Left", "Sleep 1.4s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Ctrl+Alt+→"], "resize", [
            "Ctrl+Alt+Right", "Sleep 250ms", "Ctrl+Alt+Right", "Sleep 250ms",
            "Ctrl+Alt+Right", "Sleep 1.2s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "z"], "zoom the pane in and out", [
            LEADER, type_("z"), "Sleep 1.6s", LEADER, type_("z"), "Sleep 1.2s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "t"], "new window", [
            LEADER, type_("t"), "Sleep 800ms", type_("lazygit"), "Enter", "Sleep 2.5s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "1"], "back to window 1 (Alt+1 also works)", [
            LEADER, type_("1"), "Sleep 1.6s",
        ]),
    ]),
    dict(session="web", detach=False, steps=[
        (["Alt+Space", "d"], "detach: everything keeps running", [
            LEADER, type_("d"), "Sleep 1.5s",
        ]),
        (["nerdmux ls"], "list sessions", [
            type_("nerdmux ls"), "Sleep 300ms", "Enter", "Sleep 1.8s",
        ]),
        (["nerdmux attach"], "and back, right where you left off", [
            type_("nerdmux attach"), "Sleep 300ms", "Enter", "Sleep 2s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "Shift+s"], "a second session for other work", [
            LEADER, type_("S"), "Sleep 900ms", type_("api"), "Sleep 300ms", "Enter", "Sleep 900ms",
            type_("git log --oneline"), "Enter", "Sleep 1.5s",
        ]),
    ]),
    dict(session="api", steps=[
        (["Alt+Space", "f"], "find any session, window or pane", [
            LEADER, type_("f"), "Sleep 1.2s", type_("lazy"), "Sleep 1.2s", "Enter", "Sleep 1.8s",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space", "s"], "all sessions at a glance", [
            LEADER, type_("s"), "Sleep 2.5s", "Escape", "Sleep 400ms",
        ]),
    ]),
    dict(session="web", steps=[
        (["Alt+Space"], "pause after the leader to see every key", [
            LEADER, "Sleep 3.5s", "Escape", "Sleep 500ms",
        ]),
    ]),
]


# ----- demo environment -------------------------------------------------------

DEMORC = r"""
# Shell for the demo panes: no personal aliases or prompt.
unalias -a
export PS1='\[\e[1;38;2;232;162;58m\]∷\[\e[0m\] '
export LESS=-R GIT_PAGER=cat HISTFILE=/dev/null
# nvim keeps your configuration but shows the terminal's background, so any
# colorscheme sits on the nerdmux theme.
# lazygit keeps your configuration too, with a state of its own that has
# already shown the first-run popup.
alias lazygit='XDG_STATE_HOME="$HOME/.demo-state" lazygit'
alias nvim='nvim -c "hi Normal guibg=NONE | hi NormalNC guibg=NONE | hi NormalFloat guibg=NONE | hi SignColumn guibg=NONE | hi LineNr guibg=NONE | hi EndOfBuffer guibg=NONE | hi MsgArea guibg=NONE"'
cd "$DEMO_PROJECT"
"""

APP = '''\
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

ORDERS = [{"id": 1, "item": "coffee"}, {"id": 2, "item": "bagel"}]


class Shop(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(ORDERS).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body + b"\\n")


if __name__ == "__main__":
    print("shop: serving orders on :8000", flush=True)
    HTTPServer(("", 8000), Shop).serve_forever()
'''

COMMITS = [
    ("README.md", "# shop\n\nA tiny order service.\n", "Start the shop"),
    ("app.py", APP, "Serve the order list"),
    ("README.md", "# shop\n\nA tiny order service: `python3 app.py`.\n", "Say how to run it"),
]


def git(*args):
    env = dict(os.environ, GIT_AUTHOR_NAME="demo", GIT_AUTHOR_EMAIL="demo@example.com",
               GIT_COMMITTER_NAME="demo", GIT_COMMITTER_EMAIL="demo@example.com")
    subprocess.run(["git", "-C", str(PROJECT), *args], check=True, env=env, capture_output=True)


def prepare():
    stop_server()
    shutil.rmtree(WORK, ignore_errors=True)
    PROJECT.mkdir(parents=True)
    git("init", "-q", "-b", "main")
    for name, text, message in COMMITS:
        (PROJECT / name).write_text(text)
        git("add", name)
        git("commit", "-q", "-m", message)
    (WORK / "demorc").write_text(DEMORC)
    (WORK / ".demo-state/lazygit").mkdir(parents=True)
    # Seen the intro and this version's notes, so no popup covers the scene.
    info = subprocess.run(["lazygit", "--version"], capture_output=True, text=True).stdout
    version = next((p[len("version="):] for p in info.split(", ") if p.startswith("version=")), "")
    (WORK / ".demo-state/lazygit/state.yml").write_text(
        f"startuppopupversion: 999\nlastversion: {version}\n"
    )
    (WORK / "config.toml").write_text(
        f'shell = "/bin/bash"\nshell_args = ["--rcfile", "{WORK}/demorc", "-i"]\n'
    )


def stop_server():
    subprocess.run(["nerdmux", "-S", str(SOCK), "kill-server"], capture_output=True)


# ----- recording --------------------------------------------------------------


def step_seconds(commands):
    """How long vhs takes for `commands`: each key and typed character waits
    one typing interval, and Sleep waits what it says."""
    total = 0.0
    for c in commands:
        if c.startswith("Sleep "):
            v = c.split()[1]
            total += float(v[:-2]) / 1000 if v.endswith("ms") else float(v[:-1])
        elif c.startswith("Type "):
            total += len(c[6:-1]) * TYPING_MS / 1000
        else:
            total += TYPING_MS / 1000
    return total


def tape(scene, clip):
    lines = [
        f'Output "{clip}"',
        'Set Shell "bash"',
        f'Set FontFamily "{FONT_NAME}"',
        f"Set FontSize {FONT_SIZE}",
        f"Set Width {WIDTH}",
        f"Set Height {HEIGHT}",
        "Set Padding 12",
        f"Set TypingSpeed {TYPING_MS}ms",
        "Set Framerate 30",
        f"Set Theme {THEME}",
        'Env COLORTERM "truecolor"',
        f'Env NERDMUX_SOCKET "{SOCK}"',
        f'Env NERDMUX_CONFIG "{WORK}/config.toml"',
        f'Env DEMO_PROJECT "{PROJECT}"',
        # A home of its own so paths read ~/shop; programs keep their real config.
        f'Env HOME "{WORK}"',
        f'Env XDG_CONFIG_HOME "{REAL_HOME}/.config"',
        f'Env XDG_DATA_HOME "{REAL_HOME}/.local/share"',
        f'Env XDG_STATE_HOME "{REAL_HOME}/.local/state"',
        f'Env XDG_CACHE_HOME "{REAL_HOME}/.cache"',
        "Hide",
        type_(f"unset NERDMUX; exec bash --rcfile {WORK}/demorc -i"), "Enter", "Sleep 400ms",
    ]
    if scene["session"]:
        lines += [type_("clear"), "Enter", "Sleep 200ms",
                  type_(f"nerdmux attach -t {scene['session']}"), "Enter", "Sleep 1.5s"]
    else:
        lines += [type_("clear"), "Enter", "Sleep 300ms"]
    lines.append("Show")
    for _, _, commands in scene["steps"]:
        lines += commands
    lines.append("Hide")
    if scene["session"] and scene.get("detach", True):
        lines += [LEADER, type_("d"), "Sleep 600ms"]
    return "\n".join(lines) + "\n"


def record(i, scene):
    clip = WORK / f"clip{i:02}.mp4"
    path = WORK / f"clip{i:02}.tape"
    path.write_text(tape(scene, clip))
    print(f"scene {i}: recording", flush=True)
    subprocess.run(["vhs", "-q", str(path)], check=True, cwd=PROJECT)
    return clip


# ----- captions ---------------------------------------------------------------

# The caption panel uses its own color, so it never reads as part of nerdmux.
BG, BADGE_FG, CAPTION_TEXT = "0x15120F", "0x15120F", "0xE6C58F"
CAPTION_COLORS = {"rose": ("0xC98A8F", "0x241A1A"), "mauve": ("0xA894B8", "0x1E1A21"),
                  "sage": ("0xA3B57A", "0x1C1E15")}  # frame and badges, panel fill
CAPTION_COLOR = "sage"


def font_file():
    return subprocess.run(["fc-match", "-f", "%{file}", FONT_NAME + ":bold"],
                          capture_output=True, text=True, check=True).stdout


def caption_filter(scene, i, color=None):
    """ffmpeg filter that adds the strip and draws each step's caption while
    that step runs. Text goes through files, so no escaping is needed."""
    font = font_file()
    size = 22
    advance = size * 0.6  # JetBrains Mono: every cell is 0.6 em wide
    frame, fill = CAPTION_COLORS[color or CAPTION_COLOR]
    margin = 12
    py, ph = HEIGHT + margin, STRIP - 2 * margin  # the panel
    parts = [
        f"pad={WIDTH}:{HEIGHT + STRIP}:0:0:color={BG}",
        f"drawbox=x={margin}:y={py}:w={WIDTH - 2 * margin}:h={ph}:color={fill}:t=fill",
        f"drawbox=x={margin}:y={py}:w={WIDTH - 2 * margin}:h={ph}:color={frame}:t=2",
    ]
    pad, height = 9, size + 14
    top = py + (ph - height) // 2
    text = f"fontfile={font}:fontsize={size}:y={top + 2}:y_align=font"
    start = 0.0
    for n, (keys, what, commands) in enumerate(scene["steps"]):
        end = start + step_seconds(commands)
        last = n == len(scene["steps"]) - 1
        enable = f"gte(t,{start:.3f})" if last else f"between(t,{start:.3f},{end:.3f})"
        x = margin + 18.0
        for k, key in enumerate(keys):
            f = WORK / f"cap{i:02}-{n}-{k}.txt"
            f.write_text(key)
            w = len(key) * advance + 2 * pad
            parts.append(f"drawbox=x={x:.0f}:y={top}:w={w:.0f}:h={height}:color={frame}:t=fill"
                         f":enable='{enable}'")
            parts.append(f"drawtext={text}:textfile={f}:fontcolor={BADGE_FG}:x={x + pad:.0f}"
                         f":enable='{enable}'")
            x += w + 10
        f = WORK / f"cap{i:02}-{n}-text.txt"
        f.write_text(what)
        parts.append(f"drawtext={text}:textfile={f}:fontcolor={CAPTION_TEXT}:x={x + 8:.0f}:enable='{enable}'")
        start = end
    return ",".join(parts)


def caption(i, scene, clip):
    out = WORK / f"cap{i:02}.mp4"
    script = WORK / f"cap{i:02}.filter"
    script.write_text(caption_filter(scene, i))
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(clip),
                    "-/vf", str(script), "-an", str(out)], check=True)
    return out


def join(clips):
    listing = WORK / "clips.txt"
    listing.write_text("".join(f"file '{c}'\n" for c in clips))
    joined = WORK / "joined.mp4"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "concat", "-safe", "0",
                    "-i", str(listing), "-c", "copy", str(joined)], check=True)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(joined), "-vf",
                    "fps=15,split[a][b];[a]palettegen=stats_mode=diff[p];"
                    "[b][p]paletteuse=dither=none:diff_mode=rectangle",
                    "-loop", "0", str(OUT)], check=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", type=int, help="record scenes up to and including this one, "
                    "and keep only its clip (for checking one scene)")
    args = ap.parse_args()
    for tool in ("vhs", "ffmpeg", "nerdmux", "git", "nvim", "lazygit", "python3", "curl"):
        if not shutil.which(tool):
            sys.exit(f"make_demo: {tool} is not installed")
    prepare()
    scenes = SCENES if args.only is None else SCENES[: args.only + 1]
    clips = []
    try:
        for i, scene in enumerate(scenes):
            clips.append(caption(i, scene, record(i, scene)))
    finally:
        stop_server()
    if args.only is None:
        join(clips)
        print(f"wrote {OUT.relative_to(REPO)}")
    else:
        print(f"scene {args.only}: {clips[-1]}")


if __name__ == "__main__":
    main()
