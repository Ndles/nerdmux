# Recording the demo GIF

`make_demo.py` records `nerdmux.gif`: each scene is typed into a headless
terminal by [vhs](https://github.com/charmbracelet/vhs), and a strip below
the terminal names the keys being pressed. Rerun it whenever the look
changes.

## Setup (once)

```sh
sudo pacman -S vhs lazygit  # vhs pulls in ttyd and ffmpeg, and downloads a headless Chromium on first run
cargo install --path .      # the demo runs the installed `nerdmux`
```

It also uses `nvim`, `python3`, `curl` and `git`, and the font
`JetBrainsMono Nerd Font Mono` (change `FONT_NAME` for another).

## Run

From the repository root:

```sh
python3 docs/demo/make_demo.py            # about 3 minutes, writes docs/demo/nerdmux.gif
python3 docs/demo/make_demo.py --only 4   # record up to scene 4 and keep that clip, to check one scene
```

Everything happens in `/tmp/nerdmux-demo`: its own server socket, config,
`$HOME` and a throwaway git project, `~/shop`. Your sessions, aliases and
prompt are untouched and never appear in the GIF; nvim and lazygit keep
your configuration, except that nvim's background is cleared so any
colorscheme sits on the nerdmux theme, and lazygit gets a state of its own
so its first-run and release-notes popups never cover a scene. Running it
from inside nerdmux is fine.

## How it works

`SCENES` in the script is the storyboard. Each scene is recorded as its
own clip: vhs attaches to the demo server out of view, plays the scene,
and detaches. Since the server keeps running between clips, the scenes
continue from one another. Each step of a scene lists the keys shown as
badges, what they do, and the vhs commands that press them. The caption
timing is computed from those commands (every key or typed character
takes one typing interval, `Sleep` what it says), which matches the
recording to within a frame or two.

## Limits of vhs

vhs cannot press `Alt` with an arrow key, `Alt+Shift` with a letter, or
`Alt+digit`. Those scenes use the leader form instead (`Alt+Space` then
`←`, `Alt+Space` then `1`), and the captions show what is actually pressed.
