//! `devrun ls`: list servers.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdResult, Env, STATE_DEAD};
use crate::render::{paint, tab_write, ANSI_BOLD, ANSI_YELLOW};
use crate::resolve::stop_stale;
use crate::rtime::human_age;
use crate::view::{view_sort_key, View};
use std::io::Write;
use std::time::Duration;

pub fn cmd_ls(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("[DEBUG] entering");
    let mut fs = new_flag_set(env, "ls");
    fs.string_flag("project", "", "only show this project");
    fs.bool_flag("json", false, "machine-readable output");
    fs.bool_flag("all", false, "include dead servers");
    {
        let mut hp = help_printer(env);
        fs.parse(args, &mut hp).map_err(flag_err)?;
    }
    let project = fs.get_string("project");
    let as_json = fs.get_bool("json");
    let all = fs.get_bool("all");

    stop_stale(env);
    let f = env.store.load()?;
    let listening = (env.listening)()?;
    let mut views: Vec<View> = Vec::new();
    for e in &f.entries {
        if !project.is_empty() && e.project != project {
            continue;
        }
        let v = env.view(e, &listening);
        if v.state == STATE_DEAD && !all {
            continue;
        }
        views.push(v);
    }
    views.sort_by(|a, b| view_sort_key(a).cmp(&view_sort_key(b)));

    if as_json {
        let mut buf = serde_json::to_string_pretty(&views).map_err(|e| e.to_string())?;
        buf.push('\n');
        env.stdout.write_all(buf.as_bytes())?;
        return Ok(());
    }
    if views.is_empty() {
        env.errln("no servers running");
        return Ok(());
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    rows.push(
        ["PROJECT", "WORKTREE", "BRANCH", "NAME", "PORT", "URL", "PID", "KIND", "STATE", "AGE", "OWNER"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
    for v in &views {
        let owner = match &v.owner {
            Some(o) => o.herdr_pane.clone(),
            None => String::new(),
        };
        let flag = if v.flagged.is_some() { " !" } else { "" };
        rows.push(vec![
            v.project.clone(),
            crate::gitctx::base(&v.worktree),
            v.branch.clone(),
            v.name.clone(),
            v.port.to_string(),
            v.url.clone(),
            v.pid.to_string(),
            v.kind.clone(),
            format!("{}{}", v.state, flag),
            human_age(Duration::from_secs(v.age_seconds.max(0) as u64)),
            owner,
        ]);
    }
    // Highlight flagged rows after alignment so escapes don't skew widths.
    let mut tbl: Vec<u8> = Vec::new();
    tab_write(&mut tbl, &rows)?;
    let text = String::from_utf8_lossy(&tbl);
    let color = crate::render::use_color(crate::render::is_tty_stdout());
    for (i, row) in text.trim_end_matches('\n').split('\n').enumerate() {
        if i > 0 && views[i - 1].flagged.is_some() {
            writeln!(env.stdout, "{}", paint(color, &format!("{ANSI_BOLD}{ANSI_YELLOW}"), row))?;
        } else {
            writeln!(env.stdout, "{row}")?;
        }
    }
    for v in &views {
        for w in &v.warnings {
            env.errln(&format!(
                "devrun: warning: {}/{}: {}",
                v.project,
                crate::gitctx::base(&v.worktree),
                w
            ));
        }
    }
    Ok(())
}
