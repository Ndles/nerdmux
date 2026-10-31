//! The `nerdmux` command line, kept in the library so the binary is one
//! call and every other module can stay private to the crate.

use crate::proto::Request;
use crate::{client, config, keys, server};
use std::path::PathBuf;

const USAGE: &str = "\
nerdmux - terminal sessions, windows and panes with easy keyboard navigation

USAGE
  nerdmux                          attach to the last session, or start one
  nerdmux new [-s NAME] [-d] [-- COMMAND...]
                                start a session (-d: do not attach)
  nerdmux attach [-t NAME]         attach to a session        (alias: a)
  nerdmux attach --plain -t NAME   attach without a status bar or key
                                bindings, every key to the pane; makes
                                the session if needed, on a server of
                                its own unless -S is given (for nerdfleet)
  nerdmux ls                       list sessions
  nerdmux kill-session -t NAME     end a session and everything in it
  nerdmux kill-server              end every session
  nerdmux restore                  bring back sessions saved before a crash
                                or reboot
  nerdmux action [-t NAME] ACTION  run an action in a session, e.g.
                                nerdmux action split-right
  nerdmux new-window [-t NAME] [-n WINDOW] [--reuse] [-- COMMAND...]
                                open a window in a session (the current
                                one inside nerdmux); --reuse switches to
                                the window called WINDOW if there is one
  nerdmux run [-t NAME] [-n WINDOW] [--timeout SECS] -- COMMAND...
                                run a command in a new window, wait for it, and
                                print its output and exit with its code; 124
                                when it outlives --timeout and keeps running,
                                125 when nerdmux itself failed
  nerdmux send [-t TARGET|-p PANE] [--enter] [--interrupt] [--] [TEXT...]
                                type into a pane; --interrupt sends Ctrl+C first
                                and --enter presses Enter last
  nerdmux capture [-t TARGET|-p PANE] [-n LINES]
                                print a pane's text, scrollback included
  nerdmux panes [-t NAME]       list panes, one per line, tab-separated: session,
                                window number, window name, pane, active, columns,
                                rows, directory, program
  nerdmux keys                     print the key bindings
  nerdmux config                   print an example config file
  nerdmux config --path            print where the config file lives

A TARGET is SESSION or SESSION:WINDOW, the window by name or number from 1.
Inside nerdmux the default is the current pane.

OPTIONS
  -S, --socket PATH             use another server socket ($NERDMUX_SOCKET)
  -h, --help                    this text
  -V, --version                 version

Inside nerdmux, press the leader Alt+Space, then ? to list every key or p
for the command palette. If your terminal does not pass Alt through, set
another leader with `prefix` in the config.";

fn fail(msg: &str) -> ! {
    eprintln!("nerdmux: {msg}\nTry `nerdmux --help`.");
    std::process::exit(2);
}

fn print_keys() {
    let config = config::Config::load();
    let prefix = keys::key_label(config.prefix);
    let rows = keys::help_rows(&config.direct_table, &config.prefix_table);
    println!("Chords work any time; for the second column press the leader, {prefix}, then the key.\n");
    println!("  {:<18} {:<14} ", "Chord", "Leader +");
    for cat in keys::CATEGORIES {
        println!("{cat}");
        for r in rows.iter().filter(|r| r.category == cat) {
            println!("  {:<18} {:<14} {}", r.chord, r.leader, r.desc);
        }
        println!();
    }
    println!("Actions for the config file and `nerdmux action`:");
    let mut names: Vec<String> = keys::Action::palette().iter().map(|a| a.name()).collect();
    names.extend(["select-window N".to_string(), "send-prefix".to_string(), "none".to_string()]);
    println!("  {}", names.join(", "));
    for w in &config.warnings {
        eprintln!("warning: {w}");
    }
}

/// The pane this command runs in, when no other target was named.
fn current_pane(targeted: bool) -> Option<u32> {
    if targeted {
        return None;
    }
    std::env::var("NERDMUX_PANE").ok().and_then(|p| p.parse().ok())
}

fn pane_arg(arg: Option<&String>) -> u32 {
    arg.and_then(|p| p.parse().ok()).unwrap_or_else(|| fail("-p needs a pane number"))
}

