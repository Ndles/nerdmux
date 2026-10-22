#!/usr/bin/env bash
# Set up the desktop around nerdmux: a launcher with the nerdmux icon, a
# terminal window made for it, and a Nerd Font. Linux (GNOME, Hyprland,
# others) and macOS.
#
#   scripts/setup.sh              set it up, asking before anything outside nerdmux
#   scripts/setup.sh --dry-run    show what it would do, change nothing
#
# Everything goes under your home directory; nothing needs sudo. A file it
# replaces is kept next to itself as <name>.bak-<date>. Run it again at any
# time: what is already in place is left alone.
#
# Works with bash 3.2, the one macOS ships.

set -eu

# Omarchy and others name the preferred terminal in $TERMINAL.
PREFERRED_TERMINAL="${TERMINAL:-}"
DRY_RUN=0
YES=0
FONT=1
SIZE="200x80"

usage() {
    cat <<'EOF'
Usage: setup.sh [options]

  -n, --dry-run     show what would change, change nothing
  -y, --yes         answer yes to every question
      --no-font     do not offer to install a Nerd Font
      --size CxR    open the window CxR cells large (default 200x80)
      --maximized   open the window maximized instead
  -h, --help        this text
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        -n | --dry-run) DRY_RUN=1 ;;
        -y | --yes) YES=1 ;;
        --no-font) FONT=0 ;;
        --size)
            [ $# -ge 2 ] || { echo "--size needs a value such as 200x80" >&2; exit 2; }
            SIZE="$2"
            shift
            ;;
        --size=*) SIZE="${1#--size=}" ;;
        --maximized) SIZE="" ;;
        -h | --help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

if [ -n "$SIZE" ]; then
    case "$SIZE" in
        *[!0-9x]* | x* | *x | *x*x* | "") echo "--size must look like 200x80, not $SIZE" >&2; exit 2 ;;
        *x*) COLS="${SIZE%x*}"; ROWS="${SIZE#*x}" ;;
        *) echo "--size must look like 200x80, not $SIZE" >&2; exit 2 ;;
    esac
fi

# ----- output and changes -----------------------------------------------------

if [ -t 1 ]; then
    BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); AMBER=$(printf '\033[38;5;214m'); OFF=$(printf '\033[0m')
else
    BOLD=""; DIM=""; AMBER=""; OFF=""
fi

STAMP=$(date +%Y%m%d-%H%M%S)
CHANGED=0

step() { printf '\n%s%s%s\n' "$BOLD" "$1" "$OFF"; }
say() { printf '  %s\n' "$1"; }
note() { printf '  %s%s%s\n' "$DIM" "$1" "$OFF"; }
done_() { printf '  %s✓%s %s\n' "$AMBER" "$OFF" "$1"; }

# Run a command, or only show it on a dry run.
run() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '  %swould run:%s %s\n' "$DIM" "$OFF" "$*"
    else
        "$@"
    fi
}

# ask "question" -> 0 for yes. Defaults to no; --yes says yes; a dry run
# assumes yes, to show everything it would do.
ask() {
    if [ "$YES" = 1 ] || [ "$DRY_RUN" = 1 ]; then
        printf '  %s [y/N] y\n' "$1"
        return 0
    fi
    if [ ! -t 0 ]; then
        printf '  %s [y/N] n (no terminal to ask in)\n' "$1"
        return 1
    fi
    printf '  %s [y/N] ' "$1"
    read -r answer
    case "$answer" in y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}

# put_file <path> <content>: write the file unless it already holds exactly
# that; an old version is kept as <path>.bak-<stamp>.
put_file() {
    path="$1"
    content="$2"
    if [ -f "$path" ] && [ "$(cat "$path")" = "$content" ]; then
        note "unchanged: $path"
        return 0
    fi
    if [ "$DRY_RUN" = 1 ]; then
        if [ -e "$path" ]; then say "would replace $path (keeping $path.bak-$STAMP)"; else say "would write $path"; fi
        return 0
    fi
    mkdir -p "$(dirname "$path")"
    if [ -e "$path" ]; then
        mv "$path" "$path.bak-$STAMP"
        say "kept the old one as $path.bak-$STAMP"
    fi
    printf '%s\n' "$content" >"$path"
    done_ "wrote $path"
    CHANGED=1
}

