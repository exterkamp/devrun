//! `devrun stop`: stop one server, or a whole project/worktree/everything.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdError, CmdResult, Env};
use crate::registry::Entry;
use std::io::Read;

pub fn cmd_stop(env: &mut Env, args: &[String]) -> CmdResult {
    let mut fs = new_flag_set(env, "stop");
    fs.string_flag("project", "", "stop every server of this project");
    fs.bool_flag("all", false, "stop every server");
    fs.string_flag("worktree", "", "stop every server in this worktree directory");
    fs.bool_flag("yes", false, "skip the confirmation for --project/--all/--worktree");
    let pos = {
        let mut hp = help_printer(env);
        fs.parse_interspersed(args, &mut hp).map_err(flag_err)?
    };
    let (project, all, worktree, yes) = (
        fs.get_string("project"),
        fs.get_bool("all"),
        fs.get_string("worktree"),
        fs.get_bool("yes"),
    );
    if !project.is_empty() || all || !worktree.is_empty() {
        return stop_wide(env, &project, &worktree, all, yes, &pos);
    }
    let name = pos.first().cloned().unwrap_or_default();
    let f = env.store.load()?;
    let e = env.resolve_entry(&f, "stop", &name)?;
    env.stop_entry(&e)
}

/// confirm asks a yes/no question on stderr and reads the answer from stdin.
/// No stdin or EOF means no.
pub fn confirm(env: &mut Env, prompt: &str) -> bool {
    env.errln(&format!("{prompt} [y/N] "));
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match env.stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(_) => break,
        }
    }
    let a = String::from_utf8_lossy(&buf).trim().to_lowercase();
    a == "y" || a == "yes"
}

/// stopScoped stops every server matching the scope. Each one goes through
/// its runner's Stop (via stopEntry), so process and docker kinds both work,
/// and the per-kind PID-reuse guards apply.
pub fn stop_scoped(
    env: &mut Env,
    matches: impl Fn(&Entry) -> bool,
    what: &str,
    yes: bool,
) -> CmdResult {
    let f = env.store.load()?;
    let mut targets: Vec<Entry> = f.entries.iter().filter(|e| matches(e)).cloned().collect();
    if targets.is_empty() {
        env.errln(&format!("devrun: no servers for {what}"));
        return Ok(());
    }
    targets.sort_by(|a, b| a.key.cmp(&b.key));
    if !yes {
        env.errln(&format!("About to stop {} server(s) for {what}:", targets.len()));
        for e in &targets {
            env.errln(&format!(
                "  {}/{} {} (port {}, {})",
                e.project,
                crate::gitctx::base(&e.worktree),
                e.name,
                e.port(),
                e.kind
            ));
        }
        if !confirm(env, "Stop them?") {
            return Err(CmdError::exit(1, "aborted (pass --yes to skip confirmation)"));
        }
    }
    let mut errs: Vec<String> = Vec::new();
    for e in &targets {
        if let Err(err) = env.stop_entry(e) {
            errs.push(match err {
                CmdError::Msg(m) => m,
                CmdError::Exit { msg, .. } => msg,
                CmdError::Help => String::new(),
            });
        }
    }
    if !errs.is_empty() {
        return Err(CmdError::msg(errs.join("; ")));
    }
    Ok(())
}

/// cleanPath normalises a directory for comparison with Entry.worktree.
pub fn clean_path(p: &str) -> String {
    let abs = if std::path::Path::new(p).is_absolute() {
        std::path::PathBuf::from(p)
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(p)
    };
    let abs = crate::gitctx::clean(&abs);
    match std::fs::canonicalize(&abs) {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => abs.to_string_lossy().into_owned(),
    }
}

/// stopWide handles the scoped forms of `stop`.
fn stop_wide(
    env: &mut Env,
    project: &str,
    worktree: &str,
    all: bool,
    yes: bool,
    pos: &[String],
) -> CmdResult {
    let n = [!project.is_empty(), !worktree.is_empty(), all]
        .iter()
        .filter(|b| **b)
        .count();
    if n > 1 || !pos.is_empty() {
        return Err(CmdError::msg(
            "use exactly one of: stop [name] | --project P | --worktree DIR | --all",
        ));
    }
    if all {
        return stop_scoped(env, |_| true, "all projects", yes);
    }
    if !project.is_empty() {
        let p = project.to_string();
        return stop_scoped(env, move |e| e.project == p, &format!("project {project}"), yes);
    }
    let wt = clean_path(worktree);
    let wt2 = wt.clone();
    stop_scoped(
        env,
        move |e| clean_path(&e.worktree) == wt2,
        &format!("worktree {wt}"),
        yes,
    )
}
