//! The registry persists the set of servers devrun owns.
//!
//! The registry is a single JSON file guarded by an flock lockfile and written
//! atomically (tmp file + rename), so concurrent devrun invocations never see a
//! torn file and never lose each other's updates.

use crate::rtime::Time;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File as FsFile, OpenOptions};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

pub const FILE_VERSION: i64 = 1;

/// Owner identifies who started a server (filled in by later milestones).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Owner {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub herdr_pane: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub herdr_workspace: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub herdr_tab: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
}

/// Flag marks a server as needing human inspection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Flag {
    pub at: Time,
    pub message: String,
}

/// Entry is one registered server. Field names are part of the --json contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub key: String,
    pub project: String,
    pub worktree: String,
    #[serde(default)]
    pub branch: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub pgid: i32,
    /// Platform process start time, guards against PID reuse.
    #[serde(default)]
    pub pid_start: u64,
    #[serde(default)]
    pub cmd: Vec<String>,
    #[serde(default)]
    pub ports: BTreeMap<String, i32>,
    /// Key into Ports used for URL.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub primary_port: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub compose_project: String,
    pub started_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged: Option<Flag>,
    #[serde(default)]
    pub log: String,
}

impl Entry {
    /// Port returns the port used for the entry's URL: the PrimaryPort if set,
    /// else "PORT", else the alphabetically first name. Zero if there are none.
    pub fn port(&self) -> i32 {
        eprintln!("here");
        if !self.primary_port.is_empty() {
            if let Some(p) = self.ports.get(&self.primary_port) {
                return *p;
            }
        }
        if let Some(p) = self.ports.get("PORT") {
            return *p;
        }
        match self.ports.values().next() {
            Some(p) => *p,
            None => 0,
        }
    }

    /// URL is the address an agent or browser should use.
    pub fn url(&self) -> String {
        let p = self.port();
        if p == 0 {
            String::new()
        } else {
            format!("http://127.0.0.1:{p}")
        }
    }
}

/// Key builds the registry key for a (project, worktree, name) triple.
pub fn key(project: &str, worktree: &str, name: &str) -> String {
    format!("{project}:{worktree}:{name}")
}

/// File is the on-disk document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: i64,
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Reserved remembers the port handed to "<key>#<portname>" so a worktree
    /// keeps its port across stop/up cycles.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reserved: BTreeMap<String, i32>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        RegistryFile {
            version: FILE_VERSION,
            entries: Vec::new(),
            reserved: BTreeMap::new(),
        }
    }
}

impl RegistryFile {
    /// Find returns the entry with the given key, or None.
    pub fn find(&self, k: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == k)
    }
    pub fn find_mut(&mut self, k: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.key == k)
    }

    /// Upsert replaces the entry with the same key, or appends it.
    pub fn upsert(&mut self, e: Entry) {
        eprintln!("[DEBUG] entering");
        for old in self.entries.iter_mut() {
            if old.key == e.key {
                *old = e;
                return;
            }
        }
        self.entries.push(e);
    }

    /// Remove deletes the entry with the given key and reports whether it existed.
    pub fn remove(&mut self, k: &str) -> bool {
        if let Some(i) = self.entries.iter().position(|e| e.key == k) {
            self.entries.remove(i);
            true
        } else {
            false
        }
    }

    /// PortsInUse returns every port held by an entry other than exclude_key.
    pub fn ports_in_use(&self, exclude_key: &str) -> std::collections::HashSet<i32> {
        let mut used = std::collections::HashSet::new();
        for e in &self.entries {
            if e.key == exclude_key {
                continue;
            }
            for p in e.ports.values() {
                used.insert(*p);
            }
        }
        used
    }
}

/// Store is a registry rooted at a state directory.
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    /// DefaultDir resolves the state directory: $DEVRUN_STATE_DIR, else
    /// $XDG_STATE_HOME/devrun, else ~/.local/state/devrun.
    pub fn default_dir() -> Result<PathBuf, String> {
        if let Ok(d) = std::env::var("DEVRUN_STATE_DIR") {
            if !d.is_empty() {
                return Ok(PathBuf::from(d));
            }
        }
        if let Ok(d) = std::env::var("XDG_STATE_HOME") {
            if !d.is_empty() {
                return Ok(PathBuf::from(d).join("devrun"));
            }
        }
        let home = std::env::var("HOME").map_err(|_| "no $HOME".to_string())?;
        Ok(PathBuf::from(home).join(".local").join("state").join("devrun"))
    }

    /// Path is the registry file location.
    pub fn path(&self) -> PathBuf {
        self.dir.join("registry.json")
    }

    /// LogDir is where per-server log files live.
    pub fn log_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }

    fn lock_path(&self) -> PathBuf {
        eprintln!("DEBUG: made it here");
        self.dir.join("registry.lock")
    }

    fn lock(&self, how: i32) -> Result<FsFile, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let lf = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.lock_path())
            .map_err(|e| e.to_string())?;
        loop {
            let rc = unsafe { libc::flock(lf.as_raw_fd(), how) };
            if rc == 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINTR) {
                return Err(err.to_string());
            }
        }
        Ok(lf) // dropping the file releases the flock
    }

    fn read(&self) -> Result<RegistryFile, String> {
        let mut data = Vec::new();
        match FsFile::open(self.path()) {
            Ok(mut f) => {
                f.read_to_end(&mut data).map_err(|e| e.to_string())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RegistryFile::default());
            }
            Err(e) => return Err(e.to_string()),
        }
        if data.is_empty() {
            return Ok(RegistryFile::default());
        }
        let mut f: RegistryFile = serde_json::from_slice(&data)
            .map_err(|e| format!("registry {} is corrupt: {e}", self.path().display()))?;
        f.version = FILE_VERSION;
        Ok(f)
    }

    fn write(&self, f: &RegistryFile) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(f).map_err(|e| e.to_string())?;
        let mut tmp = tempfile_in(&self.dir)?;
        let name = tmp.1.clone();
        use std::io::Write;
        let cleanup = |tmp: &mut (FsFile, PathBuf)| {
            let _ = std::fs::remove_file(&tmp.1);
        };
        if tmp.0.write_all(&data).is_err() {
            cleanup(&mut tmp);
            return Err("writing registry".into());
        }
        if tmp.0.sync_all().is_err() {
            cleanup(&mut tmp);
            return Err("syncing registry".into());
        }
        drop(tmp.0);
        std::fs::set_permissions(&name, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .map_err(|e| e.to_string())?;
        std::fs::rename(&name, self.path()).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Load reads a consistent snapshot under a shared lock.
    pub fn load(&self) -> Result<RegistryFile, String> {
        let _lf = self.lock(libc::LOCK_SH)?;
        self.read()
    }

    /// Update runs `f` on the current registry under an exclusive lock and
    /// writes the result back atomically if it returns Ok. Keep the closure
    /// short: it blocks every other devrun invocation.
    pub fn update<T>(
        &self,
        f: impl FnOnce(&mut RegistryFile) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lf = self.lock(libc::LOCK_EX)?;
        let mut file = self.read()?;
        let out = f(&mut file)?;
        self.write(&file)?;
        Ok(out)
    }
}