# put_copy <source> <path>: copy unless identical, keeping an old version.
put_copy() {
    src="$1"
    path="$2"
    if [ -f "$path" ] && cmp -s "$src" "$path"; then
        return 0
    fi
    if [ "$DRY_RUN" = 1 ]; then
        say "would copy $(basename "$src") to $path"
        return 0
    fi
    mkdir -p "$(dirname "$path")"
    if [ -e "$path" ]; then
        mv "$path" "$path.bak-$STAMP"
    fi
    cp "$src" "$path"
    CHANGED=1
}

# ----- where things are -------------------------------------------------------

HERE=$(cd "$(dirname "$0")" && pwd)
if [ -d "$HERE/icons" ]; then
    ICONS="$HERE/icons"            # a release archive: setup.sh next to icons/
elif [ -d "$HERE/../docs/logo" ]; then
    ICONS=$(cd "$HERE/../docs/logo" && pwd)   # the repository: scripts/setup.sh
else
    echo "cannot find the nerdmux icons next to this script" >&2
    exit 1
fi

OS=$(uname -s)
case "$OS" in
    Darwin) PLATFORM=macos ;;
    Linux)
        desktop=$(printf '%s' "${XDG_CURRENT_DESKTOP:-}" | tr '[:upper:]' '[:lower:]')
        if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] || case "$desktop" in *hyprland*) true ;; *) false ;; esac; then
            PLATFORM=hyprland
        elif case "$desktop" in *gnome*) true ;; *) false ;; esac; then
            PLATFORM=gnome
        else
            PLATFORM=linux
        fi
        ;;
    *) echo "nerdmux runs on Linux and macOS; this is $OS" >&2; exit 1 ;;
esac

printf '%snerdmux setup%s for %s%s\n' "$BOLD" "$OFF" "$PLATFORM" "$([ "$DRY_RUN" = 1 ] && echo ' (dry run: nothing changes)')"
if [ "$PLATFORM" = macos ]; then
    note "macOS support is new and not yet tested on a Mac; --dry-run shows each step first."
fi

# ----- 1. the binary ----------------------------------------------------------

step "1. nerdmux itself"
NERDMUX=""
for candidate in "$(command -v nerdmux 2>/dev/null || true)" "$HOME/.cargo/bin/nerdmux" "$HOME/.local/bin/nerdmux"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        NERDMUX="$candidate"
        break
    fi
done
if [ -z "$NERDMUX" ] && [ -x "$HERE/nerdmux" ] && [ ! -d "$HERE/../src" ]; then
    # Run from an unpacked release archive.
    say "nerdmux is not installed yet; the one in this archive can go to ~/.local/bin."
    if ask "Install it there?"; then
        run mkdir -p "$HOME/.local/bin"
        run cp "$HERE/nerdmux" "$HOME/.local/bin/nerdmux"
        NERDMUX="$HOME/.local/bin/nerdmux"
        case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) note "add ~/.local/bin to your PATH to run it from a shell" ;; esac
    fi
fi
if [ -z "$NERDMUX" ]; then
    echo "  nerdmux is not installed. Build it first: cargo install --path . (from the repository)" >&2
    exit 1
fi
done_ "using $NERDMUX"

# ----- 2. a Nerd Font ---------------------------------------------------------

FONT_FAMILY=""
# mac_font <glob>: a font file in either macOS font folder matches.
mac_font() {
    for f in "$HOME/Library/Fonts"/$1 /Library/Fonts/$1; do
        [ -e "$f" ] && return 0
    done
    return 1
}
nerd_font_present() {
    if [ "$PLATFORM" = macos ]; then
        mac_font '*NerdFont*'
    else
        command -v fc-list >/dev/null 2>&1 && fc-list : family | grep -qi 'nerd font'
    fi
}
step "2. A Nerd Font, for the icons nvim, lazygit and others draw"
if nerd_font_present; then
    done_ "a Nerd Font is installed"
    if [ "$PLATFORM" = macos ]; then
        if mac_font 'JetBrainsMonoNerdFont-*'; then
            FONT_FAMILY="JetBrainsMono Nerd Font"
        fi
    elif fc-list : family | grep -qi 'JetBrainsMono Nerd Font'; then
        FONT_FAMILY="JetBrainsMono Nerd Font"
    fi
