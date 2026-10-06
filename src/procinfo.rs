//! Small per-OS helpers for asking about another process.

use std::path::PathBuf;

/// Short name of the program running as `pid` (`vim`, `zsh`, ...).
#[cfg(target_os = "linux")]
pub fn name(pid: i32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Current working directory of `pid`.
#[cfg(target_os = "linux")]
pub fn cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
pub fn name(pid: i32) -> Option<String> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is valid for `buf.len()` bytes for the duration of the call.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = std::str::from_utf8(&buf[..n as usize]).ok()?;
    let base = path.rsplit('/').next()?;
    (!base.is_empty()).then(|| base.to_string())
}

#[cfg(target_os = "macos")]
pub fn cwd(pid: i32) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    // SAFETY: the struct is plain data, so all-zero is a valid value, and the
    // kernel writes at most `size` bytes into it.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut libc::proc_vnodepathinfo as *mut libc::c_void,
            size as libc::c_int,
        )
    };
    if n <= 0 || (n as usize) < size {
        return None;
    }
    // `vip_path` is declared as [[c_char; 32]; 32]; it is one 1024-byte C string.
    let raw = &info.pvi_cdir.vip_path;
    // SAFETY: `raw` is 32 * 32 contiguous bytes inside `info`.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(raw.as_ptr() as *const u8, 32 * 32) };
    let len = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    (len > 0).then(|| PathBuf::from(OsStr::from_bytes(&bytes[..len])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn name(_pid: i32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd(_pid: i32) -> Option<PathBuf> {
    None
}

/// The git branch checked out in the repository containing `dir`: the
/// branch name, or a short commit id when the head is detached.
pub fn git_branch(dir: &std::path::Path) -> Option<String> {
    let mut at = Some(dir);
    while let Some(d) = at {
        let dot_git = d.join(".git");
        let git_dir = if dot_git.is_dir() {
            Some(dot_git)
        } else if dot_git.is_file() {
            // Worktrees and submodules: ".git" is a file naming the real directory.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let path = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
            Some(if path.is_absolute() { path } else { d.join(path) })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: ") {
                Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_string(),
                None => head.chars().take(7).collect(),
            });
        }
        at = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_branch_from_a_subdirectory() {
        let root = std::env::temp_dir().join(format!("nerdmux-git-{}", std::process::id()));
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(git_branch(&sub).as_deref(), Some("feature/x"));
        std::fs::write(root.join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(git_branch(&root).as_deref(), Some("0123456"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
