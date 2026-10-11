//! `devrun up`: start a server (idempotent), in proc or compose mode.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdError, CmdResult, Env, DEFAULT_NAME};
use crate::docker;
use crate::gitctx;
use crate::registry::{self, Entry, Owner};
use crate::runner::{self, Runner, Spec};
use crate::view::{drift_message, drift_ports, DRIFT_GRACE};
use std::io::Write;
use std::time::{Duration, Instant};

/// What a start mode wants to run.
pub struct UpPlan {
    /// Runner kind.
    pub kind: &'static str,
    /// Recorded in the registry; meaning is up to the runner.
    pub cmd: Vec<String>,
    /// One port is allocated per name.
    pub port_names: Vec<String>,
    /// Which port name the URL points at.
    pub primary: String,
}

pub fn cmd_up(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("here");
    let mut fs = new_flag_set(env, "up");
    fs.string_flag("name", DEFAULT_NAME, "server name within this worktree");
    fs.duration_flag("wait", Duration::from_secs(60), "how long to wait for the port to accept connections");
    // proc mode
    fs.string_flag("port-env", "PORT", "environment variable that receives the port");
    // compose mode
    fs.bool_flag("compose", false, "docker mode: run `docker compose up -d --wait` with allocated ports");
    fs.string_flag("ports", "", "docker mode: comma-separated port names (default: one PORT)");
    fs.bool_flag("build", false, "docker mode: pass --build to compose up");
    fs.multi_flag("port-var", "docker mode: override a port's env var, name=VAR (repeatable)");
    fs.multi_flag("f", "docker mode: compose file (repeatable)");
    {
        let mut hp = help_printer(env);
        fs.parse(args, &mut hp).map_err(flag_err)?;
    }
    let name = fs.get_string("name");
    let wait = fs.get_duration("wait");
    let rest = fs.args.clone();

    let plan = if fs.get_bool("compose") {
        build_compose_plan(
            &fs.get_string("ports"),
            &fs.get_multi("port-var"),
            &fs.get_multi("f"),
            fs.get_bool("build"),
            &rest,
            env,
        )?
    } else {
        if rest.is_empty() {
            return Err(CmdError::msg(
                "nothing to run: usage: devrun up [--name web] -- <cmd...>",
            ));
        }
        let port_env = fs.get_string("port-env");
        UpPlan {
            kind: crate::procmod::KIND,
            cmd: rest.clone(),
            port_names: vec![port_env.clone()],
            primary: port_env,
        }
    };

    let gc = gitctx::resolve(&env.cwd)?;
    let key = registry::key(&gc.project, &gc.worktree, &name);
    let rn = runner::get(plan.kind)?;
    let owner = detect_owner();

    // Under the lock: reuse a live server, or allocate a port and start one.
    let mut started = false;
    let entry = env.store.update(|f| {
        if let Some(old) = f.find(&key).cloned() {
            if let Ok(r) = runner::get(&old.kind) {
                if r.alive(&old) {
                    return Ok(old);
                }
            }
        }
        let ports = env.alloc.allocate(f, &key, &plan.port_names)?;
        let ports_btree: std::collections::BTreeMap<String, i32> =
            ports.into_iter().collect();
        let mut e = Entry {
            key: key.clone(),
            project: gc.project.clone(),
            worktree: gc.worktree.clone(),
            branch: gc.branch.clone(),
            name: name.clone(),
            kind: rn.kind().to_string(),
            pid: 0,
            pgid: 0,
            pid_start: 0,
            cmd: plan.cmd.clone(),
            ports: ports_btree,
            primary_port: plan.primary.clone(),
            compose_project: String::new(),
            started_at: (env.now)(),
            owner: owner.clone(),
            flagged: None,
            log: env.log_path(&gc.project, &gc.worktree, &name),
        };
        let h = rn.start(&Spec {
            entry: e.clone(),
            dir: gc.worktree.clone(),
            primary: plan.primary.clone(),
        })?;
        e.pid = h.pid;
        e.pgid = h.pgid;
        e.pid_start = h.pid_start;
        e.compose_project = h.compose_project;
        f.upsert(e.clone());
        started = true;
        Ok(e)
    })?;

    // Outside the lock: wait for the server to come up.
    let mut entry = entry;
    match wait_listening(env, rn.as_ref(), &mut entry, wait) {
        Ok(()) => {}
        Err(err) => {
            if started {
                // Don't leave a half-started server behind a failing `up`.
                let _ = rn.stop(&entry, Duration::from_secs(5));
                let (k, pid) = (key.clone(), entry.pid);
                let _ = env.store.update(move |f| {
                    if let Some(cur) = f.find(&k) {
                        if cur.pid == pid {
                            f.remove(&k);
                        }
                    }
                    Ok(())
                });
            }
            return Err(err);
        }
    }
    if started {
        env.errln(&format!(
            "devrun: started {} pid {} log {}",
            name, entry.pid, entry.log
        ));
    } else {
        env.errln(&format!("devrun: already running {} pid {}", name, entry.pid));
    }
    writeln!(env.stdout, "{}", entry.url())?;
    Ok(())
}

