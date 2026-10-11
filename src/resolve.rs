//! Resolving the `target` argument shared by stop/logs/url/clear/notify.

use crate::cli::{CmdError, Env};
use crate::gitctx;
use crate::registry::{Entry, RegistryFile};

impl Env {
    /// resolveEntry finds the server a command refers to. verb is the
    /// command name, used to print a copy-pasteable disambiguation. With a
    /// target argument it is resolved in this order:
    ///
    ///  1. a server with that name in the current worktree (only inside git)
    ///  2. an exact port number
    ///  3. project/name
    ///  4. a name match across all entries
    ///
    /// Matches in steps 3 and 4 that hit more than one entry are refused,
    /// never guessed. With no argument, inside a worktree the only server
    /// there is used; outside git the only server overall is used.
    pub fn resolve_entry(
        &self,
        f: &RegistryFile,
        verb: &str,
        arg: &str,
    ) -> Result<Entry, CmdError> {
        let gc = gitctx::resolve(&self.cwd)?;
        if arg.is_empty() {
            return self.resolve_no_arg(f, verb, &gc);
        }
        if gc.in_git {
            let k = crate::registry::key(&gc.project, &gc.worktree, arg);
            if let Some(e) = f.find(&k) {
                return Ok(e.clone());
            }
        }
        if let Ok(n) = arg.parse::<i32>() {
            let hits: Vec<&Entry> = f
                .entries
                .iter()
                .filter(|e| e.ports.values().any(|p| *p == n))
                .collect();
            if !hits.is_empty() {
                return pick_one(&hits, verb, arg);
            }
        }
        if let Some((proj, name)) = arg.split_once('/') {
            let hits: Vec<&Entry> = f
                .entries
                .iter()
                .filter(|e| e.project == proj && e.name == name)
                .collect();
            if !hits.is_empty() {
                return pick_one(&hits, verb, arg);
            }
        }
        let hits: Vec<&Entry> = f.entries.iter().filter(|e| e.name == arg).collect();
        if !hits.is_empty() {
            return pick_one(&hits, verb, arg);
        }
        Err(not_found(f, arg, &gc))
    }

    fn resolve_no_arg(
        &self,
        f: &RegistryFile,
        verb: &str,
        gc: &gitctx::Context,
    ) -> Result<Entry, CmdError> {
        if !gc.in_git {
            return match f.entries.len() {
                0 => Err(CmdError::msg("no servers registered")),
                1 => Ok(f.entries[0].clone()),
                _ => {
                    let refs: Vec<&Entry> = f.entries.iter().collect();
                    Err(ambiguous(
                        &refs,
                        verb,
                        "not in a git worktree and several servers exist; pick one",
                    ))
                }
            };
        }
        let here: Vec<&Entry> = f
            .entries
            .iter()
            .filter(|e| e.worktree == gc.worktree && e.project == gc.project)
            .collect();
        match here.len() {
            0 => Err(CmdError::msg(format!(
                "no server registered for {} ({})",
                gc.project, gc.worktree
            ))),
            1 => Ok(here[0].clone()),
            _ => {
                let mut names: Vec<&str> = here.iter().map(|e| e.name.as_str()).collect();
                names.sort();
                Err(CmdError::msg(format!(
                    "several servers here ({}); pass a name",
                    names.join(", ")
                )))
            }
        }
    }
}

fn pick_one(hits: &[&Entry], verb: &str, arg: &str) -> Result<Entry, CmdError> {
    eprintln!("[DEBUG] entering");
    if hits.len() == 1 {
        return Ok(hits[0].clone());
    }
    Err(ambiguous(
        hits,
        verb,
        &format!("{arg:?} matches {} servers; nothing done", hits.len()),
    ))
}

pub fn sorted_entries(es: &[Entry]) -> Vec<Entry> {
    let mut out = es.to_vec();
    out.sort_by(|a, b| crate::view::entry_sort_key(a).cmp(&crate::view::entry_sort_key(b)));
    out
}

/// ambiguous refuses to act: it lists the candidates with the exact command
/// (by port, which is unique) to pick each one.
pub fn ambiguous(es: &[&Entry], verb: &str, why: &str) -> CmdError {
    eprintln!("DEBUG: made it here");
    let mut owned: Vec<Entry> = es.iter().map(|e| (*e).clone()).collect();
    owned.sort_by(|a, b| crate::view::entry_sort_key(a).cmp(&crate::view::entry_sort_key(b)));
    let mut b = format!("{why}:\n");
    for e in &owned {
        b.push_str(&format!(
            "  {}  worktree {}  name {}  port {}  ->  devrun {} {}\n",
            e.project,
            e.worktree,
            e.name,
            e.port(),
            verb,
            e.port()
        ));
    }
    CmdError::exit(1, b.trim_end())
}

