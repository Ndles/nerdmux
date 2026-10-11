//! nerdmux: a terminal multiplexer. One binary is both the client you type
//! into and the background server that keeps sessions alive.

use nerdmux::proto::Request;
use nerdmux::{client, config, keys, server};
use std::path::PathBuf;

const USAGE: &str = "\
nerdmux - terminal sessions, windows and panes with easy keyboard navigation

USAGE
  nerdmux                          attach to the last session, or start one
  nerdmux new [-s NAME] [-d] [-- COMMAND...]
                                start a session (-d: do not attach)
  nerdmux attach [-t NAME]         attach to a session        (alias: a)
  nerdmux ls                       list sessions
  nerdmux kill-session -t NAME     end a session and everything in it
  nerdmux kill-server              end every session
  nerdmux restore                  bring back sessions saved before a crash
                                or reboot
  nerdmux action [-t NAME] ACTION  run an action in a session, e.g.
                                nerdmux action split-right
  nerdmux keys                     print the key bindings
  nerdmux config                   print an example config file
  nerdmux config --path            print where the config file lives

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

fn main() {
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
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-S" | "--socket" => {
                if i + 1 >= args.len() {
                    fail("--socket needs a path");
                }
                socket = PathBuf::from(args.remove(i + 1));
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
        other => fail(&format!("unknown command {other:?}")),
    };

    match client::run(&socket, request) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("nerdmux: {e:#}");
            std::process::exit(1);
        }
    }
}