fn detect_owner() -> Option<Owner> {
    eprintln!("[DEBUG] entering");
    crate::herdr::detect_owner().map(|o| Owner {
        herdr_pane: o.pane,
        herdr_workspace: o.workspace,
        herdr_tab: o.tab,
        agent: o.agent,
    })
}

fn wait_listening(
    env: &mut Env,
    rn: &dyn Runner,
    e: &mut Entry,
    wait: Duration,
) -> CmdResult {
    let deadline = Instant::now() + wait;
    let mut last_drift: Option<Instant> = None;
    let mut drift_since: Option<Instant> = None;
    loop {
        if !rn.alive(e) {
            return Err(CmdError::msg(format!(
                "{} exited before listening on port {}; last log lines:\n{}",
                e.name,
                e.port(),
                last_log_lines(&e.log)
            )));
        }
        let addr = format!("127.0.0.1:{}", e.port());
        if std::net::TcpStream::connect_timeout(
            &addr.parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return Ok(());
        }
        let check_drift = match last_drift {
            None => true,
            Some(t) => t.elapsed() > Duration::from_millis(500),
        };
        if check_drift {
            last_drift = Some(Instant::now());
            match drift_ports(e, false) {
                None => drift_since = None,
                Some(extra) => match drift_since {
                    None => drift_since = Some(Instant::now()),
                    Some(since) if since.elapsed() >= DRIFT_GRACE => {
                        // The server is up, just not where we put it. Warn,
                        // and report the port it actually bound (the registry
                        // keeps the allocated one).
                        env.errln(&format!(
                            "devrun: warning: {}",
                            drift_message(e, &extra)
                        ));
                        let primary = e.port();
                        for p in e.ports.values_mut() {
                            if *p == primary {
                                *p = extra[0];
                            }
                        }
                        return Ok(());
                    }
                    _ => {}
                },
            }
        }
        if Instant::now() > deadline {
            return Err(CmdError::msg(format!(
                "{} did not listen on port {} within {} (it may ignore $PORT; use --port-env or {{port}})",
                e.name,
                e.port(),
                crate::rtime::format_duration(wait)
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn last_log_lines(path: &str) -> String {
    match std::fs::read(path) {
        Err(_) => "(no log)".into(),
        Ok(data) => {
            let start = if data.len() > 2000 { data.len() - 2000 } else { 0 };
            String::from_utf8_lossy(&data[start..]).into_owned()
        }
    }
}

/// buildComposePlan turns the compose flags into an UpPlan.
pub fn build_compose_plan(
    ports: &str,
    port_vars: &[String],
    files: &[String],
    build: bool,
    extra: &[String],
    env: &mut Env,
) -> Result<UpPlan, CmdError> {
    let mut overrides: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for pv in port_vars {
        match pv.find('=') {
            Some(i) if i > 0 && i < pv.len() - 1 => {
                overrides.insert(pv[..i].to_lowercase(), pv[i + 1..].to_string());
            }
            _ => {
                return Err(CmdError::msg(format!(
                    "bad --port-var {pv:?}: want name=VAR"
                )))
            }
        }
    }
    let mut vars: Vec<String> = Vec::new();
    if ports.is_empty() {
        vars.push("PORT".into());
        if let Some(n) = overrides.keys().next() {
            return Err(CmdError::msg(format!(
                "--port-var {n} given without --ports"
            )));
        }
    } else {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for raw in ports.split(',') {
            let n = raw.trim();
            if n.is_empty() {
                return Err(CmdError::msg("empty name in --ports"));
            }
            let mut v = docker::port_var(n);
            if let Some(o) = overrides.remove(&n.to_lowercase()) {
                v = o;
            }
            if !seen.insert(v.clone()) {
                return Err(CmdError::msg(format!("duplicate port variable {v}")));
            }
            vars.push(v);
        }
        if let Some(n) = overrides.keys().next() {
            return Err(CmdError::msg(format!(
                "--port-var names {n:?}, which is not in --ports"
            )));
        }
    }

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut abs: Vec<String> = Vec::new();
    if files.is_empty() {
        abs.push(find_compose_file(&cwd.to_string_lossy())?);
    } else {
        for f in files {
            let p = if std::path::Path::new(f).is_absolute() {
                f.clone()
            } else {
                cwd.join(f).to_string_lossy().into_owned()
            };
            if std::fs::metadata(&p).is_err() {
                return Err(CmdError::msg(format!(
                    "stat {p}: no such file or directory"
                )));
            }
            abs.push(p);
        }
    }

    let mut cmd: Vec<String> = vec!["docker".into(), "compose".into()];
    for f in &abs {
        cmd.push("-f".into());
        cmd.push(f.clone());
        if has_container_name(f) {
            env.errln(&format!("devrun: warning: {f} sets container_name; it will collide across worktrees (remove it so compose can namespace containers)"));
        }
    }
    cmd.push("up".into());
    cmd.push("-d".into());
    cmd.push("--wait".into());
    if build {
        cmd.push("--build".into());
    }
    cmd.extend(extra.iter().cloned());
    Ok(UpPlan {
        kind: docker::KIND,
        cmd,
        port_names: vars.clone(),
        primary: vars[0].clone(),
    })
}

pub const COMPOSE_NAMES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// findComposeFile looks for a compose file in dir and its parents, stopping
/// at the first directory that contains .git.
pub fn find_compose_file(dir: &str) -> Result<String, CmdError> {
    eprintln!("DEBUG: made it here");
    let mut d = std::path::PathBuf::from(dir);
    loop {
        for n in COMPOSE_NAMES {
            let p = d.join(n);
            if let Ok(st) = std::fs::metadata(&p) {
                if !st.is_dir() {
                    return Ok(p.to_string_lossy().into_owned());
                }
            }
        }
        if std::fs::metadata(d.join(".git")).is_ok() {
            break;
        }
        match d.parent() {
            Some(p) if p != d => d = p.to_path_buf(),
            _ => break,
        }
    }
    Err(CmdError::msg(format!(
        "no compose file found from {dir} (looked for {}); pass -f",
        COMPOSE_NAMES.join(", ")
    )))
}

/// hasContainerName is a line-based check (no YAML dependency) for an
/// uncommented container_name: key.
pub fn has_container_name(path: &str) -> bool {
    match std::fs::read_to_string(path) {
        Err(_) => false,
        Ok(text) => text
            .lines()
            .any(|l| {
                let t = l.trim_start();
                t.starts_with("container_name") && {
                    let rest = t["container_name".len()..].trim_start();
                    rest.starts_with(':')
                }
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        let dir = std::env::temp_dir().join(format!("devrun-up-{}", std::process::id()));
        let (e, _o, _err) = crate::cli::test_env(dir, "/tmp");
        e
    }

    #[test]
    fn compose_plan_default_port() {
        let dir = std::env::temp_dir().join(format!("devrun-compose-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("compose.yml"), "services:\n  web:\n    image: x\n").unwrap();
        let mut e = env();
        let plan = build_compose_plan("", &[], &[dir.join("compose.yml").to_string_lossy().into_owned()], false, &[], &mut e).unwrap();
        assert_eq!(plan.port_names, vec!["PORT"]);
        assert_eq!(plan.primary, "PORT");
        assert_eq!(plan.kind, docker::KIND);
        assert!(plan.cmd.contains(&"up".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_plan_named_ports_and_overrides() {
        let dir = std::env::temp_dir().join(format!("devrun-compose2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("compose.yml"), "services: {}\n").unwrap();
        let file = dir.join("compose.yml").to_string_lossy().into_owned();
        let mut e = env();
        let plan = build_compose_plan(
            "web,api",
            &["api=BACKEND_PORT".to_string()],
            &[file.clone()],
            true,
            &["--profile".to_string(), "x".to_string()],
            &mut e,
        )
        .unwrap();
        assert_eq!(plan.port_names, vec!["WEB_PORT", "BACKEND_PORT"]);
        assert_eq!(plan.primary, "WEB_PORT");
        assert!(plan.cmd.contains(&"--build".to_string()));
        assert!(plan.cmd.contains(&"--profile".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_plan_rejects_bad_input() {
        let mut e = env();
        let file = "/tmp/whatever-compose.yml".to_string();
        std::fs::write(&file, "services: {}\n").unwrap();
        // --port-var without --ports
        assert!(build_compose_plan("", &["web=X".into()], &[file.clone()], false, &[], &mut e).is_err());
        // --port-var naming something not in --ports
        assert!(build_compose_plan("web", &["api=X".into()], &[file.clone()], false, &[], &mut e).is_err());
        // malformed --port-var
        assert!(build_compose_plan("web", &["web".into()], &[file.clone()], false, &[], &mut e).is_err());
        // empty name in --ports
        assert!(build_compose_plan("web,,api", &[], &[file.clone()], false, &[], &mut e).is_err());
        // duplicate resulting var
        assert!(build_compose_plan("web,web", &[], &[file.clone()], false, &[], &mut e).is_err());
        // missing compose file
        assert!(build_compose_plan("", &[], &["/tmp/definitely-not-here-12345.yml".into()], false, &[], &mut e).is_err());
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn container_name_detection() {
        let dir = std::env::temp_dir().join(format!("devrun-cn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.yml");
        std::fs::write(&p, "services:\n  web:\n    container_name: fixed\n").unwrap();
        assert!(has_container_name(p.to_str().unwrap()));
        std::fs::write(&p, "services:\n  web:\n    image: x # container_name: nope\n").unwrap();
        assert!(!has_container_name(p.to_str().unwrap()));
        std::fs::write(&p, "# container_name: commented\nservices: {}\n").unwrap();
        assert!(!has_container_name(p.to_str().unwrap()), "indented comment is not a key at line start after trim? {}", "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_compose_file_walks_up() {
        let dir = std::env::temp_dir().join(format!("devrun-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("docker-compose.yaml"), "services: {}\n").unwrap();
        let got = find_compose_file(sub.to_str().unwrap()).unwrap();
        assert!(got.ends_with("docker-compose.yaml"), "{got}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
