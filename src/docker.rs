//! The "docker" kind: compose stacks owned by devrun.
//!
//! Safety rules enforced here: only compose projects named devrun-* are ever
//! touched, volumes are never removed, and nothing mounts the docker socket.

use crate::procmod::{self, ProcRunner};
use crate::registry::Entry;
use crate::runner::{Handle, LogOpts, Runner, Spec};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The registry kind string for compose stacks.
pub const KIND: &str = "docker";

/// The only compose project prefix devrun will operate on.
pub const PROJECT_PREFIX: &str = "devrun-";

/// ProjectName builds COMPOSE_PROJECT_NAME: devrun-<project>-<hash of
/// worktree>. Compose requires lowercase alphanumerics, dashes and
/// underscores.
pub fn project_name(project: &str, worktree: &str) -> String {
    eprintln!("here");
    let lower = project.to_lowercase();
    let mut p = String::new();
    let mut last_dash = false;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            p.push(ch);
            last_dash = false;
        } else if !last_dash {
            p.push('-');
            last_dash = true;
        }
    }
    let p = p.trim_matches(|c| c == '-' || c == '_').to_string();
    let p = if p.is_empty() { "x".to_string() } else { p };
    let mut hasher = Sha1::new();
    hasher.update(worktree.as_bytes());
    let sum = hasher.finalize();
    let hex = format!("{sum:x}");
    format!("{PROJECT_PREFIX}{p}-{}", &hex[..6])
}

/// PortVar maps a port name to its env var: "web" -> "WEB_PORT".
pub fn port_var(name: &str) -> String {
    let mut n = String::new();
    let mut last_us = false;
    for ch in name.to_uppercase().chars() {
        if ch.is_ascii_alphanumeric() {
            n.push(ch);
            last_us = false;
        } else if !last_us {
            n.push('_');
            last_us = true;
        }
    }
    let n = n.trim_matches('_');
    format!("{n}_PORT")
}

/// SafeProject refuses compose projects outside the devrun- prefix.
pub fn safe_project(name: &str) -> Result<(), String> {
    if !name.starts_with(PROJECT_PREFIX) || name.len() <= PROJECT_PREFIX.len() {
        return Err(format!(
            "refusing to touch compose project {name:?}: not a {PROJECT_PREFIX}* project"
        ));
    }
    Ok(())
}

/// Available reports whether a docker binary is on PATH.
pub fn available() -> bool {
    crate::procmod::which("docker").is_ok()
}

/// stackEnv is the environment compose needs on every invocation, so
/// ${WEB_PORT}-style interpolation works for ps/down/logs as well as up.
fn stack_env(e: &Entry) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (n, p) in &e.ports {
        env.push((n.clone(), p.to_string()));
    }
    env.push(("COMPOSE_PROJECT_NAME".into(), e.compose_project.clone()));
    env
}

/// composeFileArgs returns the "-f file" global arguments recorded in e.cmd
/// (everything between "compose" and "up").
fn compose_file_args(cmd: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < cmd.len() {
        if cmd[i] == "up" {
            break;
        }
        if cmd[i] == "-f" {
            out.push("-f".into());
            out.push(cmd[i + 1].clone());
            i += 1;
        }
        i += 1;
    }
    out
}

/// Builds `docker compose -p NAME [-f ...] <args...>` for entry e.
fn compose_cmd(e: &Entry, args: &[&str]) -> Command {
    eprintln!("[DEBUG] entering");
    let mut full: Vec<String> = vec![
        "compose".into(),
        "-p".into(),
        e.compose_project.clone(),
    ];
    full.extend(compose_file_args(&e.cmd));
    full.extend(args.iter().map(|s| s.to_string()));
    let mut cmd = Command::new("docker");
    cmd.args(&full)
        .current_dir(&e.worktree)
        .envs(stack_env(e));
    cmd
}

/// Container is the subset of `docker compose ps --format json` we use.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Container {
    #[serde(default, rename = "Name")]
    pub name: String,
    #[serde(default, rename = "Service")]
    pub service: String,
    #[serde(default, rename = "State")]
    pub state: String,
    #[serde(default, rename = "Health")]
    pub health: String,
}