elif [ "$FONT" = 0 ]; then
    note "skipped (--no-font)"
else
    if [ "$PLATFORM" = macos ]; then fonts="$HOME/Library/Fonts"; else fonts="$HOME/.local/share/fonts/JetBrainsMonoNerdFont"; fi
    say "None found. JetBrainsMono Nerd Font (about 7 MB from github.com/ryanoasis/nerd-fonts) can go to $fonts."
    if ask "Download and install it?"; then
        if [ "$DRY_RUN" = 1 ]; then
            say "would download JetBrainsMono.tar.xz and unpack its .ttf files into $fonts"
        else
            tmp=$(mktemp -d)
            curl -fsSL -o "$tmp/font.tar.xz" https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.tar.xz
            mkdir -p "$tmp/x" "$fonts"
            tar -xJf "$tmp/font.tar.xz" -C "$tmp/x"
            cp "$tmp"/x/JetBrainsMonoNerdFont-*.ttf "$fonts/"
            rm -rf "$tmp"
            if command -v fc-cache >/dev/null 2>&1; then fc-cache -f "$fonts" >/dev/null; fi
            done_ "installed JetBrainsMono Nerd Font in $fonts"
            CHANGED=1
        fi
        FONT_FAMILY="JetBrainsMono Nerd Font"
    fi
fi

# ----- 3. the terminal --------------------------------------------------------

step "3. A terminal for nerdmux"
TERMINAL=""
TERMINAL_CMD=""
if [ "$PLATFORM" = macos ]; then
    for app in /Applications/Alacritty.app "$HOME/Applications/Alacritty.app"; do
        if [ -x "$app/Contents/MacOS/alacritty" ]; then
            TERMINAL=alacritty
            TERMINAL_CMD="$app/Contents/MacOS/alacritty"
        fi
    done
else
    # Prefer the terminal the desktop is set up with, then the ones that can
    # drop their title bar.
    for t in "$PREFERRED_TERMINAL" alacritty kitty ghostty; do
        case "$t" in alacritty | kitty | ghostty) ;; *) continue ;; esac
        if command -v "$t" >/dev/null 2>&1; then
            TERMINAL="$t"
            TERMINAL_CMD=$(command -v "$t")
            break
        fi
    done
fi

if [ -z "$TERMINAL" ]; then
    say "No Alacritty, kitty or Ghostty found. nerdmux runs in any terminal; these three can drop the title bar."
    case "$PLATFORM" in
        macos) note "brew install --cask alacritty, then run this again" ;;
        gnome | linux) note "sudo apt install alacritty (or your distribution's package), then run this again" ;;
        hyprland) note "install one with your package manager, then run this again" ;;
    esac
else
    done_ "found $TERMINAL ($TERMINAL_CMD)"
fi