/// The `nerdmux` command: parses the arguments, and runs the server or a
/// client.
pub fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Internal entry point: `nerdmux __server SOCKET` becomes the daemon.
    if args.first().map(String::as_str) == Some("__server") {
        let socket = PathBuf::from(args.get(1).cloned().unwrap_or_default());
        // SAFETY: no threads exist yet, so forking is sound. The parent
        // exits at once; the child carries on as the detached server.
        match unsafe { libc::fork() } {
            -1 => std::process::exit(1),
            0 => {}
            _ => std::process::exit(0),
        }
        if let Err(e) = server::serve(&socket) {
            eprintln!("nerdmux server: {e:#}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    // Commands that only print should end quietly when piped into `head`.
    // SAFETY: changing a signal disposition before any thread exists.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let mut socket = server::default_socket();
    let mut socket_given = std::env::var_os("NERDMUX_SOCKET").is_some_and(|s| !s.is_empty());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-S" | "--socket" => {
                if i + 1 >= args.len() {
                    fail("--socket needs a path");
                }
                socket = PathBuf::from(args.remove(i + 1));
                socket_given = true;
                args.remove(i);
            }
            "--" => break,
            _ => i += 1,
        }
    }

    let command = args.first().cloned().unwrap_or_default();
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    // `-t NAME`, `-s NAME` or a bare NAME.
    let named = |flags: &[&str], rest: &[String]| -> Option<String> {
        let mut it = rest.iter();
        let mut bare = None;
        while let Some(a) = it.next() {
            if flags.contains(&a.as_str()) {
                return Some(it.next().cloned().unwrap_or_else(|| fail(&format!("{a} needs a name"))));
            }
            if !a.starts_with('-') && bare.is_none() {
                bare = Some(a.clone());
            }
        }
        bare
    };

    let request = match command.as_str() {
        "" => Request::Attach { target: None, create: true },
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return;
        }
        "-V" | "--version" | "version" => {
            println!("nerdmux {}", env!("CARGO_PKG_VERSION"));
            println!("{}", env!("CARGO_PKG_REPOSITORY"));
            return;
        }
        "keys" => {
            print_keys();
            return;
        }
        "config" => {
            if rest.iter().any(|a| a == "--path") {
                println!("{}", config::config_path().display());
            } else {
                print!("{}", config::EXAMPLE);
            }
            return;
        }
        "new" | "new-session" => {
            let mut name = None;
            let mut detached = false;
            let mut cmd = Vec::new();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-s" | "--name" => name = Some(it.next().cloned().unwrap_or_else(|| fail("-s needs a name"))),
                    "-d" | "--detached" => detached = true,
                    "--" => {
                        cmd = it.cloned().collect();
                        break;
                    }
                    other if other.starts_with('-') => fail(&format!("unknown option {other}")),
                    other => {
                        if name.is_none() {
                            name = Some(other.to_string());
                        } else {
                            fail(&format!("unexpected argument {other}; put commands after --"));
                        }
                    }
                }
            }
            Request::New { name, command: cmd, detached }
        }
        "attach" | "attach-session" | "a" if rest.iter().any(|a| a == "--plain") => {
            let rest: Vec<String> = rest.iter().filter(|a| *a != "--plain").cloned().collect();
            match named(&["-t"], &rest) {
                Some(target) => Request::AttachPlain { target },
                None => fail("attach --plain needs a session name: -t NAME"),
            }
        }
        "attach" | "attach-session" | "a" => Request::Attach { target: named(&["-t"], &rest), create: false },
        "ls" | "list" | "list-sessions" => Request::List,
        "kill-session" | "kill" => match named(&["-t"], &rest) {
            Some(target) => Request::KillSession { target },
            None => fail("kill-session needs a session name"),
        },
        "kill-server" => Request::KillServer,
        // Inside nerdmux the current client shows the saved sessions.
        "restore" if std::env::var_os("NERDMUX").is_some_and(|v| !v.is_empty()) => {
            let pane = std::env::var("NERDMUX_PANE").ok().and_then(|p| p.parse().ok());
            Request::Action { target: None, pane, action: "restore-sessions".into() }
        }
        "restore" => Request::Restore,
        "action" => {
            let mut target = None;
            let mut words = Vec::new();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if a == "-t" {
                    target = Some(it.next().cloned().unwrap_or_else(|| fail("-t needs a name")));
                } else {
                    words.push(a.clone());
                }
            }
            if words.is_empty() {
                fail("action needs a name; `nerdmux keys` lists them");
            }
            let pane = std::env::var("NERDMUX_PANE").ok().and_then(|p| p.parse().ok());
            Request::Action { target, pane, action: words.join(" ") }
        }
        "new-window" | "neww" => {
            let (mut target, mut name, mut reuse, mut command) = (None, None, false, Vec::new());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-t" => target = Some(it.next().cloned().unwrap_or_else(|| fail("-t needs a name"))),
                    "-n" | "--name" => name = Some(it.next().cloned().unwrap_or_else(|| fail("-n needs a name"))),
                    "--reuse" => reuse = true,
                    "--" => {
                        command = it.cloned().collect();
                        break;
                    }
                    other => fail(&format!("unexpected argument {other}; put commands after --")),
                }
            }
            if reuse && name.is_none() {
                fail("--reuse needs a window name: -n WINDOW");
            }
            let pane = std::env::var("NERDMUX_PANE").ok().and_then(|p| p.parse().ok());
            Request::NewWindow { target, pane, name, reuse, command }
        }
        "run" => {
            let (mut target, mut name, mut timeout, mut command) = (None, None, None, Vec::new());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-t" => target = Some(it.next().cloned().unwrap_or_else(|| fail("-t needs a name"))),
                    "-n" | "--name" => name = Some(it.next().cloned().unwrap_or_else(|| fail("-n needs a name"))),
                    "--timeout" => {
                        let secs = it.next().and_then(|s| s.parse().ok());
                        timeout = Some(secs.unwrap_or_else(|| fail("--timeout needs a number of seconds")));
                    }
                    "--" => {
                        command = it.cloned().collect();
                        break;
                    }
                    other => fail(&format!("unexpected argument {other}; put commands after --")),
                }
            }
            if command.is_empty() {
                fail("run needs a command: nerdmux run -- COMMAND...");
            }
            Request::Run { pane: current_pane(target.is_some()), target, name, timeout, command }
        }
        "send" | "send-keys" => {
            let (mut target, mut pane, mut enter, mut interrupt, mut words) = (None, None, false, false, Vec::new());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-t" => target = Some(it.next().cloned().unwrap_or_else(|| fail("-t needs a target"))),
                    "-p" | "--pane" => pane = Some(pane_arg(it.next())),
                    "--enter" => enter = true,
                    "--interrupt" => interrupt = true,
                    "--" => {
                        words = it.cloned().collect();
                        break;
                    }
                    other if other.starts_with('-') => fail(&format!("unknown option {other}; put text after --")),
                    other => words.push(other.to_string()),
                }
            }
            let text = words.join(" ");
            if text.is_empty() && !enter && !interrupt {
                fail("send needs text, --enter or --interrupt");
            }
            let pane = pane.or_else(|| current_pane(target.is_some()));
            Request::Send { target, pane, text, enter, interrupt }
        }
        "capture" | "capture-pane" => {
            let (mut target, mut pane, mut lines) = (None, None, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-t" => target = Some(it.next().cloned().unwrap_or_else(|| fail("-t needs a target"))),
                    "-p" | "--pane" => pane = Some(pane_arg(it.next())),
                    "-n" | "--lines" => {
                        let count = it.next().and_then(|s| s.parse().ok());
                        lines = Some(count.unwrap_or_else(|| fail("-n needs a number of lines")));
                    }
                    other => fail(&format!("unexpected argument {other}")),
                }
            }
            let pane = pane.or_else(|| current_pane(target.is_some()));
            Request::Capture { target, pane, lines }
        }
        "panes" | "list-panes" => Request::Panes { target: named(&["-t"], &rest) },
        other => fail(&format!("unknown command {other:?}")),
    };

    // Plain sessions belong to the program they run inside, so they live on
    // a server of their own: not in your session list, your kill-server or
    // your restore, and not tied to the version of your running server.
    if matches!(request, Request::AttachPlain { .. }) && !socket_given {
        socket = server::runtime_dir().join("plain.sock");
    }

    // A script reads `run`'s code as its program's, so its own failures must not
    // pass for one.
    let failed = if matches!(request, Request::Run { .. }) { 125 } else { 1 };
    match client::run(&socket, request) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("nerdmux: {e:#}");
            std::process::exit(failed);
        }
    }
}