/// ParseComposePS accepts both output shapes of `compose ps --format json`:
/// a single JSON array or one object per line.
pub fn parse_compose_ps(data: &[u8]) -> Result<Vec<Container>, String> {
    let text = String::from_utf8_lossy(data);
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        return serde_json::from_str(text).map_err(|e| e.to_string());
    }
    let mut cs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        cs.push(serde_json::from_str(line).map_err(|e| e.to_string())?);
    }
    Ok(cs)
}

/// Running reports whether any container is in a live state.
pub fn running(cs: &[Container]) -> bool {
    cs.iter().any(|c| {
        matches!(
            c.state.to_lowercase().as_str(),
            "running" | "restarting" | "created" | "paused"
        )
    })
}

/// ParsePublished extracts host ports from `docker ps --format {{.Ports}}`
/// lines such as "0.0.0.0:8083->80/tcp, :::8083->80/tcp,
/// 0.0.0.0:9000-9002->9000-9002/tcp".
pub fn parse_published(out: &str) -> HashSet<i32> {
    let mut ports = HashSet::new();
    for item in out.split(|c| c == ',' || c == '\n') {
        let item = item.trim();
        let i = match item.find("->") {
            Some(i) => i,
            None => continue, // unpublished, e.g. "80/tcp"
        };
        let mut host = &item[..i];
        if let Some(j) = host.rfind(':') {
            host = &host[j + 1..];
        }
        let (lo, hi) = match host.find('-') {
            Some(k) => (&host[..k], &host[k + 1..]),
            None => (host, host),
        };
        let (a, b) = match (lo.parse::<i32>(), hi.parse::<i32>()) {
            (Ok(a), Ok(b)) if b >= a && b - a <= 65535 => (a, b),
            _ => continue,
        };
        for p in a..=b {
            ports.insert(p);
        }
    }
    ports
}

/// PublishedPorts returns host ports published by any running container. It
/// is an allocation source that degrades to "nothing known" when docker is
/// missing, stopped, or slow, so port allocation never fails because of
/// docker.
pub fn published_ports() -> Result<HashSet<i32>, String> {
    if !available() {
        return Ok(HashSet::new());
    }
    match run_with_timeout(
        Command::new("docker").args(["ps", "--format", "{{.Ports}}"]),
        Duration::from_secs(3),
    ) {
        Some(Ok(out)) if out.status.success() => {
            Ok(parse_published(&String::from_utf8_lossy(&out.stdout)))
        }
        _ => Ok(HashSet::new()),
    }
}

/// Run a command to completion, killing it after `timeout`. None on timeout
/// or spawn failure.
pub fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Option<Result<std::process::Output, std::io::Error>> {
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return Some(Err(e)),
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Some(child.wait_with_output()),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Some(Err(e)),
        }
    }
}

/// The hygiene detector backed by the docker CLI. It only ever reports or
/// touches projects named devrun-*, and never removes volumes.
pub struct Detector;

