<p align="center">
<picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo/nerdmux-on-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/logo/nerdmux-on-light.svg">
    <img alt="nerdmux" src="docs/logo/nerdmux-card-dark.svg" width="320">
</picture>
</p>

![nerdmux: panes, windows, detach and reattach, sessions](docs/demo/nerdmux.gif)

A terminal multiplexer in one binary: windows (tabs), split panes, and sessions that outlive the terminal, and even a
crash or a reboot.

nerdmux is opinionated by design, with keys that work like a game's controls: movement always sits on the arrows (`Alt`
for panes, `Alt+Shift` for windows and sessions), and every action is the leader, `Alt+Space`, followed by the first
letter of what it does. No modes, nothing to memorize, and your shell keeps its own `Alt+letter` keys. It looks finished
out of the box, in warm charcoal and amber, and a settings panel changes anything on the spot.

Runs on Linux and macOS.

## Build

Needs Rust 1.85 or newer.

```sh
cargo install --path .      # puts `nerdmux` in ~/.cargo/bin
```

## Getting started

```sh
nerdmux                # attach to your last session, or start the first one
```

| You want to                    | Press                                   |
|--------------------------------|-----------------------------------------|
| open a new pane                | `Alt+Enter`                             |
| move between panes             | `Alt+←` `Alt+↓` `Alt+↑` `Alt+→`         |
| go to the window left or right | `Alt+Shift+←` `Alt+Shift+→`             |
| go to the session above/below  | `Alt+Shift+↑` `Alt+Shift+↓`             |
| anything else                  | `Alt+Space`, then a key (a panel lists them) |
| open a new window (tab)        | `Alt+Space t`                           |
| manage sessions                | `Alt+Space s`                           |
| change settings                | `Alt+Space ,`                           |
| leave, keeping it all running  | `Alt+Space d`                           |

`Alt+Space ?` shows every key, and `nerdmux keys` prints them. The mouse works too: click to focus, drag borders to
resize, drag to copy, scroll back with the wheel.

```sh
nerdmux new -s work            # start a named session
nerdmux new -s build -- make   # ...running a command instead of a shell
nerdmux ls                     # list sessions
nerdmux attach -t work         # attach by name (a unique prefix is enough)
nerdmux kill-server            # end everything
nerdmux action split-right     # run any action from a script or a pane
```

### After a crash or reboot

nerdmux keeps a snapshot of its sessions in `~/.local/state/nerdmux/`, readable only by you, and deletes it when it ends
cleanly. If it did not, the next `nerdmux` offers the saved sessions back: each pane returns in its directory with its
old text. On Linux, editors, pagers and monitors (nvim, less, btop, lazygit, ...) start again; anything else, such as a
dev server or `ssh`, is typed at the prompt for you to run with Enter. Environment variables are not saved, since they
may hold secrets.

## Configuration

`~/.config/nerdmux/config.toml`; `nerdmux config` prints a commented template, and the settings panel (`Alt+Space ,`)
edits the file for you, keeping your comments. Everything is optional:

```toml
prefix = "ctrl+a"           # the leader
status = "top"              # "top", "bottom" or "off"
restore_programs = ["k9s"]  # also started again after a crash

[theme]
accent = "#FF8C00"
own_palette = false         # keep your terminal's colors inside panes

[keys.direct]               # chords
"alt+enter" = "none"        # "none" removes a default binding

[keys.prefix]               # keys after the leader
"e" = "split-right"
```

## Things worth knowing

- **Alt on macOS.** Terminal.app and iTerm2 keep `Alt` (Option) to themselves until "Use Option as Meta key" is turned
  on. Or pick a leader without it, such as `prefix = "ctrl+a"`.
- **Chords that collide.** `Alt+arrows` jump by word in some shell setups. Unbind what you miss with
  `"alt+left" = "none"`.
- **Selecting text.** Hold `Shift` to use your terminal's own selection instead of nerdmux's.
- **Nesting.** Starting nerdmux inside nerdmux is refused; use the session manager (`Alt+Space s`) to change session.

## How it compares

tmux and Zellij are both excellent; nerdmux makes different choices.

- **Keys.** tmux puts everything behind a prefix and single symbols to memorize (`%`, `"`, `&`); Zellij uses modes and
  `Ctrl` chords. nerdmux moves with the arrows, runs actions with the leader and a first letter, and leaves
  `Ctrl+letter` and `Alt+letter` to your shell and editor.
- **Setup.** It is ready without a config file, and the settings panel covers what you would otherwise look up.
- **Scope.** One small binary focused on panes, windows and sessions. Instead of a plugin system, any action can be
  scripted with `nerdmux action`.

## License

MIT