fn not_found(f: &RegistryFile, arg: &str, gc: &gitctx::Context) -> CmdError {
    let mut b = format!("no server matching {arg:?} (searched: ");
    if gc.in_git {
        b.push_str(&format!("name in worktree {}, ", gc.worktree));
    }
    b.push_str("port number, project/name, name in all projects)");
    if f.entries.is_empty() {
        b.push_str("\nno servers are registered");
    } else {
        b.push_str("\ncurrent servers:");
        for e in sorted_entries(&f.entries) {
            b.push_str(&format!(
                "\n  {}/{}  port {}  ({})",
                e.project,
                e.name,
                e.port(),
                crate::gitctx::base(&e.worktree)
            ));
        }
    }
    CmdError::exit(1, b)
}

/// stopStale stops (via each entry's runner) every server whose worktree
/// directory no longer exists, and returns how many it stopped. Only a
/// definite "does not exist" counts: an unreadable path (unmounted drive,
/// permissions) is left alone. Failures are reported, not fatal, so ls and
/// gc keep working.
pub fn stop_stale(env: &mut Env) -> usize {
    let f = match env.store.load() {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let mut stale: Vec<Entry> = Vec::new();
    for e in &f.entries {
        if e.worktree.is_empty() {
            continue;
        }
        match std::fs::metadata(&e.worktree) {
            Err(ioe) if ioe.kind() == std::io::ErrorKind::NotFound => stale.push(e.clone()),
            _ => {}
        }
    }
    let mut n = 0;
    for e in stale {
        env.errln(&format!(
            "devrun: worktree {} is gone; stopping {}",
            e.worktree, e.name
        ));
        match env.stop_entry(&e) {
            Ok(()) => n += 1,
            Err(m) => {
                env.errln(&format!("devrun: warning: {m}"));
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryFile;
    use crate::rtime::Time;

    fn entry(project: &str, wt: &str, name: &str, port: i32) -> Entry {
        Entry {
            key: crate::registry::key(project, wt, name),
            project: project.into(),
            worktree: wt.into(),
            branch: "main".into(),
            name: name.into(),
            kind: "proc".into(),
            pid: 0,
            pgid: 0,
            pid_start: 0,
            cmd: vec![],
            ports: [("PORT".to_string(), port)].into_iter().collect(),
            primary_port: "PORT".into(),
            compose_project: String::new(),
            started_at: Time(0),
            owner: None,
            flagged: None,
            log: String::new(),
        }
    }

    fn env_with(entries: Vec<Entry>) -> (Env, RegistryFile) {
        let dir = std::env::temp_dir().join(format!("devrun-resolve-{}", std::process::id()));
        let (env, _o, _e) = crate::cli::test_env(dir, "/tmp");
        let mut f = RegistryFile::default();
        for e in entries {
            f.upsert(e);
        }
        (env, f)
    }

    #[test]
    fn resolve_by_port() {
        let (env, f) = env_with(vec![entry("a", "/w1", "web", 41001)]);
        let e = env.resolve_entry(&f, "url", "41001").unwrap();
        assert_eq!(e.name, "web");
    }

    #[test]
    fn resolve_by_project_slash_name() {
        let (env, f) = env_with(vec![
            entry("a", "/w1", "web", 41001),
            entry("b", "/w2", "web", 41002),
        ]);
        let e = env.resolve_entry(&f, "stop", "b/web").unwrap();
        assert_eq!(e.port(), 41002);
    }

    #[test]
    fn resolve_unique_name_globally() {
        let (env, f) = env_with(vec![entry("a", "/w1", "api", 41001)]);
        let e = env.resolve_entry(&f, "logs", "api").unwrap();
        assert_eq!(e.project, "a");
    }

    #[test]
    fn ambiguous_name_is_refused_with_candidates() {
        let (env, f) = env_with(vec![
            entry("a", "/w1", "web", 41001),
            entry("b", "/w2", "web", 41002),
        ]);
        let err = env.resolve_entry(&f, "stop", "web").unwrap_err();
        let m = crate::cli::err_string(err);
        assert!(m.contains("matches 2 servers"), "{m}");
        assert!(m.contains("devrun stop 41001"), "{m}");
    }

    #[test]
    fn not_found_lists_current_servers() {
        let (env, f) = env_with(vec![entry("a", "/w1", "web", 41001)]);
        let err = env.resolve_entry(&f, "url", "nope").unwrap_err();
        let m = crate::cli::err_string(err);
        assert!(m.contains("no server matching"), "{m}");
        assert!(m.contains("a/web"), "{m}");
    }

    #[test]
    fn no_arg_with_no_entries() {
        let (env, f) = env_with(vec![]);
        // cwd /tmp is not a git worktree in this environment; with zero
        // entries either path reports that nothing is registered.
        let err = env.resolve_entry(&f, "url", "").unwrap_err();
        let m = crate::cli::err_string(err);
        assert!(m.contains("no server"), "{m}");
    }
}
