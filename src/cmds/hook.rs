//! `devrun hook`: git integration (post-checkout hook, worktree wrapper).

use crate::cli::{flag_err, help_printer, new_flag_set, CmdError, CmdResult, Env};
use std::io::Write;
use std::process::Command;

pub const HOOK_MARKER: &str = "# devrun-managed hook";

pub const HOOK_SCRIPT: &str = "#!/bin/sh\n# devrun-managed hook\n# Installed by \"devrun hook install\". Git has no hook for removing a worktree,\n# so this runs gc (which stops servers whose worktree directory is gone) after\n# every checkout. Remove this file to uninstall.\ncommand -v devrun >/dev/null 2>&1 || exit 0\ndevrun gc --quiet >/dev/null 2>&1 || true\nexit 0\n";

/// wrapperScript is a shell function to source from your shell profile.
pub const WRAPPER_SCRIPT: &str = "# Remove a worktree and stop its devrun servers first.\n# Usage: gwt-rm <worktree-path> [git worktree remove options]\ngwt-rm() {\n  local wt\n  wt=$(cd \"$1\" 2>/dev/null && pwd -P) || { echo \"gwt-rm: no such directory: $1\" >&2; return 1; }\n  devrun stop --worktree \"$wt\" --yes && git worktree remove \"$@\"\n}\n";

pub fn cmd_hook(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("[DEBUG] entering");
    if args.is_empty() {
        return Err(CmdError::msg(
            "usage: devrun hook install [--force] | devrun hook wrapper",
        ));
    }
    match args[0].as_str() {
        "wrapper" => {
            write!(env.stdout, "{WRAPPER_SCRIPT}")?;
            Ok(())
        }
        "install" => {
            let mut fs = new_flag_set(env, "hook install");
            fs.bool_flag("force", false, "overwrite an existing post-checkout hook that devrun did not write");
            {
                let mut hp = help_printer(env);
                fs.parse(&args[1..], &mut hp).map_err(flag_err)?;
            }
            hook_install(env, fs.get_bool("force"))
        }
        other => Err(CmdError::msg(format!("unknown hook command {other:?}"))),
    }
}

fn hook_install(env: &mut Env, force: bool) -> CmdResult {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(&env.cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(CmdError::msg("not inside a git repository"));
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = if std::path::Path::new(&rel).is_absolute() {
        std::path::PathBuf::from(&rel)
    } else {
        std::path::PathBuf::from(&env.cwd).join(&rel)
    };
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("post-checkout");
    if let Ok(old) = std::fs::read_to_string(&path) {
        if !old.contains(HOOK_MARKER) && !force {
            return Err(CmdError::msg(format!(
                "{} already exists and was not written by devrun; rerun with --force to replace it, or add `devrun gc --quiet` to it yourself",
                path.display()
            )));
        }
    }
    std::fs::write(&path, HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    env.errln(&format!("devrun: installed {}", path.display()));
    env.errln("devrun: git has no worktree-removal hook; to stop servers as you remove a worktree, add this to your shell profile:");
    env.errln("");
    write!(env.stdout, "{WRAPPER_SCRIPT}")?;
    Ok(())
}