ALACRITTY_CONF="$HOME/.config/nerdmux/alacritty.toml"
alacritty_config() {
    own="$HOME/.config/alacritty/alacritty.toml"
    printf '%s\n' "# Alacritty for nerdmux, written by nerdmux's setup.sh."
    if [ -f "$own" ]; then
        printf '%s\n' "# Your own ~/.config/alacritty/alacritty.toml still applies; this changes the window."
    fi
    if [ "$PLATFORM" = macos ]; then
        printf '%s\n' "# Option works as Alt, which the nerdmux keys need."
    else
        printf '%s\n' "# Move it with Super+drag, resize it with Alt+F8 or Super+middle-drag."
    fi
    if [ -f "$own" ]; then
        printf '\n[general]\nimport = ["~/.config/alacritty/alacritty.toml"]\n'
    fi
    printf '\n[window]\n'
    if [ "$PLATFORM" = macos ]; then
        printf 'decorations = "Buttonless"\noption_as_alt = "Both"\n'
    else
        printf 'decorations = "None"\n'
    fi
    if [ -n "$SIZE" ]; then
        printf 'startup_mode = "Windowed"\ndimensions = { columns = %s, lines = %s }\n' "$COLS" "$ROWS"
    else
        printf 'startup_mode = "Maximized"\n'
    fi
    # Your font, unless it lacks the icons and a Nerd Font is at hand.
    if [ -n "$FONT_FAMILY" ] && ! { [ -f "$own" ] && grep -qi 'nerd font' "$own"; }; then
        printf '\n[font]\n'
        for face in normal bold italic bold_italic; do
            printf '%s.family = "%s"\n' "$face" "$FONT_FAMILY"
        done
    fi
}

# The command the launcher runs, and the window size for kitty and Ghostty.
LAUNCH=""
KITTY_SIZE=""
GHOSTTY_SIZE=""
if [ -n "$SIZE" ]; then
    KITTY_SIZE="-o remember_window_size=no -o initial_window_width=${COLS}c -o initial_window_height=${ROWS}c "
    GHOSTTY_SIZE="--window-width=$COLS --window-height=$ROWS "
fi
if [ -n "$TERMINAL" ]; then
    case "$PLATFORM:$TERMINAL" in
        hyprland:alacritty)
            # Hyprland draws no title bars; the usual Alacritty is right.
            LAUNCH="$TERMINAL_CMD --class nerdmux -e $NERDMUX"
            ;;
        macos:alacritty)
            # --class is for X11 and Wayland only.
            put_file "$ALACRITTY_CONF" "$(alacritty_config)"
            LAUNCH="$TERMINAL_CMD --config-file $ALACRITTY_CONF -e $NERDMUX"
            ;;
        *:alacritty)
            put_file "$ALACRITTY_CONF" "$(alacritty_config)"
            LAUNCH="$TERMINAL_CMD --config-file $ALACRITTY_CONF --class nerdmux -e $NERDMUX"
            ;;
        hyprland:kitty) LAUNCH="$TERMINAL_CMD --class nerdmux $NERDMUX" ;;
        *:kitty) LAUNCH="$TERMINAL_CMD -o hide_window_decorations=yes ${KITTY_SIZE}--class nerdmux $NERDMUX" ;;
        hyprland:ghostty) LAUNCH="$TERMINAL_CMD --class=io.github.ndles.nerdmux -e $NERDMUX" ;;
        *:ghostty) LAUNCH="$TERMINAL_CMD --window-decoration=none ${GHOSTTY_SIZE}--class=io.github.ndles.nerdmux -e $NERDMUX" ;;
    esac
fi

# ----- 4. launcher and icon ---------------------------------------------------

step "4. A launcher with the nerdmux icon"
if [ -z "$LAUNCH" ]; then
    note "skipped: no terminal to launch nerdmux in"
elif [ "$PLATFORM" = macos ]; then
    app="$HOME/Applications/nerdmux.app"
    put_file "$app/Contents/Info.plist" "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
    <key>CFBundleName</key><string>nerdmux</string>
    <key>CFBundleIdentifier</key><string>io.github.ndles.nerdmux</string>
    <key>CFBundleExecutable</key><string>nerdmux</string>
    <key>CFBundleIconFile</key><string>nerdmux</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>1</string>
</dict>
</plist>"
    # Apps opened from Finder get a bare PATH; a login shell reads the
    # profile (Homebrew and the like), as Terminal.app would.
    put_file "$app/Contents/MacOS/nerdmux" "#!/bin/sh