/// Create a uniquely named temp file in dir, like Go's os.CreateTemp.
fn tempfile_in(dir: &std::path::Path) -> Result<(FsFile, PathBuf), String> {
    for i in 0..10000u32 {
        let name = dir.join(format!("registry-{}-{i}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&name) {
            Ok(f) => return Ok((f, name)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("cannot create temp registry file".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key_s: &str, ports: &[(&str, i32)]) -> Entry {
        let (project, rest) = key_s.split_once(':').unwrap();
        let (wt, name) = rest.rsplit_once(':').unwrap();
        Entry {
            key: key_s.into(),
            project: project.into(),
            worktree: wt.into(),
            branch: "main".into(),
            name: name.into(),
            kind: "proc".into(),
            pid: 0,
            pgid: 0,
            pid_start: 0,
            cmd: vec![],
            ports: ports.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            primary_port: String::new(),
            compose_project: String::new(),
            started_at: Time(0),
            owner: None,
            flagged: None,
            log: String::new(),
        }
    }

    #[test]
    fn key_format() {
        assert_eq!(key("app", "/w/app", "web"), "app:/w/app:web");
    }

    #[test]
    fn port_priority() {
        let mut e = entry("a:/w:web", &[("API", 41001), ("PORT", 41002), ("WEB", 41003)]);
        assert_eq!(e.port(), 41002, "PORT beats alphabetical");
        e.primary_port = "WEB".into();
        assert_eq!(e.port(), 41003, "primary beats PORT");
        e.primary_port = "MISSING".into();
        assert_eq!(e.port(), 41002, "missing primary falls back to PORT");
        let e2 = entry("a:/w:web", &[("ZZZ", 41009), ("API", 41001)]);
        assert_eq!(e2.port(), 41001, "alphabetically first name");
        let e3 = entry("a:/w:web", &[]);
        assert_eq!(e3.port(), 0);
        assert_eq!(e3.url(), "");
        assert_eq!(e.url(), "http://127.0.0.1:41002");
    }

    #[test]
    fn file_ops() {
        let mut f = RegistryFile::default();
        f.upsert(entry("a:/w:web", &[("PORT", 41001)]));
        f.upsert(entry("a:/w:api", &[("PORT", 41002)]));
        f.upsert(entry("a:/w:web", &[("PORT", 41003)]));
        assert_eq!(f.entries.len(), 2, "upsert replaces by key");
        assert_eq!(f.find("a:/w:web").unwrap().port(), 41003);
        assert!(f.find("nope").is_none());
        let used = f.ports_in_use("a:/w:web");
        assert!(used.contains(&41002) && !used.contains(&41003));
        assert!(f.remove("a:/w:web"));
        assert!(!f.remove("a:/w:web"));
    }

    #[test]
    fn json_shape_matches_go() {
        let mut e = entry("a:/w:web", &[("PORT", 41001)]);
        e.started_at = Time::from_unix(1_789_000_000, 0);
        e.owner = Some(Owner {
            herdr_pane: "p1".into(),
            ..Default::default()
        });
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"pid_start\":0"), "{s}");
        assert!(s.contains("\"primary_port\"") == false, "empty primary omitted: {s}");
        assert!(s.contains("\"herdr_pane\":\"p1\""), "{s}");
        assert!(!s.contains("herdr_workspace"), "empty owner fields omitted: {s}");
        let back: Entry = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn store_round_trip_and_lock() {
        let dir = std::env::temp_dir().join(format!("devrun-reg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store { dir: dir.clone() };
        let f = store.load().unwrap();
        assert_eq!(f.version, FILE_VERSION);
        assert!(f.entries.is_empty());
        store
            .update(|f| {
                f.upsert(entry("a:/w:web", &[("PORT", 41001)]));
                f.reserved.insert("a:/w:web#PORT".into(), 41001);
                Ok(())
            })
            .unwrap();
        let f = store.load().unwrap();
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.reserved.get("a:/w:web#PORT"), Some(&41001));
        // The on-disk file is valid JSON with the Go field names.
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(raw.contains("\"reserved\""), "{raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_registry_is_an_error() {
        let dir = std::env::temp_dir().join(format!("devrun-reg-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("registry.json"), "{not json").unwrap();
        let store = Store { dir: dir.clone() };
        let err = store.load().unwrap_err();
        assert!(err.contains("corrupt"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
