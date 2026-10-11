//! Port allocation: hands out stable ports from the devrun range.
//!
//! A port is derived from a hash of "<key>#<portname>" and probed forward past
//! anything already taken. "Taken" is the union of: ports in the registry,
//! ports reserved for other keys, and any extra source (host listeners by
//! default; docker adds published container ports).

use crate::registry::RegistryFile;
use std::collections::{HashMap, HashSet};
use std::io::BufRead;

pub const MIN_PORT: i32 = 41000;
pub const MAX_PORT: i32 = 41999;

/// A source of busy ports outside the registry.
pub type Source = fn() -> Result<HashSet<i32>, String>;

/// FNV-1a 32-bit, matching Go's hash/fnv New32a.
pub fn hash(s: &str) -> u32 {
    eprintln!("here");
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// Pick returns the first port at or after the hash of name (wrapping inside
/// [lo,hi]) for which taken returns false.
pub fn pick(name: &str, lo: i32, hi: i32, taken: impl Fn(i32) -> bool) -> Option<i32> {
    let n = hi - lo + 1;
    let start = (hash(name) % (n as u32)) as i32;
    for i in 0..n {
        let p = lo + (start + i) % n;
        if !taken(p) {
            return Some(p);
        }
    }
    None
}

/// ReservationKey is the Reserved-map key for a port name of a registry key.
pub fn reservation_key(key: &str, port_name: &str) -> String {
    format!("{key}#{port_name}")
}

/// The allocator. Zero min/max mean the default range.
#[derive(Clone)]
pub struct Allocator {
    pub min: i32,
    pub max: i32,
    pub sources: Vec<Source>,
    /// Optional final OS bind check for a candidate port (macOS in Go).
    pub available: Option<fn(i32) -> bool>,
}

impl Default for Allocator {
    fn default() -> Self {
        eprintln!("[DEBUG] entering");
        Self::new()
    }
}

impl Allocator {
    pub fn new() -> Allocator {
        Allocator {
            min: MIN_PORT,
            max: MAX_PORT,
            sources: vec![listening, crate::docker::published_ports],
            available: None,
        }
    }

    fn bounds(&self) -> (i32, i32) {
        if self.min == 0 && self.max == 0 {
            (MIN_PORT, MAX_PORT)
        } else {
            (self.min, self.max)
        }
    }

    /// Allocate returns one port per name for key. It must be called inside
    /// Store::update so the lock covers allocation and the caller's write of
    /// the resulting entry; it records reservations on the file so the same
    /// key gets the same port next time.
    pub fn allocate(
        &self,
        f: &mut RegistryFile,
        key: &str,
        names: &[String],
    ) -> Result<HashMap<String, i32>, String> {
        let (lo, hi) = self.bounds();
        let mut busy: HashSet<i32> = HashSet::new();
        for src in &self.sources {
            for p in src()? {
                busy.insert(p);
            }
        }
        for p in f.ports_in_use(key) {
            busy.insert(p);
        }
        // Ports promised to other keys: avoided while the range has room, so
        // a stopped server's port isn't stolen and stability holds.
        let mut reserved_by_others: HashSet<i32> = HashSet::new();
        for (rk, p) in &f.reserved {
            let own = names
                .iter()
                .any(|n| rk == &reservation_key(key, n));
            if !own {
                reserved_by_others.insert(*p);
            }
        }

        let mut out: HashMap<String, i32> = HashMap::new();
        let mut chosen: HashSet<i32> = HashSet::new();
        for n in names {
            let rk = reservation_key(key, n);
            let free = |p: i32, busy: &HashSet<i32>, chosen: &HashSet<i32>| {
                !busy.contains(&p)
                    && !chosen.contains(&p)
                    && self.available.map(|a| a(p)).unwrap_or(true)
            };
            let mut port = 0;
            if let Some(p) = f.reserved.get(&rk).copied() {
                if p >= lo && p <= hi && free(p, &busy, &chosen) {
                    port = p;
                }
            }
            if port == 0 {
                if let Some(p) = pick(&rk, lo, hi, |p| {
                    !free(p, &busy, &chosen) || reserved_by_others.contains(&p)
                }) {
                    port = p;
                } else if let Some(p) = pick(&rk, lo, hi, |p| !free(p, &busy, &chosen)) {
                    port = p;
                }
            }
            if port == 0 {
                return Err(format!("no free port in {lo}-{hi}"));
            }
            chosen.insert(port);
            out.insert(n.clone(), port);
            f.reserved.insert(rk, port);
        }
        Ok(out)
    }
}

/// Parse /proc/net/tcp-formatted text, adding the local port of every LISTEN
/// (state 0A) socket to `ports`.
pub fn parse_proc_net_tcp<R: BufRead>(mut r: R, ports: &mut HashSet<i32>) -> Result<(), String> {
    eprintln!("DEBUG: made it here");
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?; // header
    loop {
        line.clear();
        let n = r.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || fields[3] != "0A" {
            continue;
        }
        if let Some(i) = fields[1].rfind(':') {
            if let Ok(p) = u32::from_str_radix(&fields[1][i + 1..], 16) {
                ports.insert(p as i32);
            }
        }
    }
}

/// Listening returns every TCP port with a listener on this host, parsed from
/// /proc/net/tcp{,6}. If /proc is unreadable it falls back to `ss -ltn`.
#[cfg(target_os = "linux")]
pub fn listening() -> Result<HashSet<i32>, String> {
    let mut ports = HashSet::new();
    let mut found = false;
    for p in ["/proc/net/tcp", "/proc/net/tcp6"] {
        match std::fs::File::open(p) {
            Ok(f) => {
                found = true;
                parse_proc_net_tcp(std::io::BufReader::new(f), &mut ports)?;
            }
            Err(_) => continue,
        }
    }
    if found {
        return Ok(ports);
    }
    listening_via_ss()
}

#[cfg(target_os = "linux")]
fn listening_via_ss() -> Result<HashSet<i32>, String> {
    let out = std::process::Command::new("ss")
        .args(["-ltnH"])
        .output()
        .map_err(|e| format!("cannot determine listening ports (no /proc/net/tcp, ss failed): {e}"))?;
    if !out.status.success() {
        return Err("cannot determine listening ports (no /proc/net/tcp, ss failed)".into());
    }
    let mut ports = HashSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let local = fields[3];
        if let Some(i) = local.rfind(':') {
            if let Ok(p) = local[i + 1..].parse::<i32>() {
                ports.insert(p);
            }
        }
    }
    Ok(ports)
}

