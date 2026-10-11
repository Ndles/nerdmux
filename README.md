# nerdmux

![nerdmux: panes, windows, detach and reattach, sessions](docs/demo/nerdmux.gif)

A terminal multiplexer in one binary: sessions that survive closing the
terminal, windows (tabs) and split panes. Arrow-key chords move you
around; everything else is one leader key, `Alt+Space`, followed by a
letter that names what it does. There are no modes to learn, and the
shell keeps its own `Alt+letter` keys.

It comes with its own look: a warm charcoal screen, amber as the main
color, and muted, earthy tones for everything else. A settings panel
changes any option on the spot and writes it to your config file, keeping
your comments.

Runs on Linux and macOS.

> **Early days.** This is version 0.1.0. Expect rough edges, and changes
> to keys and config between versions until 1.0. Bug reports are welcome; feature requests may take a
> while or not fit the goal of a small, simple tool.

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
| see every key                  | `Alt+Space ?`                           |
| leave, keeping it all running  | `Alt+Space d`                           |

The idea behind the keys:

- **Chords** are only arrows, digits and `Enter`, so the shell keeps its
  own `Alt+letter` keys (word motions, history search, ...). `Alt` moves
  between panes, `Alt+Shift` reaches one level further out (windows
  sideways, sessions up and down), `Ctrl+Alt` resizes.
- **Everything else** goes through the leader, `Alt+Space`, followed by
  the first letter of what you want: `t` tab, `n` name, `s` sessions,
  `c` copy, `v` paste, `z` zoom, `x` close, `p` palette.

The status bar shows the session and its windows on the left; on the
right, the focused pane's directory, its git branch and how many panes the
window has, followed by badges such as `ZOOM` or `COPY` while those are on.

### Sessions from the shell

```sh
nerdmux new -s work            # start a named session
nerdmux new -s build -- make   # ...running a command instead of a shell
nerdmux new -d -s bg           # start one without attaching
nerdmux ls                     # list sessions
nerdmux attach -t work         # attach by name (a unique prefix is enough)
nerdmux kill-session -t work
nerdmux kill-server            # end everything
nerdmux action split-right     # run any action from a script or a pane
```

## Keys

Press the leader, `Alt+Space`, then the key in the second column. Pause
after the leader and a panel lists the keys.

### Panes

