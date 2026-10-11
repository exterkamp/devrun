//! Plain processes as the "proc" kind: each in its own session/process group
//! via setsid, with output going to a log file. Also the platform process
//! inspection the other modules rely on (stat parsing, start times, groups).

use crate::registry::Entry;
use crate::runner::{Handle, LogOpts, Runner, Spec};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;

/// The registry kind string for plain processes.
pub const KIND: &str = "proc";

/// Parse a /proc/<pid>/stat body: returns state, pgrp and start time.
/// The comm field may contain spaces and parens, so split after the last ')'.
pub fn parse_stat(s: &str) -> Option<(u8, i32, u64)> {
    let i = s.rfind(')')?;
    let f: Vec<&str> = s[i + 1..].split_whitespace().collect();
    if f.len() < 20 {
        return None;
    }
    let pgrp: i32 = f[2].parse().ok()?;
    let start: u64 = f[19].parse().ok()?;
    Some((f[0].as_bytes()[0], pgrp, start))
}

/// procStat reads /proc/<pid>/stat under `root`.
pub fn proc_stat_in(root: &str, pid: i32) -> Option<(u8, i32, u64)> {
    let data = std::fs::read_to_string(format!("{root}/{pid}/stat")).ok()?;
    parse_stat(&data)
}

#[cfg(target_os = "linux")]
pub fn proc_stat(pid: i32) -> Option<(u8, i32, u64)> {
    proc_stat_in("/proc", pid)
}

#[cfg(not(target_os = "linux"))]
pub fn proc_stat(pid: i32) -> Option<(u8, i32, u64)> {
    // No /proc elsewhere: use `ps` for the process group, with start time 0
    // (identity then degrades to "process exists").
    let out = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let pgid: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some((b'S', pgid, 0))
}

/// StartTime returns the start time of pid, or None if it is not running.
pub fn start_time(pid: i32) -> Option<u64> {
    proc_stat(pid).map(|(_, _, s)| s)
}

/// pidAlive: the process exists, is not a zombie, and is the one we started.
pub fn pid_alive(pid: i32, start: u64) -> bool {
    if pid <= 0 {
        return false;
    }
    match proc_stat(pid) {
        Some((state, _, s)) => state != b'Z' && state != b'X' && (start == 0 || s == start || s == 0),
        None => false,
    }
}

/// PIDAlive reports whether pid is running (not a zombie) and still the
/// process that started at `start`, so a recycled PID never counts as alive.
pub fn pid_alive_exported(pid: i32, start: u64) -> bool {
    pid_alive(pid, start)
}

/// Group membership from /proc on Linux: any non-zombie process in pgid.
#[cfg(target_os = "linux")]
pub fn group_alive(pgid: i32) -> bool {
    if let Ok(ents) = std::fs::read_dir("/proc") {
        for de in ents.flatten() {
            let name = de.file_name();
            let pid: i32 = match name.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if let Some((state, g, _)) = proc_stat(pid) {
                if state != b'Z' && state != b'X' && g == pgid {
                    return true;
                }
            }
        }
        return false;
    }
    unsafe { libc::kill(-pgid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) }
}

#[cfg(not(target_os = "linux"))]
pub fn group_alive(pgid: i32) -> bool {
    unsafe { libc::kill(-pgid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) }
}

pub fn group_gone(pgid: i32) -> bool {
    !group_alive(pgid)
}

