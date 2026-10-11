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