| Chord            | Leader +       | Action                                           |
|------------------|----------------|--------------------------------------------------|
| `Alt+Enter`      | `Enter`        | new pane, split along the longer side            |
|                  | `\|` or `\`    | split right                                      |
|                  | `-`            | split down                                       |
|                  | `x`            | close the pane (asks first)                      |
|                  | `z`            | zoom the pane to the whole window, and back      |
| `Alt+←↓↑→`       | `←↓↑→`         | focus the pane in that direction (wraps around)  |
|                  | `Shift+←↓↑→`   | move the pane in that direction                  |
| `Ctrl+Alt+←↓↑→`  | `Ctrl+←↓↑→`    | resize the pane (after the leader it repeats)    |
|                  | `r`            | resize mode, see below                           |
|                  | `l`            | cycle through layouts                            |
|                  | `g`            | show pane numbers, then press one to go there    |
|                  | `b`            | move the pane to a window of its own             |
|                  | `a`            | type in all panes of the window at once          |
|                  | `k`            | clear the pane's scrollback                      |

### Windows (tabs)

| Chord            | Leader +       | Action                                           |
|------------------|----------------|--------------------------------------------------|
|                  | `t`            | new window                                       |
|                  | `w`            | close the window (asks first)                    |
|                  | `n`            | name the window                                  |
| `Alt+Shift+←→`   |                | previous / next window                           |
| `Alt+1` … `Alt+9`| `1` … `9`      | go to window N                                   |
|                  | `Tab`          | last used window                                 |
|                  | `<` / `>`      | move the window left / right                     |

### Sessions

| Chord            | Leader +       | Action                                           |
|------------------|----------------|--------------------------------------------------|
|                  | `s`            | session manager, see below                       |
|                  | `f`            | find a session, window or pane by name           |
| `Alt+Shift+↑↓`   |                | previous / next session                          |
|                  | `Shift+s`      | new session                                      |
|                  | `Shift+n`      | name the session                                 |
|                  | `d`            | detach                                           |

### Copy & paste, and the rest

| Chord            | Leader +       | Action                                           |
|------------------|----------------|--------------------------------------------------|
|                  | `c`            | copy mode                                        |
| `Alt+PgUp`       | `PgUp`         | copy mode, one page up                           |
|                  | `v`            | paste what was copied                            |
|                  | `p`            | command palette                                  |
|                  | `,`            | settings, see below                              |
|                  | `?` or `h`     | every key, on screen                             |
|                  | `Shift+r`      | reload the config                                |
|                  | `Alt+Space`    | send `Alt+Space` itself to the program           |

### Session manager (`Alt+Space s`)

A list of your sessions with their windows and directory.

| Key            | Action                                  |
|----------------|-----------------------------------------|
| `↑↓`, `Enter`  | pick a session and switch to it         |
| `n`            | new session (asks for a name)           |
| `r`            | rename the selected session             |
| `x`            | kill the selected session (asks first)  |
| `/`            | filter by name; `Esc` clears the filter |
| `Esc`          | close                                   |

### Resize mode (`Alt+Space r`)

For when a pane needs more than one nudge. A line at the bottom shows the
keys while it is active.

| Key            | Action                              |
|----------------|-------------------------------------|
| `←↓↑→`         | move the border of the focused pane |
| `Shift+←↓↑→`   | the same, in bigger steps           |
| `Ctrl+←↓↑→`    | move the pane itself                |
| `Enter`, `Esc` | done                                |

Any other chord ends the mode and does what it normally does.

### Settings (`Alt+Space ,`)

Every option of the config file in one list, with the value in force.
Changes apply at once and are saved to `config.toml`, which is created on
the first change; your own comments and layout in it are kept. A `•`
marks what the file sets; the rest are defaults.

| Key              | Action                                              |
|------------------|-----------------------------------------------------|
| type             | filter the list                                     |
| `↑↓`, `Enter`    | pick a setting and change it                        |
| `←→`             | flip a switch, cycle a choice, step a number (`Shift` for bigger steps) |
| `Del`            | back to the default (removes the line from the file)|
| `Enter` on a key | press the new key: with `Alt` or `Super` it becomes a chord, otherwise it goes after the leader |
| `Esc`            | clear the filter, then close                        |

### Copy mode (`Alt+Space c`)

Scroll back through output, search it and copy, with the keys of an
ordinary text editor.

| Key                      | Action                                     |
|--------------------------|--------------------------------------------|
| `←↓↑→`                   | move                                       |
| `Ctrl+←` / `Ctrl+→`      | previous / next word                       |
| `Home` / `End`           | start / end of the line                    |
| `PgUp` / `PgDn`          | a page up / down                           |
| `Ctrl+Home` / `Ctrl+End` | oldest / newest line                       |
| `Ctrl+↑` / `Ctrl+↓`      | scroll the view by one line                |
| `Shift` + any of these   | select while moving                        |
| `Ctrl+a`                 | select everything                          |
| `Ctrl+l`                 | select whole lines                         |
| `Ctrl+r`                 | select a rectangle                         |
| `Ctrl+c` or `Enter`      | copy the selection and leave               |
| `Ctrl+f`                 | find (regular expression; lower case ignores case) |
| `F3` / `Shift+F3`        | older / newer match                        |
| `Esc`                    | clear the selection, then leave            |

Copied text goes to nerdmux's own paste buffer (`Alt+Space v`) and to the system
clipboard: through your terminal (OSC 52, which also works over SSH) and
through `pbcopy`, `wl-copy`, `xclip` or `xsel` when one is available.

### Mouse

Click a pane to focus it or a tab to select it, drag a border to resize,
drag to select and copy, double- or triple-click for a word or a line, use
the wheel to scroll back, middle-click to paste. Programs that ask for the
mouse themselves (vim, htop, ...) get it instead. Hold `Shift` to use your
terminal's own selection.

### Super, and Ctrl+Shift chords

Classic terminals cannot tell a program that `Super` (`Cmd`, the Windows
key) is held, nor distinguish `Ctrl+Shift+t` from `Ctrl+t`. Terminals that
implement the kitty keyboard protocol can: kitty, WezTerm, Ghostty, foot,
Alacritty, and iTerm2 with the setting enabled. nerdmux asks for that protocol
when the terminal offers it, so there you can bind such chords:

```toml
[keys.direct]
"super+left"   = "focus left"
"super+enter"  = "split-auto"
"ctrl+shift+t" = "new-window"
```

None are bound by default, because window managers and terminal emulators
keep most `Super` chords for themselves and what is left differs from desk
to desk. `Super` and `Ctrl+Shift` chords have been checked in kitty and
Ghostty; the other terminals only with the byte sequences they send. Set
`extended_keys = false` to switch the request off.

### The leader on other terminals

Some terminals swallow `Alt` (macOS Terminal.app and iTerm2 until "Use
Option as Meta key" is turned on). There, pick a leader without it, for
example `prefix = "ctrl+a"`, and bind the chords you miss under
`[keys.direct]`.

## Colors

The default theme paints the whole screen itself, so it looks the same in
any terminal:

| Role                                 | Color                    |
|--------------------------------------|---------------------------|
| background                           | warm charcoal `#15120F`   |
| panes without the focus              | darker `#0E0C0A`          |
| text                                 | soft amber `#E6C58F`      |
| focus, active tab, frames            | amber `#E8A23A`           |
| key names, tab numbers               | teal `#7FB4A8`            |
| headings, prefix badge, selection    | dusty rose `#C98A8F`      |
| messages, copy badge, search matches | sage `#A3B57A`            |
| zoom badge, pane numbers             | mauve `#A894B8`           |
| bells, "all panes" badge             | brick `#D0705E`           |

