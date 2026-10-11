//! Talks to the herdr CLI. Every call goes through the binary (never the
//! socket directly), has a timeout, and understands herdr's JSON envelope:
//! {"id":...,"result":{...}} or {"id":...,"error":{"code","message"}}.
//!
//! See docs/herdr-notes.md for the behaviour this encodes.

use serde::Deserialize;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Bounds every herdr call so a hung server never blocks devrun.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// An error envelope returned by herdr.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "herdr: {}: {}", self.code, self.message)
    }
}

/// ServerNotRunning reports whether err is herdr's "no server" error.
pub fn server_not_running(err: &str) -> bool {
    err.contains("server_not_running")
}

/// InPane reports whether this process runs inside a herdr pane and has a
/// pane id to attribute work to. HERDR_ENV alone is not enough: with no pane
/// id, `--current` commands silently resolve to the *focused* pane.
pub fn in_pane() -> bool {
    std::env::var("HERDR_ENV").map(|v| v == "1").unwrap_or(false)
        && !std::env::var("HERDR_PANE_ID").unwrap_or_default().is_empty()
}

/// Binary finds the herdr executable: $HERDR_BIN_PATH, else PATH.
pub fn binary() -> Option<String> {
    if let Ok(p) = std::env::var("HERDR_BIN_PATH") {
        if !p.is_empty() {
            if let Ok(st) = std::fs::metadata(&p) {
                use std::os::unix::fs::PermissionsExt;
                if !st.is_dir() && st.permissions().mode() & 0o111 != 0 {
                    return Some(p);
                }
            }
        }
    }
    crate::procmod::which("herdr")
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Run executes herdr with args and returns the envelope's result. A
/// non-zero exit with a parseable error envelope yields an ApiError string.
pub fn run(timeout: Duration, args: &[&str]) -> Result<serde_json::Value, String> {
    let bin = binary().ok_or("herdr not found (set HERDR_BIN_PATH or put it on PATH)")?;
    let timeout = if timeout.is_zero() { DEFAULT_TIMEOUT } else { timeout };
    let mut cmd = Command::new(&bin);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = match crate::docker::run_with_timeout(&mut cmd, timeout) {
        Some(Ok(o)) => o,
        Some(Err(e)) => return Err(format!("herdr {}: {e}", args.first().copied().unwrap_or(""))),
        None => {
            return Err(format!(
                "herdr {}: timed out after {}",
                args.first().copied().unwrap_or(""),
                crate::rtime::format_duration(timeout)
            ))
        }
    };
    #[derive(Deserialize)]
    struct Envelope {
        result: Option<serde_json::Value>,
        error: Option<ErrBody>,
    }
    #[derive(Deserialize)]
    struct ErrBody {
        code: String,
        message: String,
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let env: Envelope = match serde_json::from_str(stdout.trim()) {
        Ok(e) => e,
        Err(e) => {
            if !out.status.success() {
                return Err(format!(
                    "herdr {}: {}: {}",
                    args.first().copied().unwrap_or(""),
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            return Err(format!(
                "herdr {}: unparseable output: {e}",
                args.first().copied().unwrap_or("")
            ));
        }
    };
    if let Some(err) = env.error {
        return Err(ApiError {
            code: err.code,
            message: err.message,
        }
        .to_string());
    }
    if !out.status.success() {
        return Err(format!(
            "herdr {}: {}",
            args.first().copied().unwrap_or(""),
            out.status
        ));
    }
    Ok(env.result.unwrap_or(serde_json::Value::Null))
}

/// Owner is who is running devrun, as far as herdr can tell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Owner {
    pub pane: String,
    pub workspace: String,
    pub tab: String,
    pub agent: String,
}

/// DetectOwner reads the pane/tab/workspace ids from the environment and asks
/// herdr for the agent name (falling back to the agent kind). It returns None
/// outside a herdr pane. Lookup failures leave Agent empty but keep the ids.
pub fn detect_owner() -> Option<Owner> {
    if !in_pane() {
        return None;
    }
    let mut o = Owner {
        pane: std::env::var("HERDR_PANE_ID").unwrap_or_default(),
        workspace: std::env::var("HERDR_WORKSPACE_ID").unwrap_or_default(),
        tab: std::env::var("HERDR_TAB_ID").unwrap_or_default(),
        agent: String::new(),
    };
    if let Ok(raw) = run(Duration::from_secs(2), &["agent", "list"]) {
        o.agent = agent_for(&raw, &o.pane);
    }
    Some(o)
}

fn agent_for(raw: &serde_json::Value, pane: &str) -> String {
    #[derive(Deserialize)]
    struct AgentList {
        #[serde(default)]
        agents: Vec<Agent>,
    }
    #[derive(Deserialize)]
    struct Agent {
        #[serde(default)]
        agent: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        pane_id: String,
    }
    let list: AgentList = match serde_json::from_value(raw.clone()) {
        Ok(l) => l,
        Err(_) => return String::new(),
    };
    for a in &list.agents {
        if a.pane_id == pane {
            if !a.name.is_empty() {
                return a.name.clone();
            }
            return a.agent.clone();
        }
    }
    String::new()
}

/// PaneExists reports whether the pane is still in the session.
pub fn pane_exists(pane: &str) -> Result<bool, String> {
    match run(Duration::from_secs(2), &["pane", "get", pane]) {
        Ok(_) => Ok(true),
        Err(e) => {
            if e.contains("herdr:") && !server_not_running(&e) && e.contains(": ") {
                // An API error other than "no server" means the pane is gone.
                if e.starts_with("herdr: ") && !e.contains("timed out") {
                    return Ok(false);
                }
            }
            Err(e)
        }
    }
}

/// Truncate shortens s to at most n runes, ending in an ellipsis.
pub fn truncate(s: &str, n: usize) -> String {
    let r: Vec<char> = s.chars().collect();
    if r.len() <= n {
        return s.to_string();
    }
    if n <= 1 {
        return r[..n].iter().collect();
    }
    let mut out: String = r[..n - 1].iter().collect();
    out.push('…');
    out
}

/// NotifyResult is herdr's answer to notification.show.
#[derive(Debug, Deserialize, Default)]
pub struct NotifyResult {
    #[serde(default)]
    pub shown: bool,
    #[serde(default)]
    pub reason: String,
}

/// Notify shows a toast. Title is limited to 80 runes and body to 240 by
/// herdr; we truncate first so the URL-first body keeps the part that matters.
pub fn notify(title: &str, body: &str, sound: &str) -> Result<NotifyResult, String> {
    let mut args: Vec<String> = vec![
        "notification".into(),
        "show".into(),
        truncate(title, 80),
    ];
    if !body.is_empty() {
        args.push("--body".into());
        args.push(truncate(body, 240));
    }
    if !sound.is_empty() {
        args.push("--sound".into());
        args.push(sound.to_string());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let raw = run(DEFAULT_TIMEOUT, &refs)?;
    serde_json::from_value(raw).map_err(|e| e.to_string())
}

/// Snapshot is the subset of `herdr api snapshot` devrun reads.
#[derive(Debug, Deserialize, Default, Clone, PartialEq)]
pub struct Snapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: i64,
}

/// GetSnapshot fetches `herdr api snapshot`.
pub fn get_snapshot() -> Result<Snapshot, String> {
    #[derive(Deserialize)]
    struct Wrap {
        snapshot: Snapshot,
    }
    let raw = run(DEFAULT_TIMEOUT, &["api", "snapshot"])?;
    let w: Wrap = serde_json::from_value(raw).map_err(|e| e.to_string())?;
    Ok(w.snapshot)
}

/// Version runs `herdr --version`.
pub fn version() -> Result<String, String> {
    let bin = binary().ok_or("herdr not found")?;
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("herdr --version: {}", out.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_cases() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("123456", 3), "12…");
        assert_eq!(truncate("ab", 1), "a");
        assert_eq!(truncate("héllo wörld", 5), "héll…");
        assert_eq!(truncate("", 3), "");
    }

    #[test]
    fn agent_for_picks_the_pane() {
        let raw = serde_json::json!({
            "agents": [
                {"agent": "codex", "name": "", "pane_id": "p1"},
                {"agent": "claude", "name": "claude-2", "pane_id": "p2"},
                {"agent": "claude", "name": "", "pane_id": "p3"}
            ]
        });
        assert_eq!(agent_for(&raw, "p1"), "codex", "falls back to kind");
        assert_eq!(agent_for(&raw, "p2"), "claude-2", "name wins");
        assert_eq!(agent_for(&raw, "nope"), "");
        assert_eq!(agent_for(&serde_json::json!({}), "p1"), "");
    }

    #[test]
    fn in_pane_needs_env_and_pane_id() {
        // The test process is not a herdr pane; just assert no panic and the
        // function agrees with the raw environment.
        let expect = std::env::var("HERDR_ENV").map(|v| v == "1").unwrap_or(false)
            && !std::env::var("HERDR_PANE_ID").unwrap_or_default().is_empty();
        assert_eq!(in_pane(), expect);
    }

    #[test]
    fn server_not_running_detection() {
        assert!(server_not_running("herdr: server_not_running: no server"));
        assert!(!server_not_running("herdr: other: x"));
    }

    #[test]
    fn run_without_herdr_errors_cleanly() {
        if binary().is_none() {
            let err = run(Duration::from_millis(100), &["api", "snapshot"]).unwrap_err();
            assert!(err.contains("herdr not found"), "{err}");
        }
    }
}
