//! Finds things devrun should clean up: listeners nobody registered
//! (orphans), ports a server bound instead of the one it was given (drift),
//! and compose projects with no registry entry.

use serde::Serialize;
use std::collections::HashSet;
use std::time::Duration;

/// One process holding a LISTEN socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Listener {
    pub port: i32,
    pub pid: i32,
    #[serde(skip)]
    pub pgid: i32,
    /// Platform process start time; guards against PID reuse.
    #[serde(skip)]
    pub start: u64,
}

/// Scanner reads native listener ownership. Tests can point `root` at a fake
/// /proc tree on Linux.
#[derive(Debug, Clone)]
pub struct Scanner {
    pub root: String,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Scanner {
        Scanner {
            root: "/proc".to_string(),
        }
    }
    pub fn with_root(root: &str) -> Scanner {
        Scanner {
            root: root.to_string(),
        }
    }

    fn stat(&self, pid: i32) -> Option<(u8, i32, u64)> {
        #[cfg(target_os = "linux")]
        {
            return crate::procmod::proc_stat_in(&self.root, pid);
        }
        #[cfg(not(target_os = "linux"))]
        {
            return crate::procmod::proc_stat(pid);
        }
    }

    /// listen_inodes maps socket inode to port for every LISTEN socket.
    #[cfg(target_os = "linux")]
    fn listen_inodes(&self) -> Result<std::collections::HashMap<u64, i32>, String> {
        let mut out = std::collections::HashMap::new();
        let mut found = false;
        for n in ["net/tcp", "net/tcp6"] {
            let path = format!("{}/{n}", self.root);
            let data = match std::fs::read_to_string(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            found = true;
            for line in data.lines().skip(1) {
                let fl: Vec<&str> = line.split_whitespace().collect();
                if fl.len() < 10 || fl[3] != "0A" {
                    continue;
                }
                let local = fl[1];
                let port = match local.rfind(':') {
                    Some(i) => u32::from_str_radix(&local[i + 1..], 16).ok(),
                    None => None,
                };
                let ino: Option<u64> = fl[9].parse().ok();
                if let (Some(p), Some(i)) = (port, ino) {
                    if i != 0 {
                        out.insert(i, p as i32);
                    }
                }
            }
        }
        if !found {
            return Err(format!("cannot read {}/net/tcp", self.root));
        }
        Ok(out)
    }

    /// Listeners returns every (port, process) pair with a LISTEN socket that
    /// this user can see. Zombies are skipped. Sorted by port, then PID.
    pub fn listeners(&self) -> Result<Vec<Listener>, String> {
        #[cfg(target_os = "linux")]
        {
            if self.root == "/proc" || std::path::Path::new(&self.root).join("net/tcp").exists() {
                return self.proc_listeners();
            }
            // A fake root without net files still serves stat/fd lookups.
            self.proc_listeners()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.lsof_listeners()
        }
    }

    #[cfg(target_os = "linux")]
    fn proc_listeners(&self) -> Result<Vec<Listener>, String> {
        let inodes = self.listen_inodes()?;
        let ents = std::fs::read_dir(&self.root).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for de in ents.flatten() {
            let name = de.file_name();
            let pid: i32 = match name.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let (state, pgrp, start) = match self.stat(pid) {
                Some(s) => s,
                None => continue,
            };
            if state == b'Z' || state == b'X' {
                continue;
            }
            let fd_dir = format!("{}/{pid}/fd", self.root);
            let fds = match std::fs::read_dir(&fd_dir) {
                Ok(f) => f,
                Err(_) => continue, // not ours to see, or exited
            };
            let mut seen: HashSet<i32> = HashSet::new();
            for fd in fds.flatten() {
                let link = match std::fs::read_link(fd.path()) {
                    Ok(l) => l.to_string_lossy().into_owned(),
                    Err(_) => continue,
                };
                if let Some(rest) = link.strip_prefix("socket:[") {
                    if let Some(ino_s) = rest.strip_suffix(']') {
                        if let Ok(ino) = ino_s.parse::<u64>() {
                            if let Some(port) = inodes.get(&ino) {
                                if seen.insert(*port) {
                                    out.push(Listener {
                                        port: *port,
                                        pid,
                                        pgid: pgrp,
                                        start,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.port.cmp(&b.port).then(a.pid.cmp(&b.pid)));
        Ok(out)
    }

    #[cfg(not(target_os = "linux"))]
    fn lsof_listeners(&self) -> Result<Vec<Listener>, String> {
        let out = std::process::Command::new("/usr/sbin/lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"])
            .output()
            .map_err(|e| format!("cannot read listener ownership (lsof failed): {e}"))?;
        let mut listeners = Vec::new();
        let mut pid = 0;
        let mut seen: HashSet<(i32, i32)> = HashSet::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.len() < 2 {
                continue;
            }
            match line.as_bytes()[0] {
                b'p' => pid = line[1..].parse().unwrap_or(0),
                b'n' => {
                    if pid <= 0 {
                        continue;
                    }
                    if let Some(i) = line.rfind(':') {
                        if let Ok(port) = line[i + 1..].parse::<i32>() {
                            if port > 0 && port <= 65535 && seen.insert((pid, port)) {
                                let (_, pgrp, start) = self.stat(pid).unwrap_or((b'S', pid, 0));
                                listeners.push(Listener {
                                    port,
                                    pid,
                                    pgid: pgrp,
                                    start,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(listeners)
    }

    /// GroupPorts returns the sorted ports listened on by any process in pgid.
    pub fn group_ports(&self, pgid: i32) -> Result<Vec<i32>, String> {
        let ls = self.listeners()?;
        let mut set: HashSet<i32> = HashSet::new();
        for l in &ls {
            if l.pgid == pgid {
                set.insert(l.port);
            }
        }
        let mut ports: Vec<i32> = set.into_iter().collect();
        ports.sort();
        Ok(ports)
    }

    /// Kill terminates the process behind l with SIGTERM, then SIGKILL after
    /// grace. It re-reads the process's start time first and refuses
    /// (returning false) if the PID now belongs to a different process, and
    /// it never signals devrun itself. Only the one PID is signalled, not
    /// its group.
    pub fn kill(&self, l: &Listener, grace: Duration) -> Result<bool, String> {
        if l.pid <= 1 || l.pid == std::process::id() as i32 {
            return Ok(false);
        }
        let same = |s: &Scanner| -> bool {
            match s.stat(l.pid) {
                Some((state, _, start)) => state != b'Z' && state != b'X' && start == l.start,
                None => false,
            }
        };
        if !same(self) {
            return Ok(false);
        }
        let rc = unsafe { libc::kill(l.pid, libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(true);
            }
            return Err(err.to_string());
        }
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if !same(self) {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if same(self) {
            let rc = unsafe { libc::kill(l.pid, libc::SIGKILL) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(err.to_string());
                }
            }
        }
        Ok(true)
    }
}

/// Orphan is something devrun did not register but that sits in devrun's
/// territory: a listener in the port range, or a devrun-* compose project.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Orphan {
    pub kind: String, // "port" or "compose"
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pid: i32,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "compose_project")]
    pub project: String,
    #[serde(skip)]
    pub listen: Option<Listener>,
}

fn is_zero(n: &i32) -> bool {
    *n == 0
}

/// PortOrphans returns listeners with a port in [min,max] that is not in
/// `registered`.
pub fn port_orphans(ls: &[Listener], registered: &HashSet<i32>, min: i32, max: i32) -> Vec<Orphan> {
    let mut out = Vec::new();
    for l in ls {
        if l.port < min || l.port > max || registered.contains(&l.port) {
            continue;
        }
        out.push(Orphan {
            kind: "port".into(),
            port: l.port,
            pid: l.pid,
            project: String::new(),
            listen: Some(*l),
        });
    }
    out
}

/// The compose orphan detector: backed by the docker CLI (see docker.rs).
/// Only projects named devrun-* are ever reported or touched.
pub struct ComposeAccess;

impl ComposeAccess {
    pub fn find(&self, known: &HashSet<String>) -> Result<Vec<String>, String> {
        crate::docker::Detector.find(known)
    }
    pub fn down(&self, project: &str) -> Result<(), String> {
        crate::docker::Detector.down(project)
    }
}

pub fn compose() -> ComposeAccess {
    ComposeAccess
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(port: i32, pid: i32, pgid: i32) -> Listener {
        Listener {
            port,
            pid,
            pgid,
            start: 42,
        }
    }

    #[test]
    fn port_orphans_filters() {
        let ls = vec![
            listener(41001, 10, 10),
            listener(41002, 11, 11), // registered
            listener(3000, 12, 12),  // outside range
            listener(41999, 13, 13),  // edge of range
        ];
        let registered: HashSet<i32> = [41002].into_iter().collect();
        let got = port_orphans(&ls, &registered, 41000, 41999);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].port, 41001);
        assert_eq!(got[0].kind, "port");
        assert_eq!(got[1].port, 41999);
        // JSON shape: compose_project omitted, port/pid present.
        let s = serde_json::to_string(&got[0]).unwrap();
        assert!(s.contains("\"kind\":\"port\""), "{s}");
        assert!(!s.contains("compose_project"), "{s}");
    }

    #[test]
    fn orphan_compose_json_shape() {
        let o = Orphan {
            kind: "compose".into(),
            port: 0,
            pid: 0,
            project: "devrun-x-abc123".into(),
            listen: None,
        };
        let s = serde_json::to_string(&o).unwrap();
        assert!(s.contains("\"compose_project\":\"devrun-x-abc123\""), "{s}");
        assert!(!s.contains("\"port\""), "{s}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scanner_reads_fake_proc() {
        // Build a tiny fake /proc: one pid holding socket inode 4242, which
        // net/tcp says is LISTEN on 41047 (0xA057).
        let root = std::env::temp_dir().join(format!("devrun-fakeproc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pid_dir = root.join("777");
        std::fs::create_dir_all(pid_dir.join("fd")).unwrap();
        std::fs::create_dir_all(root.join("net")).unwrap();
        std::fs::write(
            root.join("net/tcp"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
             0: 0100007F:A057 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 4242 1 0000000000000000 100 0 0 10 0\n",
        )
        .unwrap();
        std::fs::write(root.join("net/tcp6"), "").unwrap();
        // stat: pid (comm) state ppid pgrp ... field 22 (index 19 after ')') = 555
        std::fs::write(
            pid_dir.join("stat"),
            "777 (fake) S 1 777 777 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 555 0 0",
        )
        .unwrap();
        std::os::unix::fs::symlink("socket:[4242]", pid_dir.join("fd/3")).unwrap();
        let sc = Scanner::with_root(root.to_str().unwrap());
        let ls = sc.listeners().unwrap();
        assert_eq!(ls.len(), 1, "{ls:?}");
        assert_eq!(ls[0].port, 41047);
        assert_eq!(ls[0].pid, 777);
        assert_eq!(ls[0].pgid, 777);
        assert_eq!(ls[0].start, 555);
        assert_eq!(sc.group_ports(777).unwrap(), vec![41047]);
        assert!(sc.group_ports(999).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn kill_refuses_recycled_or_self() {
        let sc = Scanner::new();
        // Our own pid: Kill must refuse outright.
        let me = Listener {
            port: 41001,
            pid: std::process::id() as i32,
            pgid: 0,
            start: 0,
        };
        assert_eq!(sc.kill(&me, Duration::from_millis(1)).unwrap(), false);
        // A pid that does not exist: start time will not match.
        let ghost = Listener {
            port: 41001,
            pid: 999_999,
            pgid: 0,
            start: 1,
        };
        assert_eq!(sc.kill(&ghost, Duration::from_millis(1)).unwrap(), false);
    }
}