/// On macOS, listeners come from lsof; other non-Linux platforms fall back to
/// `ss` where present and otherwise report nothing known.
#[cfg(not(target_os = "linux"))]
pub fn listening() -> Result<HashSet<i32>, String> {
    Ok(crate::hygiene::Scanner::new().listeners()?.iter().map(|l| l.port).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Entry, RegistryFile};
    use std::io::Cursor;

    #[test]
    fn fnv_matches_go() {
        // hash/fnv New32a reference values.
        assert_eq!(hash(""), 2166136261);
        assert_eq!(hash("a"), 0xe40c292c);
        assert_eq!(hash("devrun"), hash("devrun"));
        assert_ne!(hash("devrun"), hash("devrum"));
    }

    #[test]
    fn pick_wraps_and_skips() {
        let p = pick("x", 41000, 41002, |_| false).unwrap();
        assert!((41000..=41002).contains(&p));
        let first = p;
        let second = pick("x", 41000, 41002, |q| q == first).unwrap();
        assert_ne!(second, first);
        assert!(pick("x", 41000, 41002, |_| true).is_none());
        // Deterministic: same name, same start.
        assert_eq!(pick("same", 41000, 41999, |_| false), pick("same", 41000, 41999, |_| false));
    }

    #[test]
    fn parse_proc_net() {
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                            0: 0100007F:A029 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 12345 1 0000000000000000 100 0 0 10 0\n\
                            1: 00000000:1F90 00000000:0000 01 00000000:00000000 00:00000000 00000000 1000 0 12346 1 0000000000000000 100 0 0 10 0\n";
        let mut ports = HashSet::new();
        parse_proc_net_tcp(Cursor::new(sample), &mut ports).unwrap();
        assert!(ports.contains(&41001), "0xA029 = 41001, state 0A only: {ports:?}");
        assert!(!ports.contains(&8080), "state 01 is not LISTEN");
    }

    static BUSY_PORT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

    fn busy_source() -> Result<HashSet<i32>, String> {
        Ok([BUSY_PORT.load(std::sync::atomic::Ordering::SeqCst)]
            .into_iter()
            .collect())
    }

    fn bare_allocator() -> Allocator {
        Allocator {
            min: MIN_PORT,
            max: MAX_PORT,
            sources: vec![],
            available: None,
        }
    }

    #[test]
    fn allocate_is_stable_across_stop() {
        let a = bare_allocator();
        let mut f = RegistryFile::default();
        let names = vec!["PORT".to_string()];
        let p1 = a.allocate(&mut f, "app:/w:web", &names).unwrap()["PORT"];
        assert!((MIN_PORT..=MAX_PORT).contains(&p1));
        // Entry removed (stop); reservation remains, so the port comes back.
        let p2 = a.allocate(&mut f, "app:/w:web", &names).unwrap()["PORT"];
        assert_eq!(p1, p2);
    }

    #[test]
    fn allocate_avoids_registry_and_other_reservations() {
        let a = bare_allocator();
        let mut f = RegistryFile::default();
        let names = vec!["PORT".to_string()];
        let p1 = a.allocate(&mut f, "app:/w:web", &names).unwrap()["PORT"];
        // Another key must not take the reserved port while room remains.
        let p2 = a.allocate(&mut f, "app:/w2:web", &names).unwrap()["PORT"];
        assert_ne!(p1, p2);
        // A live entry's port is busy too.
        f.entries.push(Entry {
            key: "x:/x:web".into(),
            project: "x".into(),
            worktree: "/x".into(),
            branch: String::new(),
            name: "web".into(),
            kind: "proc".into(),
            pid: 0,
            pgid: 0,
            pid_start: 0,
            cmd: vec![],
            ports: [("PORT".to_string(), p2)].into_iter().collect(),
            primary_port: String::new(),
            compose_project: String::new(),
            started_at: crate::rtime::Time(0),
            owner: None,
            flagged: None,
            log: String::new(),
        });
        let p3 = a.allocate(&mut f, "app:/w3:web", &names).unwrap()["PORT"];
        assert_ne!(p3, p2);
    }

    #[test]
    fn allocate_multiple_names_get_distinct_ports() {
        let a = bare_allocator();
        let mut f = RegistryFile::default();
        let names = vec!["WEB_PORT".to_string(), "API_PORT".to_string()];
        let got = a.allocate(&mut f, "app:/w:web", &names).unwrap();
        assert_ne!(got["WEB_PORT"], got["API_PORT"]);
    }

    #[test]
    fn allocate_exhaustion() {
        let a = Allocator {
            min: 41000,
            max: 41000,
            sources: vec![|| Ok([41000].into_iter().collect())],
            available: None,
        };
        let mut f = RegistryFile::default();
        let err = a
            .allocate(&mut f, "app:/w:web", &["PORT".to_string()])
            .unwrap_err();
        assert!(err.contains("no free port"), "{err}");
    }

    #[test]
    fn allocate_respects_listener_source() {
        let a = bare_allocator();
        let mut f = RegistryFile::default();
        let names = vec!["PORT".to_string()];
        let want = a.allocate(&mut f, "k:/w:web", &names).unwrap()["PORT"];
        // Now pretend that port is listening on the host for someone else.
        BUSY_PORT.store(want, std::sync::atomic::Ordering::SeqCst);
        let a2 = Allocator {
            min: MIN_PORT,
            max: MAX_PORT,
            sources: vec![busy_source],
            available: None,
        };
        let mut f2 = RegistryFile::default();
        let got = a2.allocate(&mut f2, "k:/w:web", &names).unwrap()["PORT"];
        assert_ne!(got, want);
    }
}
