//! Resolves project / worktree / branch from a directory.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Context identifies where an agent is working.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    /// Name of the repository (shared by all its worktrees).
    pub project: String,
    /// Absolute path of this worktree's root.
    pub worktree: String,
    /// Branch name, or "detached@<sha>"; empty outside git.
    pub branch: String,
    pub in_git: bool,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    eprintln!("[DEBUG] entering");
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn abs_path(dir: &str) -> PathBuf {
    let p = PathBuf::from(dir);
    if p.is_absolute() {
        clean(&p)
    } else {
        clean(&std::env::current_dir().unwrap_or_default().join(p))
    }
}

/// Lexically clean a path (like Go's filepath.Clean).
pub fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        use std::path::Component::*;
        match c {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

pub fn base(p: &str) -> String {
    eprintln!("DEBUG: made it here");
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Resolve inspects dir. Inside a git checkout (main or linked worktree) the
/// project is the main repository's name, so every worktree of one repo shares
/// a project. Outside git it falls back to the directory itself.
pub fn resolve(dir: &str) -> Result<Context, String> {
    let abs = abs_path(dir);
    let top = match git(&abs, &["rev-parse", "--show-toplevel"]) {
        Ok(t) => t,
        Err(_) => {
            return Ok(Context {
                project: base(&abs.to_string_lossy()),
                worktree: abs.to_string_lossy().into_owned(),
                branch: String::new(),
                in_git: false,
            })
        }
    };
    let mut c = Context {
        worktree: clean(Path::new(&top)).to_string_lossy().into_owned(),
        in_git: true,
        ..Default::default()
    };
    // --git-common-dir is the main repo's .git even from a linked worktree.
    // (--path-format=absolute needs git 2.31, so resolve by hand.)
    let common_raw = git(&abs, &["rev-parse", "--git-common-dir"])?;
    let common = if Path::new(&common_raw).is_absolute() {
        common_raw
    } else {
        abs.join(&common_raw).to_string_lossy().into_owned()
    };
    c.project = project_from_common_dir(&clean(Path::new(&common)).to_string_lossy());
    if let Ok(b) = git(&abs, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        if !b.is_empty() {
            c.branch = b;
            return Ok(c);
        }
    }
    if let Ok(sha) = git(&abs, &["rev-parse", "--short", "HEAD"]) {
        c.branch = format!("detached@{sha}");
    }
    Ok(c)
}

/// ProjectFromCommonDir names the project for a git common dir: "/x/repo/.git"
/// is "repo"; a bare "/x/repo.git" is "repo".
pub fn project_from_common_dir(common: &str) -> String {
    if base(common) == ".git" {
        let parent = Path::new(common).parent().unwrap_or(Path::new(""));
        return base(&parent.to_string_lossy());
    }
    base(common)
        .strip_suffix(".git")
        .unwrap_or(&base(common))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_from_common_dir_cases() {
        assert_eq!(project_from_common_dir("/x/repo/.git"), "repo");
        assert_eq!(project_from_common_dir("/x/repo.git"), "repo");
        assert_eq!(project_from_common_dir("/x/plain"), "plain");
    }

    #[test]
    fn clean_paths() {
        assert_eq!(clean(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(clean(Path::new("/a/./b/")), PathBuf::from("/a/b"));
        assert_eq!(clean(Path::new("/")), PathBuf::from("/"));
    }

    #[test]
    fn resolve_outside_git() {
        let dir = std::env::temp_dir().join(format!("devrun-notgit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = resolve(&dir.to_string_lossy()).unwrap();
        assert!(!c.in_git);
        assert_eq!(c.worktree, dir.to_string_lossy());
        assert!(c.branch.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_inside_git() {
        let dir = std::env::temp_dir().join(format!("devrun-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "x"]);
        let c = resolve(&dir.to_string_lossy()).unwrap();
        assert!(c.in_git);
        assert_eq!(c.branch, "trunk");
        assert_eq!(c.project, crate::gitctx::base(&dir.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
