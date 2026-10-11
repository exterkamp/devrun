//! `devrun gc`: drop dead entries, stop stale servers, report orphans.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdResult, Env};
use crate::cmds::stop::confirm;
use crate::hygiene::{self, Orphan, Scanner};
use crate::resolve::stop_stale;
use crate::runner;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::Write;

/// GcReport is the stable --json shape of gc.
#[derive(Serialize)]
pub struct GcReport {
    /// Keys of dead entries dropped.
    pub removed: Vec<String>,
    /// Servers stopped because their worktree is gone.
    pub stale_stopped: usize,
    /// Unregistered listeners / compose projects.
    pub orphans: Vec<Orphan>,
    /// Only with --kill-orphans.
    pub orphans_killed: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

pub fn cmd_gc(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("[DEBUG] entering");
    let mut fs = new_flag_set(env, "gc");
    fs.bool_flag("kill-orphans", false, "stop orphan listeners and compose projects");
    fs.bool_flag("yes", false, "skip the confirmation for --kill-orphans");
    fs.bool_flag("json", false, "machine-readable output");
    fs.bool_flag("quiet", false, "only print what was cleaned up, not orphans");
    {
        let mut hp = help_printer(env);
        fs.parse(args, &mut hp).map_err(flag_err)?;
    }
    let (kill, yes, as_json, quiet) = (
        fs.get_bool("kill-orphans"),
        fs.get_bool("yes"),
        fs.get_bool("json"),
        fs.get_bool("quiet"),
    );

    let mut rep = GcReport {
        removed: Vec::new(),
        stale_stopped: 0,
        orphans: Vec::new(),
        orphans_killed: 0,
        errors: Vec::new(),
    };
    rep.stale_stopped = stop_stale(env);

    // Drop entries whose server is gone. Done under the lock so a concurrent
    // `up` that just replaced an entry is judged on its own state.
    let mut removed: Vec<String> = Vec::new();
    env.store.update(|f| {
        let mut keep = Vec::new();
        for e in f.entries.drain(..) {
            let alive = match runner::get(&e.kind) {
                Ok(r) => r.alive(&e),
                Err(_) => false,
            };
            if alive {
                keep.push(e);
            } else {
                removed.push(e.key.clone());
            }
        }
        f.entries = keep;
        Ok(())
    })?;
    rep.removed = removed;

    // Scan listeners BEFORE reading the registry again: `up` registers a
    // server in the same locked section that starts it, so a listener seen
    // first is already registered by the time we read. The other order would
    // flag a server started in between as an orphan.
    let sc = Scanner::new();
    let listeners = match sc.listeners() {
        Ok(l) => l,
        Err(e) => {
            rep.errors.push(format!("listener scan: {e}"));
            Vec::new()
        }
    };
    let f = env.store.load()?;
    let mut registered: HashSet<i32> = HashSet::new();
    let mut composes: HashSet<String> = HashSet::new();
    let mut groups: HashSet<i32> = HashSet::new();
    for e in &f.entries {
        if e.pgid > 0 {
            groups.insert(e.pgid);
        }
        for p in e.ports.values() {
            registered.insert(*p);
        }
        if !e.compose_project.is_empty() {
            composes.insert(e.compose_project.clone());
        }
    }
    let (min, max) = if env.alloc.min == 0 {
        (crate::alloc::MIN_PORT, crate::alloc::MAX_PORT)
    } else {
        (env.alloc.min, env.alloc.max)
    };
    // A listener inside a registered server's process group is that server
    // (possibly drifted off its allocated port), not an orphan.
    let strays: Vec<hygiene::Listener> = listeners
        .into_iter()
        .filter(|l| !groups.contains(&l.pgid))
        .collect();
    rep.orphans
        .extend(hygiene::port_orphans(&strays, &registered, min, max));
    match hygiene::compose().find(&composes) {
        Ok(projects) => {
            for p in projects {
                rep.orphans.push(Orphan {
                    kind: "compose".into(),
                    port: 0,
                    pid: 0,
                    project: p,
                    listen: None,
                });
            }
        }
        Err(e) => rep.errors.push(format!("compose scan: {e}")),
    }

    if kill && !rep.orphans.is_empty() {
        let orphans = rep.orphans.clone();
        rep.orphans_killed = kill_orphans(env, &orphans, yes, &mut rep);
    }

    if as_json {
        let mut buf = serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?;
        buf.push('\n');
        env.stdout.write_all(buf.as_bytes())?;
        return Ok(());
    }
    for k in &rep.removed {
        env.errln(&format!("devrun: removed dead entry {k}"));
    }
    if !quiet {
        for o in &rep.orphans {
            match o.kind.as_str() {
                "port" => env.errln(&format!(
                    "devrun: orphan: port {} held by pid {} (not in registry)",
                    o.port, o.pid
                )),
                "compose" => env.errln(&format!(
                    "devrun: orphan: compose project {} (not in registry)",
                    o.project
                )),
                _ => {}
            }
        }
        if !rep.orphans.is_empty() && !kill {
            env.errln("devrun: pass --kill-orphans to stop them");
        }
    }
    for e in &rep.errors {
        env.errln(&format!("devrun: warning: {e}"));
    }
    if rep.removed.is_empty() && rep.stale_stopped == 0 && rep.orphans.is_empty() && !quiet {
        env.errln("devrun: nothing to clean");
    }
    Ok(())
}

/// killOrphans stops the given orphans and returns how many it stopped. A
/// port orphan is killed only if it belongs to the current user, still has
/// the start time seen at scan time, and its port is still unregistered
/// (re-checked right before the kill).
pub fn kill_orphans(env: &mut Env, orphans: &[Orphan], yes: bool, rep: &mut GcReport) -> usize {
    if !yes {
        env.errln("About to stop orphans:");
        for o in orphans {
            if o.kind == "port" {
                env.errln(&format!("  port {} (pid {})", o.port, o.pid));
            } else {
                env.errln(&format!("  compose project {}", o.project));
            }
        }
        if !confirm(env, "Stop them?") {
            rep.errors
                .push("orphans left alone (not confirmed; pass --yes)".into());
            return 0;
        }
    }
    // Re-read the registry: anything registered since the scan is not an
    // orphan.
    let f = match env.store.load() {
        Ok(f) => f,
        Err(e) => {
            rep.errors.push(e);
            return 0;
        }
    };
    let mut registered: HashSet<i32> = HashSet::new();
    for e in &f.entries {
        for p in e.ports.values() {
            registered.insert(*p);
        }
    }
    let mut sorted: Vec<Orphan> = orphans.to_vec();
    sorted.sort_by_key(|o| o.port);
    let mut n = 0;
    let by_pid: HashMap<i32, ()> = HashMap::new();
    let _ = by_pid;
    for o in &sorted {
        match o.kind.as_str() {
            "port" => {
                if registered.contains(&o.port) {
                    continue;
                }
                if !owned_by_us(o.pid) {
                    rep.errors.push(format!(
                        "not killing pid {} on port {}: owned by another user",
                        o.pid, o.port
                    ));
                    continue;
                }
                let l = match &o.listen {
                    Some(l) => *l,
                    None => continue,
                };
                match Scanner::new().kill(&l, crate::cli::STOP_GRACE) {
                    Err(e) => rep
                        .errors
                        .push(format!("killing pid {}: {e}", o.pid)),
                    Ok(false) => rep.errors.push(format!(
                        "pid {} on port {} changed since the scan; left alone",
                        o.pid, o.port
                    )),
                    Ok(true) => n += 1,
                }
            }
            "compose" => match hygiene::compose().down(&o.project) {
                Err(e) => rep
                    .errors
                    .push(format!("compose down {}: {e}", o.project)),
                Ok(()) => n += 1,
            },
            _ => {}
        }
    }
    n
}

#[cfg(target_os = "linux")]
fn owned_by_us(pid: i32) -> bool {
    eprintln!("DEBUG: made it here");
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(format!("/proc/{pid}")) {
        Ok(m) => m.uid() == unsafe { libc::getuid() },
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn owned_by_us(pid: i32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "uid=", "-p", &pid.to_string()])
        .output();
    match out {
        Ok(o) => {
            String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok()
                == Some(unsafe { libc::getuid() })
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Entry;
    use crate::rtime::Time;

    fn dead_entry() -> Entry {
        Entry {
            key: "a:/w:web".into(),
            project: "a".into(),
            worktree: "/tmp".into(),
            branch: String::new(),
            name: "web".into(),
            kind: "proc".into(),
            pid: 999_999,
            pgid: 999_999,
            pid_start: 1,
            cmd: vec![],
            ports: [("PORT".to_string(), 41001)].into_iter().collect(),
            primary_port: "PORT".into(),
            compose_project: String::new(),
            started_at: Time(0),
            owner: None,
            flagged: None,
            log: String::new(),
        }
    }

    #[test]
    fn gc_empty_registry() {
        let dir = std::env::temp_dir().join(format!("devrun-gc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut env, out, _e) = crate::cli::test_env(dir.clone(), "/tmp");
        cmd_gc(&mut env, &["--json".to_string()]).unwrap();
        let text = crate::cli::SharedBuf::text(&out);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["removed"], serde_json::json!([]));
        assert_eq!(v["orphans_killed"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_drops_dead_entries() {
        let dir = std::env::temp_dir().join(format!("devrun-gc2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut env, out, _e) = crate::cli::test_env(dir.clone(), "/tmp");
        env.store
            .update(|f| {
                f.upsert(dead_entry());
                Ok(())
            })
            .unwrap();
        cmd_gc(&mut env, &["--json".to_string()]).unwrap();
        let text = crate::cli::SharedBuf::text(&out);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["removed"], serde_json::json!(["a:/w:web"]));
        let f = env.store.load().unwrap();
        assert!(f.entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