pub fn wait_group_gone(pgid: i32, d: Duration) -> bool {
    let deadline = std::time::Instant::now() + d;
    loop {
        if group_gone(pgid) {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Substitute replaces {port} in args with port.
pub fn substitute(args: &[String], port: i32) -> Vec<String> {
    args.iter()
        .map(|a| a.replace("{port}", &port.to_string()))
        .collect()
}

/// The proc runner.
pub struct ProcRunner;

impl Runner for ProcRunner {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn start(&self, spec: &Spec) -> Result<Handle, String> {
        let e = &spec.entry;
        if e.cmd.is_empty() {
            return Err("empty command".into());
        }
        let args = substitute(&e.cmd, *e.ports.get(&spec.primary).unwrap_or(&0));
        let path = which(&args[0])?;
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

        let mut cmd = std::process::Command::new(path);
        cmd.args(&args[1..])
            .current_dir(&spec.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(logf))
            .stderr(std::process::Stdio::from(logf2));
        let mut names: Vec<&String> = e.ports.keys().collect();
        names.sort();
        for n in names {
            cmd.env(n, e.ports[n].to_string());
        }
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
        // We never wait: the server outlives devrun. Dropping the Child handle
        // leaves the process running, reparented away from us.
        std::mem::forget(child);
        let start = start_time(pid).unwrap_or(0);
        Ok(Handle {
            pid,
            pgid: pid,
            pid_start: start,
            compose_project: String::new(),
        })
    }

    fn alive(&self, e: &Entry) -> bool {
        e.pid > 0 && pid_alive(e.pid, e.pid_start)
    }

    fn stop(&self, e: &Entry, grace: Duration) -> Result<(), String> {
        if e.pid <= 0 {
            return Ok(());
        }
        let pgid = if e.pgid > 0 { e.pgid } else { e.pid };
        match proc_stat(e.pid) {
            Some((_, _, start)) => {
                if e.pid_start != 0 && start != 0 && start != e.pid_start {
                    return Ok(()); // PID reused by someone else; ours is gone
                }
            }
            None => {
                // The leader is possibly gone; if the pid truly does not exist
                // the group may still hold stragglers, which is fine to signal.
                let rc = unsafe { libc::kill(e.pid, 0) };
                if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                    return Err(format!("cannot verify identity of process {}", e.pid));
                }
            }
        }
        let rc = unsafe { libc::kill(-pgid, libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(err.to_string());
        }
        if group_gone(pgid) || wait_group_gone(pgid, grace) {
            return Ok(());
        }
        let rc = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err.to_string());
            }
        }
        if !wait_group_gone(pgid, Duration::from_secs(2)) {
            return Err(format!("process group {pgid} survived SIGKILL"));
        }
        Ok(())
    }

    fn logs(
        &self,
        e: &Entry,
        opts: &LogOpts,
        out: &mut dyn Write,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        tail_file(&e.log, opts.lines, opts.follow, out, stop)
    }
}