# Opens nerdmux in Alacritty; written by nerdmux's setup.sh.
exec \"\${SHELL:-/bin/zsh}\" -l -c 'exec $LAUNCH'"
    run chmod +x "$app/Contents/MacOS/nerdmux"
    icns="$app/Contents/Resources/nerdmux.icns"
    if [ -f "$icns" ]; then
        note "unchanged: $icns"
    elif [ "$DRY_RUN" = 1 ]; then
        say "would build $icns from $ICONS/icon-*.png"
    else
        set_dir=$(mktemp -d)/nerdmux.iconset
        mkdir -p "$set_dir"
        for s in 16 32 128 256 512; do
            cp "$ICONS/icon-$s.png" "$set_dir/icon_${s}x${s}.png"
        done
        for s in 16 32 128 256; do
            cp "$ICONS/icon-$((s * 2)).png" "$set_dir/icon_${s}x${s}@2x.png"
        done
        cp "$ICONS/icon-512.png" "$set_dir/icon_512x512@2x.png"
        mkdir -p "$(dirname "$icns")"
        iconutil -c icns -o "$icns" "$set_dir"
        rm -rf "$(dirname "$set_dir")"
        done_ "built $icns"
        CHANGED=1
    fi
    [ "$DRY_RUN" = 1 ] || done_ "nerdmux is in ~/Applications; open it from Launchpad or Spotlight"
else
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    icons="$data/icons/hicolor"
    put_copy "$ICONS/icon.svg" "$icons/scalable/apps/nerdmux.svg"
    for s in 16 24 32 48 64 128 256 512; do
        put_copy "$ICONS/icon-$s.png" "$icons/${s}x${s}/apps/nerdmux.png"
    done
    [ "$DRY_RUN" = 1 ] || done_ "icons in $icons"
    case "$TERMINAL" in ghostty) class=io.github.ndles.nerdmux ;; *) class=nerdmux ;; esac
    put_file "$data/applications/nerdmux.desktop" "[Desktop Entry]
Type=Application
Name=nerdmux
Comment=Terminal sessions, windows and panes
Exec=$LAUNCH
Icon=nerdmux
Terminal=false
Categories=System;TerminalEmulator;
StartupNotify=true
StartupWMClass=$class"
    if [ "$DRY_RUN" = 0 ]; then
        if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q -f -t "$icons" || true; fi
        if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database -q "$data/applications" || true; fi
    fi
    [ "$DRY_RUN" = 1 ] || done_ "nerdmux is in your app launcher"
fi

# ----- 5. desktop details -----------------------------------------------------

step "5. Desktop details"
case "$PLATFORM" in
    gnome)
        held=$(gsettings get org.gnome.desktop.wm.keybindings activate-window-menu 2>/dev/null || true)
        if printf '%s' "$held" | grep -qi '<alt>space'; then
            say "GNOME opens its window menu on Alt+Space, the nerdmux leader; until it is freed, nerdmux uses"
            say "Ctrl+Space. The window menu stays on a right click of the title bar."
            if ask "Free Alt+Space in GNOME?"; then
                run gsettings set org.gnome.desktop.wm.keybindings activate-window-menu "[]"
                note "undo: gsettings reset org.gnome.desktop.wm.keybindings activate-window-menu"
                CHANGED=1
            fi
        else
            done_ "Alt+Space is free for nerdmux"
        fi
        ;;
    hyprland)
        done_ "Hyprland leaves Alt+Space to nerdmux and draws no title bars"
        note "if your terminal pads its window already, padding = 0 and padding_rows = 0 in"
        note "$HOME/.config/nerdmux/config.toml give that space back to the panes"
        ;;
    macos)
        done_ "Spotlight is on Cmd+Space; Alt+Space is free for nerdmux"
        ;;
    linux)
        note "if your desktop takes Alt+Space, free it there or set prefix in ~/.config/nerdmux/config.toml"
        ;;
esac

if [ "$DRY_RUN" = 1 ]; then
    printf '\n%sDry run: nothing was changed.%s\n' "$BOLD" "$OFF"
elif [ "$CHANGED" = 1 ]; then
    printf '\n%sDone.%s Open nerdmux from your launcher.\n' "$BOLD" "$OFF"
else
    printf '\n%sEverything was already in place.%s\n' "$BOLD" "$OFF"
fi
