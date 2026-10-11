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
