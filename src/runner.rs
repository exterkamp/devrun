//! How devrun starts, probes, stops and tails a server of a given kind.
//! Adding a kind means adding an implementation, never editing another
//! kind's code.

use crate::registry::Entry;
use std::io::Write;
use std::time::Duration;

/// Spec is everything a Runner needs to start a server.
pub struct Spec {
    /// Identity, cmd, ports, log path already filled in by the CLI.
    pub entry: Entry,
    /// Working directory (the worktree).
    pub dir: String,
    /// Name in Entry.ports whose value substitutes {port}.
    pub primary: String,
}

/// Handle is what a started server is known by; the CLI copies it into the
/// registry entry.
#[derive(Debug, Default)]
pub struct Handle {
    pub pid: i32,
    pub pgid: i32,
    pub pid_start: u64,
    pub compose_project: String,
}

/// LogOpts controls Logs.
pub struct LogOpts {
    pub follow: bool,
    /// Trailing lines to show first (<=0 means all).
    pub lines: i64,
}

/// Runner manages one kind of server.
pub trait Runner {
    fn kind(&self) -> &'static str;
    /// Start launches the server detached and returns immediately.
    fn start(&self, spec: &Spec) -> Result<Handle, String>;
    /// Alive reports whether the server's handle is still live. It must be
    /// cheap and must never match a recycled PID.
    fn alive(&self, e: &Entry) -> bool;
    /// Stop terminates the server, escalating after grace.
    fn stop(&self, e: &Entry, grace: Duration) -> Result<(), String>;
    /// Logs writes the server's log to out. `stop` polls an interrupt flag so
    /// follow mode can end on SIGINT/SIGTERM.
    fn logs(
        &self,
        e: &Entry,
        opts: &LogOpts,
        out: &mut dyn Write,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), String>;
}

/// Get returns the Runner for `kind`.
pub fn get(kind: &str) -> Result<Box<dyn Runner>, String> {
    match kind {
        crate::procmod::KIND => Ok(Box::new(crate::procmod::ProcRunner)),
        crate::docker::KIND => Ok(Box::new(crate::docker::DockerRunner)),
        other => Err(format!(
            "no runner for kind {other:?} (have [\"docker\" \"proc\"])"
        )),
    }
}

/// Registered kind names, sorted.
pub fn kinds() -> Vec<&'static str> {
    vec![crate::docker::KIND, crate::procmod::KIND]
}