Once a window is split, the focused pane is outlined in amber on all four
sides, and the other panes sit on a darker background. To leave room for the outline along the
screen edge, split panes keep a one-cell margin around them.

Programs inside panes get a matching 16-color palette in the same muted
tones, and the cursor is tinted amber. nerdmux also sets the terminal's own
background color while attached (and restores it on detach), so the
terminal's window padding matches instead of framing the screen in a
different color. Terminals without 24-bit color receive the nearest of
the 256 standard colors instead; nerdmux decides from `$COLORTERM`.

If you would rather keep your terminal's own colors inside panes and have
only nerdmux's frame themed, set `own_palette = false`.

## Configuration

`~/.config/nerdmux/config.toml` (print the location with `nerdmux config --path`,
a commented template with `nerdmux config`). Everything is optional:

```toml
prefix = "ctrl+a"
status = "top"              # "top", "bottom" or "off"
scrollback = 20000
mouse = true
dim_inactive = true         # fade panes that do not have the focus
alt_bindings = true         # false drops every built-in chord

[theme]                     # "#rrggbb", a 0-255 palette index or a color name
accent = "#FF8C00"
cyan = "#8FC1B5"
own_palette = true
# palette = ["#2A241E", "#D0705E", ...]   # all 16 ANSI colors

[keys.direct]               # chords
"f2" = "rename-window"
"alt+enter" = "none"        # "none" removes a default binding

[keys.prefix]               # pressed after the leader
"e" = "split-right"
```

Chords are written as modifiers joined with `+`: `ctrl`, `alt`, `shift`,
`super`, then a key such as `left`, `pageup`, `enter`, `f5` or a character.
`nerdmux keys` prints the current bindings and the name of every action.
`Alt+Space Shift+r` reloads the file without restarting anything; mistakes are
reported in the status bar and the affected settings keep their defaults.

## Things worth knowing

- **Chords that collide.** `Alt+arrows` jump by word in some shell setups
  (fish uses them for directory history). Unbind what you miss with
  `"alt+left" = "none"` or move it elsewhere.
- **Alt and Escape.** Classic terminals send `Alt+x` as `Esc` followed by
  `x`, so on a slow link a quick `Esc` then `x` can be read as the chord.
  Terminals with the kitty protocol do not have this ambiguity.
- **Nesting.** Starting nerdmux inside nerdmux is refused unless you unset
  `$NERDMUX`; use the switcher to change session instead.
- **Memory.** Each pane keeps its own scrollback; a pane with 10 000 full
  lines uses roughly 25 MB.

## Why not tmux or Zellij?

Both are mature and do more. nerdmux is for people who want something
smaller with keys that are easy to learn:

- **Keys.** tmux puts everything behind a prefix and single letters that
  have to be memorized (`%`, `"`, `&`); Zellij uses modes and `Ctrl`
  chords that often clash with programs in the pane. nerdmux uses arrows
  for moving, a leader with first-letter keys for the rest, and leaves
  `Ctrl+letter` and `Alt+letter` to your shell and editor.
- **Setup.** It looks finished without configuration, and the settings
  panel covers what people usually look up how to change.
- **Size.** One small binary, no plugin system, no scripting language.

What you give up:

- **No plugins.** tmux has a large ecosystem (session restore, editor
  integration such as vim-tmux-navigator, project layout tools); Zellij
  has WebAssembly plugins and layout files. nerdmux has `nerdmux action`
  for scripting and nothing more.
- **Fewer features.** No session restore after a reboot, no floating
  panes, no images in panes.
- **Less proven.** tmux has had more than 15 years of use and is
  installed on most servers; nerdmux is new.

## How it works

`nerdmux` starts a small server in the background the first time you run it.
The server owns the pseudo-terminals, emulates each pane's screen (using
Alacritty's terminal engine) and draws for every attached client; the
client you type into only forwards keys and writes what the server sends.
That is why sessions outlive the terminal window, and why several clients
can show the same session at once. The server exits when its last session
ends.

Socket, lock and server log live in `$XDG_RUNTIME_DIR/nerdmux/` (or
`$TMPDIR/nerdmux-<uid>/`); use `-S PATH` or `$NERDMUX_SOCKET` for a separate
server.

## Not there yet

Sessions do not survive a reboot. No image protocols, hyperlinks or
ligature-aware rendering; underline styles are drawn as a plain underline.
The macOS-specific code compiles but has not been run on a Mac.

## Development

```sh
cargo test          # unit tests plus end-to-end tests against a real server
```

The end-to-end tests start a private server, attach over the socket like a
real client, replay what the server draws into a terminal emulator and
check the resulting screen. They need `/bin/sh`, `seq` and `stty`.

## License

MIT