/// Resolve a program name against PATH (exec.LookPath, roughly).
pub fn which(name: &str) -> Result<std::path::PathBuf, String> {
    if name.contains('/') {
        let p = std::path::PathBuf::from(name);
        if is_executable(&p) {
            return Ok(p);
        }
        return Err(format!("exec: {name:?}: no such file or directory"));
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let p = std::path::PathBuf::from(dir).join(name);
        if is_executable(&p) {
            return Ok(p);
        }
    }
    Err(format!("exec: {name:?}: executable file not found in $PATH"))
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// TailFile writes the last `lines` lines of path (all if lines<=0) to out
/// and, if follow, keeps streaming appended data until `stop` reports true.
pub fn tail_file(
    path: &str,
    lines: i64,
    follow: bool,
    out: &mut dyn Write,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).map_err(|e| e.to_string())?;
    out.write_all(last_lines(&data, lines)).map_err(|e| e.to_string())?;
    out.flush().ok();
    if !follow {
        return Ok(());
    }
    let mut pos = data.len() as u64;
    let mut buf = [0u8; 32 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n > 0 {
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            out.flush().ok();
            pos += n as u64;
            continue;
        }
        // File may have been truncated and rewritten (a fresh `up`); restart.
        if let Ok(meta) = f.metadata() {
            if meta.len() < pos {
                f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                pos = 0;
            }
        }
        if stop() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn last_lines(data: &[u8], n: i64) -> &[u8] {
    if n <= 0 {
        return data;
    }
    let trimmed_len = {
        let mut l = data.len();
        while l > 0 && data[l - 1] == b'\n' {
            l -= 1;
        }
        l
    };
    let mut idx = trimmed_len;
    for _ in 0..n {
        match data[..idx].iter().rposition(|b| *b == b'\n') {
            Some(j) => idx = j,
            None => return data,
        }
    }
    &data[idx + 1..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stat_with_parens_in_name() {
        // comm may contain spaces and parens: fields follow the last ')'.
        let s = "1234 (my (weird) proc) S 100 1234 1234 0 -1 4194304 100 0 0 0 0 0 0 0 20 0 1 0 98765 0 0";
        let (state, pgrp, start) = parse_stat(s).unwrap();
        assert_eq!(state, b'S');
        assert_eq!(pgrp, 1234);
        assert_eq!(start, 98765);
        assert!(parse_stat("garbage").is_none());
        assert!(parse_stat("1 (x) S 2").is_none());
    }

    #[test]
    fn substitute_ports() {
        let args: Vec<String> = vec!["serve".into(), "{port}".into(), "--p={port}x".into()];
        assert_eq!(
            substitute(&args, 41237),
            vec!["serve", "41237", "--p=41237x"]
        );
        assert_eq!(substitute(&[], 1).len(), 0);
    }

    #[test]
    fn last_lines_cases() {
        let data = b"one\ntwo\nthree\nfour\n";
        assert_eq!(last_lines(data, 0), data.as_slice());
        assert_eq!(last_lines(data, -1), data.as_slice());
        assert_eq!(last_lines(data, 2), b"three\nfour\n".as_slice());
        assert_eq!(last_lines(data, 99), data.as_slice());
        assert_eq!(last_lines(b"", 3), b"".as_slice());
        assert_eq!(last_lines(b"no newline", 1), b"no newline".as_slice());
    }

    #[test]
    fn tail_file_reads_tail() {
        let dir = std::env::temp_dir().join(format!("devrun-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.log");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        let mut out: Vec<u8> = Vec::new();
        tail_file(path.to_str().unwrap(), 2, false, &mut out, &|| false).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "b\nc\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_pid_is_alive() {
        let pid = std::process::id() as i32;
        let start = start_time(pid).expect("own stat");
        assert!(pid_alive(pid, start));
        assert!(!pid_alive(pid, start + 1), "wrong start must not match");
        assert!(!pid_alive(0, 0));
        assert!(!pid_alive(-5, 0));
        // The init process group is alive.
        assert!(group_alive(1) || !std::path::Path::new("/proc/1/stat").exists());
    }

    #[test]
    fn start_and_stop_a_process() {
        let dir = std::env::temp_dir().join(format!("devrun-proc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.log");
        let e = Entry {
            key: "t:/t:web".into(),
            project: "t".into(),
            worktree: dir.to_string_lossy().into_owned(),
            branch: String::new(),
            name: "web".into(),
            kind: KIND.into(),
            pid: 0,
            pgid: 0,
            pid_start: 0,
            cmd: vec!["python3".into(), "-c".into(), "import time; time.sleep(30)".into()],
            ports: [("PORT".to_string(), 41001)].into_iter().collect(),
            primary_port: "PORT".into(),
            compose_project: String::new(),
            started_at: crate::rtime::Time(0),
            owner: None,
            flagged: None,
            log: log.to_string_lossy().into_owned(),
        };
        let h = ProcRunner.start(&Spec {
            entry: e.clone(),
            dir: e.worktree.clone(),
            primary: "PORT".into(),
        })
        .unwrap();
        assert!(h.pid > 0 && h.pid_start > 0);
        let mut live = e.clone();
        live.pid = h.pid;
        live.pgid = h.pgid;
        live.pid_start = h.pid_start;
        assert!(ProcRunner.alive(&live));
        ProcRunner.stop(&live, Duration::from_secs(2)).unwrap();
        assert!(!ProcRunner.alive(&live));
        // Stopping a recycled pid must be a no-op, not an error.
        let mut fake = live.clone();
        fake.pid_start = h.pid_start + 999;
        assert!(ProcRunner.stop(&fake, Duration::from_millis(10)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
