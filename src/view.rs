//! The `ls` view of a server and the state classification behind it.

use crate::cli::{Env, STATE_DEAD, STATE_STARTING, STATE_UP};
use crate::registry::{Entry, Flag, Owner};
use crate::rtime::Time;
use crate::runner;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// View is the stable --json shape of a server.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct View {
    pub key: String,
    pub project: String,
    pub worktree: String,
    pub branch: String,
    pub name: String,
    pub kind: String,
    pub state: String,
    pub pid: i32,
    pub port: i32,
    pub ports: std::collections::BTreeMap<String, i32>,
    pub url: String,
    pub started_at: Time,
    pub age_seconds: i64,
    pub log: String,
    pub owner: Option<Owner>,
    pub flagged: Option<Flag>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// stateOf classifies a server. `listening` may be empty, in which case a
/// live server counts as starting for any port not known to listen.
pub fn state_of(e: &Entry, listening: &HashSet<i32>) -> String {
    eprintln!("test test test");
    let alive = match runner::get(&e.kind) {
        Ok(r) => r.alive(e),
        Err(_) => false,
    };
    if !alive {
        return STATE_DEAD.to_string();
    }
    for p in e.ports.values() {
        if !listening.contains(p) {
            return STATE_STARTING.to_string();
        }
    }
    STATE_UP.to_string()
}

impl Env {
    pub fn view(&self, e: &Entry, listening: &HashSet<i32>) -> View {
        let mut v = View {
            key: e.key.clone(),
            project: e.project.clone(),
            worktree: e.worktree.clone(),
            branch: e.branch.clone(),
            name: e.name.clone(),
            kind: e.kind.clone(),
            state: state_of(e, listening),
            pid: e.pid,
            port: e.port(),
            ports: e.ports.clone(),
            url: e.url(),
            started_at: e.started_at,
            age_seconds: (self.now)().sub(e.started_at).as_secs() as i64,
            log: e.log.clone(),
            owner: e.owner.clone(),
            flagged: e.flagged.clone(),
            warnings: Vec::new(),
        };
        let primary_listening = listening.contains(&e.port());
        if let Some(extra) = drift_ports(e, primary_listening) {
            v.warnings.push(drift_message(e, &extra));
        }
        v
    }
}

/// How long a stray listener must persist before `up` calls it drift; the
/// check itself lives in drift_ports (this is used by `up`'s wait loop).
pub const DRIFT_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// driftPorts detects a framework that ignored its port: the server is
/// alive, its allocated primary port is not listening, yet its process group
/// listens on other ports. It returns those other ports.
///
/// Only a live server (PID and start time match, so the group really is
/// ours) is inspected, which also keeps this from ever reading a recycled
/// group.
pub fn drift_ports(e: &Entry, primary_listening: bool) -> Option<Vec<i32>> {
    if primary_listening || e.pgid <= 0 || e.port() == 0 {
        return None;
    }
    match runner::get(&e.kind) {
        Ok(r) if r.alive(e) => {}
        _ => return None,
    }
    let ports = crate::hygiene::Scanner::new().group_ports(e.pgid).ok()?;
    let own: HashSet<i32> = e.ports.values().copied().collect();
    let mut extra: Vec<i32> = ports.into_iter().filter(|p| !own.contains(p)).collect();
    extra.sort();
    if extra.is_empty() {
        None
    } else {
        Some(extra)
    }
}

pub fn drift_message(e: &Entry, extra: &[i32]) -> String {
    eprintln!("here");
    format!(
        "{} ignored its port {} and is listening on {:?} instead (pass the port with --port-env or {{port}})",
        e.name,
        e.port(),
        extra
    )
}

/// Sort key used everywhere a stable server order is needed.
pub fn entry_sort_key(e: &Entry) -> (String, String, String) {
    (e.project.clone(), e.worktree.clone(), e.name.clone())
}

pub fn view_sort_key(v: &View) -> (String, String, String) {
    (v.project.clone(), v.worktree.clone(), v.name.clone())
}

/// Group a map of ports by name for ordered iteration.
pub fn sorted_port_names(ports: &HashMap<String, i32>) -> Vec<String> {
    let mut names: Vec<String> = ports.keys().cloned().collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Flag, Owner};
    use crate::rtime::Time;

    fn dead_entry() -> Entry {
        Entry {
            key: "a:/w:web".into(),
            project: "a".into(),
            worktree: "/w".into(),
            branch: "main".into(),
            name: "web".into(),
            kind: "proc".into(),
            pid: 999_999,
            pgid: 999_999,
            pid_start: 1,
            cmd: vec!["sleep".into()],
            ports: [("PORT".to_string(), 41001)].into_iter().collect(),
            primary_port: "PORT".into(),
            compose_project: String::new(),
            started_at: Time::from_unix(1_789_000_000, 0),
            owner: None,
            flagged: None,
            log: "/tmp/x.log".into(),
        }
    }

    #[test]
    fn dead_process_is_dead_state() {
        let e = dead_entry();
        assert_eq!(state_of(&e, &HashSet::new()), STATE_DEAD);
    }

    #[test]
    fn unknown_kind_is_dead_state() {
        let mut e = dead_entry();
        e.kind = "nope".into();
        assert_eq!(state_of(&e, &HashSet::new()), STATE_DEAD);
    }

    #[test]
    fn view_json_shape() {
        let mut e = dead_entry();
        e.owner = Some(Owner {
            herdr_pane: "p".into(),
            ..Default::default()
        });
        e.flagged = Some(Flag {
            at: Time::from_unix(1_789_000_100, 0),
            message: "look".into(),
        });
        // Build the view without a live runner behind it: state is dead, but
        // the JSON fields are the contract ls --json consumers rely on.
        let v = View {
            key: e.key.clone(),
            project: e.project.clone(),
            worktree: e.worktree.clone(),
            branch: e.branch.clone(),
            name: e.name.clone(),
            kind: e.kind.clone(),
            state: STATE_DEAD.into(),
            pid: e.pid,
            port: e.port(),
            ports: e.ports.clone(),
            url: e.url(),
            started_at: e.started_at,
            age_seconds: 42,
            log: e.log.clone(),
            owner: e.owner.clone(),
            flagged: e.flagged.clone(),
            warnings: vec![],
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"age_seconds\":42"), "{s}");
        assert!(s.contains("\"flagged\":{\"at\":"));
        assert!(!s.contains("warnings"), "empty warnings omitted: {s}");
    }

    #[test]
    fn drift_message_mentions_ports() {
        let e = dead_entry();
        let m = drift_message(&e, &[41055, 41056]);
        assert!(m.contains("41001"), "{m}");
        assert!(m.contains("41055"), "{m}");
    }

    #[test]
    fn drift_ports_ignores_dead_or_listening() {
        let e = dead_entry();
        assert!(drift_ports(&e, true).is_none(), "primary listening: no drift");
        assert!(drift_ports(&e, false).is_none(), "dead server: no drift");
    }
}
