//! The nerdmux prompt: a shell in a pane loads the user's own startup files
//! as usual, then nerdmux sets a small prompt of its own. Nothing outside
//! the server's runtime directory is touched.

use std::path::Path;

/// What the prompt shows; the status bar already has the directory and
/// branch. A mathematical sign, so fonts center it on the line like the
/// cursor, and one that nearly every monospace font has.
pub const MARK: &str = "∷";

/// Whether `mark` can go into every shell's prompt as it is: no quotes,
/// escapes or expansions, and short.
pub fn valid_mark(mark: &str) -> bool {
    !mark.is_empty() && mark.chars().count() <= 8 && !mark.chars().any(|c| c.is_control() || "'\"\\$%`!".contains(c))
}

/// Change `argv` and `env` so the shell they start ends with the nerdmux
/// prompt. `dir` holds the startup files written for it; `sgr` colors the
/// mark, written with `\e` as every shell reads it in a prompt. Shells it does not know, and
/// shells it cannot set up, are left as they are.
pub fn wrap(argv: &mut Vec<String>, env: &mut Vec<(String, String)>, dir: &Path, sgr: &str, mark: &str) {
    let shell = argv[0].rsplit('/').next().unwrap_or("").trim_start_matches('-');
    let written = match shell {
        "bash" => bash(argv, dir, sgr, mark),
        "zsh" => zsh(env, dir, sgr, mark),
        "fish" => fish(argv, sgr, mark),
        _ => Ok(()),
    };
    // Without its files the shell still starts, with its own prompt.
    let _ = written;
}

/// bash reads `--rcfile` in place of `~/.bashrc` (and still reads
/// `/etc/bash.bashrc`), so the file reads `~/.bashrc` itself, then sets PS1.
fn bash(argv: &mut Vec<String>, dir: &Path, sgr: &str, mark: &str) -> std::io::Result<()> {
    // A login shell, or one told what to read, never reads an rcfile.
    let own = ["-l", "--login", "--rcfile", "--init-file", "--norc", "--posix", "-c", "-s"];
    if argv[1..].iter().any(|a| own.contains(&a.as_str())) {
        return Ok(());
    }
    let rc = dir.join("bashrc");
    write(
        &rc,
        &format!(
            "# Written by nerdmux: your own ~/.bashrc, then the nerdmux prompt.\n\
             [ -f ~/.bashrc ] && . ~/.bashrc\n\
             PS1='\\[{sgr}\\]{mark}\\[\\e[0m\\] '\n"
        ),
    )?;
    argv.splice(1..1, ["--rcfile".to_string(), rc.to_string_lossy().into_owned()]);
    Ok(())
}

/// zsh finds its startup files through ZDOTDIR. Ours puts the user's back
/// at once and reads their .zshenv, so the rest load from where they always
/// do; the prompt is set before the first prompt, after .zshrc.
fn zsh(env: &mut Vec<(String, String)>, dir: &Path, sgr: &str, mark: &str) -> std::io::Result<()> {
    let zdir = dir.join("zsh");
    std::fs::create_dir_all(&zdir)?;
    write(
        &zdir.join(".zshenv"),
        &format!(
            "# Written by nerdmux: your own startup files, then the nerdmux prompt.\n\
             if [[ -n ${{NERDMUX_ZDOTDIR+x}} ]]; then ZDOTDIR=$NERDMUX_ZDOTDIR; else unset ZDOTDIR; fi\n\
             unset NERDMUX_ZDOTDIR\n\
             [[ -f ${{ZDOTDIR:-$HOME}}/.zshenv ]] && source ${{ZDOTDIR:-$HOME}}/.zshenv\n\
             if [[ -o interactive ]]; then\n\
             \x20 _nerdmux_prompt() {{\n\
             \x20   PROMPT=$'%{{{sgr}%}}{mark}%{{\\e[0m%}} '\n\
             \x20   precmd_functions=(${{precmd_functions:#_nerdmux_prompt}})\n\
             \x20   unfunction _nerdmux_prompt\n\
             \x20 }}\n\
             \x20 precmd_functions+=(_nerdmux_prompt)\n\
             fi\n"
        ),
    )?;
    if let Some((_, v)) = env.iter().find(|(k, _)| k == "ZDOTDIR") {
        let v = v.clone();
        env.push(("NERDMUX_ZDOTDIR".into(), v));
    }
    env.retain(|(k, _)| k != "ZDOTDIR");
    env.push(("ZDOTDIR".into(), zdir.to_string_lossy().into_owned()));
    Ok(())
}

/// fish runs `--init-command` after its configuration.
fn fish(argv: &mut Vec<String>, sgr: &str, mark: &str) -> std::io::Result<()> {
    argv.insert(1, format!("--init-command=function fish_prompt; printf '{sgr}{mark}\\e[0m '; end"));
    Ok(())
}

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SGR: &str = "\\e[1;38;2;232;162;58m";

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nerdmux-prompt-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bash_reads_bashrc_then_sets_the_prompt() {
        let d = dir("bash");
        let mut argv = vec!["/bin/bash".to_string()];
        wrap(&mut argv, &mut Vec::new(), &d, SGR, MARK);
        assert_eq!(argv[1], "--rcfile");
        let rc = std::fs::read_to_string(&argv[2]).unwrap();
        assert!(rc.find(". ~/.bashrc").unwrap() < rc.find("PS1=").unwrap());
    }

    #[test]
    fn a_login_bash_is_left_alone() {
        let mut argv = vec!["bash".to_string(), "-l".to_string()];
        wrap(&mut argv, &mut Vec::new(), &dir("login"), SGR, MARK);
        assert_eq!(argv, ["bash", "-l"]);
    }

    #[test]
    fn zsh_keeps_the_users_zdotdir() {
        let d = dir("zsh");
        let mut env = vec![("ZDOTDIR".to_string(), "/home/u/.config/zsh".to_string())];
        let mut argv = vec!["zsh".to_string()];
        wrap(&mut argv, &mut env, &d, SGR, MARK);
        assert_eq!(argv, ["zsh"]);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("NERDMUX_ZDOTDIR").as_deref(), Some("/home/u/.config/zsh"));
        assert!(get("ZDOTDIR").unwrap().ends_with("/zsh"));
        assert!(d.join("zsh/.zshenv").is_file());
    }

    #[test]
    fn marks_that_a_shell_would_expand_are_refused() {
        assert!(valid_mark(MARK) && valid_mark("❯") && valid_mark(">>"));
        for bad in ["", "$", "a'b", "%", "!", "`x`", "\\", "123456789"] {
            assert!(!valid_mark(bad), "{bad:?}");
        }
    }

    #[test]
    fn other_shells_are_left_alone() {
        let mut argv = vec!["/usr/bin/nu".to_string()];
        let mut env = Vec::new();
        wrap(&mut argv, &mut env, &dir("nu"), SGR, MARK);
        assert_eq!(argv, ["/usr/bin/nu"]);
        assert!(env.is_empty());
    }
}