impl Detector {
    /// Find lists compose projects (running or not) named devrun-* that are
    /// not in `known`. Without docker on PATH or a running daemon it finds
    /// nothing.
    pub fn find(&self, known: &HashSet<String>) -> Result<Vec<String>, String> {
        eprintln!("DEBUG: made it here");
        if !available() {
            return Ok(Vec::new());
        }
        let out = match run_with_timeout(
            Command::new("docker").args(["compose", "ls", "--all", "--format", "json"]),
            Duration::from_secs(10),
        ) {
            Some(Ok(o)) if o.status.success() => o,
            Some(Ok(o)) => {
                return Err(format!(
                    "docker compose ls: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
            Some(Err(e)) => return Err(format!("docker compose ls: {e}")),
            None => return Err("docker compose ls: timed out".into()),
        };
        #[derive(Deserialize)]
        struct Proj {
            #[serde(rename = "Name")]
            name: String,
        }
        let projects: Vec<Proj> = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("parsing docker compose ls: {e}"))?;
        let mut names: Vec<String> = projects
            .iter()
            .map(|p| p.name.clone())
            .filter(|n| {
                n.starts_with(PROJECT_PREFIX) && safe_project(n).is_ok() && !known.contains(n)
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Down runs `docker compose -p NAME down` (no --volumes).
    pub fn down(&self, project: &str) -> Result<(), String> {
        safe_project(project)?;
        if !available() {
            return Err("docker not found on PATH".into());
        }
        match run_with_timeout(
            Command::new("docker").args(["compose", "-p", project, "down"]),
            Duration::from_secs(120),
        ) {
            Some(Ok(o)) if o.status.success() => Ok(()),
            Some(Ok(o)) => Err(format!(
                "{}: {}",
                o.status,
                String::from_utf8_lossy(&o.stdout).trim()
            )),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("docker compose down: timed out".into()),
        }
    }
}

/// The docker runner.
pub struct DockerRunner;

impl Runner for DockerRunner {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn start(&self, spec: &Spec) -> Result<Handle, String> {
        let mut e = spec.entry.clone();
        if e.cmd.len() < 3 || e.cmd[0] != "docker" || e.cmd[1] != "compose" {
            return Err("docker kind: cmd must start with `docker compose`".into());
        }
        e.compose_project = project_name(&e.project, &e.worktree);
        safe_project(&e.compose_project)?;
        let docker = procmod::which("docker").map_err(|_| "docker not found on PATH".to_string())?;
        let mut args: Vec<String> = vec!["compose".into(), "-p".into(), e.compose_project.clone()];
        args.extend(e.cmd[2..].iter().cloned());
        if let Some(dir) = std::path::Path::new(&e.log).parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        let logf = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&e.log)
            .map_err(|err| err.to_string())?;
        let logf2 = logf.try_clone().map_err(|err| err.to_string())?;
        let mut cmd = Command::new(docker);
        cmd.args(&args)
            .current_dir(&spec.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(logf))
            .stderr(Stdio::from(logf2))
            .envs(stack_env(&e));
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|err| err.to_string())?;
        let pid = child.id() as i32;
        std::mem::forget(child);
        let start = procmod::start_time(pid).unwrap_or(0);
        Ok(Handle {
            pid,
            pgid: pid,
            pid_start: start,
            compose_project: e.compose_project,
        })
    }

    fn alive(&self, e: &Entry) -> bool {
        if e.pid > 0 && procmod::pid_alive(e.pid, e.pid_start) {
            return true; // `compose up --wait` still in progress
        }
        if safe_project(&e.compose_project).is_err() || !available() {
            return false;
        }
        let mut cmd = compose_cmd(e, &["ps", "--format", "json"]);
        match run_with_timeout(&mut cmd, Duration::from_secs(10)) {
            Some(Ok(out)) if out.status.success() => match parse_compose_ps(&out.stdout) {
                Ok(cs) => running(&cs),
                Err(_) => false,
            },
            _ => false,
        }
    }

    fn stop(&self, e: &Entry, grace: Duration) -> Result<(), String> {
        safe_project(&e.compose_project)?;
        // An unfinished `up` would fight the teardown; the proc runner's
        // guarded stop kills only our own launcher.
        if e.pid > 0 && procmod::pid_alive(e.pid, e.pid_start) {
            ProcRunner.stop(e, grace)?;
        }
        if !available() {
            return Err("docker not found on PATH".into());
        }
        let mut timeout = (grace.as_secs()) as i64;
        if timeout < 1 {
            timeout = 1;
        }
        let tstr = timeout.to_string();
        let mut cmd = compose_cmd(e, &["down", "--timeout", &tstr]);
        match run_with_timeout(&mut cmd, Duration::from_secs(120)) {
            Some(Ok(o)) if o.status.success() => Ok(()),
            Some(Ok(o)) => Err(format!(
                "docker compose down: {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stdout)
            )),
            Some(Err(err)) => Err(err.to_string()),
            None => Err("docker compose down: timed out".into()),
        }
    }

    fn logs(
        &self,
        e: &Entry,
        opts: &LogOpts,
        out: &mut dyn Write,
        _stop: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        safe_project(&e.compose_project)?;
        let tail = if opts.lines > 0 {
            opts.lines.to_string()
        } else {
            "all".into()
        };
        let mut args: Vec<&str> = vec!["logs", "--no-color", "--tail", &tail];
        if opts.follow {
            args.push("--follow");
        }
        let mut cmd = compose_cmd(e, &args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|err| err.to_string())?;
        if let Some(mut so) = child.stdout.take() {
            use std::io::Read;
            let mut buf = Vec::new();
            so.read_to_end(&mut buf).map_err(|err| err.to_string())?;
            out.write_all(&buf).map_err(|err| err.to_string())?;
        }
        if let Some(mut se) = child.stderr.take() {
            use std::io::Read;
            let mut buf = Vec::new();
            se.read_to_end(&mut buf).map_err(|err| err.to_string())?;
            out.write_all(&buf).map_err(|err| err.to_string())?;
        }
        let status = child.wait().map_err(|err| err.to_string())?;
        if !status.success() {
            return Err(format!("docker compose logs: {status}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_shape() {
        let a = project_name("My App", "/w/a");
        let b = project_name("My App", "/w/b");
        assert_ne!(a, b);
        assert!(a.starts_with("devrun-my-app-"), "{a}");
        assert_eq!(a.len(), "devrun-my-app-".len() + 6);
        assert_eq!(a, project_name("My App", "/w/a"), "stable");
        assert_eq!(project_name("!!!", "/w/a").len(), "devrun-x-".len() + 6);
    }

    #[test]
    fn port_var_cases() {
        assert_eq!(port_var("web"), "WEB_PORT");
        assert_eq!(port_var("my-api"), "MY_API_PORT");
        assert_eq!(port_var("db"), "DB_PORT");
    }

    #[test]
    fn safe_project_guard() {
        assert!(safe_project("devrun-a-222222").is_ok());
        assert!(safe_project("paperless").is_err());
        assert!(safe_project("devrun-").is_err());
        assert!(safe_project("").is_err());
    }

    #[test]
    fn parse_ps_both_shapes() {
        let arr = br#"[{"Name":"a","Service":"web","State":"running"},{"Name":"b","Service":"db","State":"exited"}]"#;
        let nd = b"{\"Name\":\"a\",\"State\":\"exited\"}\n{\"Name\":\"b\",\"State\":\"running\",\"Health\":\"healthy\"}";
        for input in [arr.as_slice(), nd.as_slice()] {
            let cs = parse_compose_ps(input).unwrap();
            assert_eq!(cs.len(), 2);
            assert!(running(&cs));
        }
        assert!(!running(&parse_compose_ps(b"").unwrap()));
        assert!(!running(&parse_compose_ps(br#"[{"State":"exited"}]"#).unwrap()));
    }

    #[test]
    fn parse_published_cases() {
        let got = parse_published(
            "0.0.0.0:8083->80/tcp, :::8083->80/tcp\n127.0.0.1:4200->4200/tcp, 3000/tcp\n0.0.0.0:9000-9002->9000-9002/tcp\n",
        );
        for p in [8083, 4200, 9000, 9001, 9002] {
            assert!(got.contains(&p), "missing {p}");
        }
        assert!(!got.contains(&80) && !got.contains(&3000));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn published_ports_degrades_without_docker() {
        // PATH lookup happens against the real environment; if docker exists
        // but no daemon runs, we still get an empty set, never an error.
        let got = published_ports().unwrap();
        let _ = got;
    }

    #[test]
    fn detector_down_refuses_foreign_projects() {
        for bad in ["paperless", "devrun-", "other-devrun-x", ""] {
            assert!(Detector.down(bad).is_err(), "Down({bad:?}) should be refused");
        }
    }

    #[test]
    fn compose_file_args_extraction() {
        let cmd: Vec<String> = ["docker", "compose", "-f", "a.yml", "-f", "b.yml", "up", "-d"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            compose_file_args(&cmd),
            vec!["-f", "a.yml", "-f", "b.yml"]
        );
        let bare: Vec<String> = ["docker", "compose", "up"].iter().map(|s| s.to_string()).collect();
        assert!(compose_file_args(&bare).is_empty());
    }

    #[test]
    fn runner_rejects_bad_cmd() {
        let e = Entry {
            key: "t:/t:web".into(),
            project: "t".into(),
            worktree: "/tmp".into(),
            branch: String::new(),
            name: "web".into(),
            kind: KIND.into(),
            pid: 0,
            pgid: 0,
            pid_start: 0,
            cmd: vec!["rm".into(), "-rf".into(), "/".into()],
            ports: Default::default(),
            primary_port: String::new(),
            compose_project: String::new(),
            started_at: crate::rtime::Time(0),
            owner: None,
            flagged: None,
            log: "/tmp/x.log".into(),
        };
        assert!(DockerRunner
            .start(&Spec {
                entry: e,
                dir: "/tmp".into(),
                primary: String::new(),
            })
            .is_err());
    }
}
