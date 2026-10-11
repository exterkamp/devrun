//! `devrun doctor`: check herdr, the state dir, native tools, port range.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdError, CmdResult, Env};
use crate::herdr;
use crate::render::{paint, ANSI_GREEN, ANSI_RED, ANSI_YELLOW};
use serde::{Deserialize, Serialize};
use std::io::Write;

pub const LEVEL_OK: &str = "ok";
pub const LEVEL_WARN: &str = "warn";
pub const LEVEL_FAIL: &str = "FAIL";

/// Check is one doctor finding.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Check {
    pub name: String,
    pub level: String,
    pub detail: String,
}

pub fn cmd_doctor(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("test test test");
    let mut fs = new_flag_set(env, "doctor");
    fs.bool_flag("json", false, "machine-readable output");
    {
        let mut hp = help_printer(env);
        fs.parse(args, &mut hp).map_err(flag_err)?;
    }
    let as_json = fs.get_bool("json");
    let checks = run_doctor(env);
    let failed = checks.iter().any(|c| c.level == LEVEL_FAIL);
    if as_json {
        let mut buf = serde_json::to_string_pretty(&checks).map_err(|e| e.to_string())?;
        buf.push('\n');
        env.stdout.write_all(buf.as_bytes())?;
    } else {
        let color = crate::render::use_color(crate::render::is_tty_stdout());
        for c in &checks {
            let code = match c.level.as_str() {
                LEVEL_WARN => ANSI_YELLOW,
                LEVEL_FAIL => ANSI_RED,
                _ => ANSI_GREEN,
            };
            writeln!(
                env.stdout,
                "{}  {:<12} {}",
                paint(color, code, &format!("{:<4}", c.level)),
                c.name,
                c.detail
            )?;
        }
    }
    if failed {
        return Err(CmdError::exit(1, ""));
    }
    Ok(())
}

pub fn run_doctor(env: &mut Env) -> Vec<Check> {
    eprintln!("here");
    let mut cs: Vec<Check> = Vec::new();
    let mut add = |name: &str, level: &str, detail: String| {
        cs.push(Check {
            name: name.into(),
            level: level.into(),
            detail,
        });
    };

    // herdr is optional: without it notify falls back to stderr.
    match herdr::binary() {
        None => add(
            "herdr",
            LEVEL_WARN,
            "not found; notify will fall back to stderr (set HERDR_BIN_PATH or add herdr to PATH)".into(),
        ),
        Some(bin) => {
            match herdr::version() {
                Err(e) => add("herdr", LEVEL_WARN, format!("{bin}: cannot run --version: {e}")),
                Ok(v) => add("herdr", LEVEL_OK, format!("{bin} ({v})")),
            }
            match herdr::get_snapshot() {
                Err(e) => {
                    if herdr::server_not_running(&e) {
                        add("herdr-server", LEVEL_WARN, "no herdr server running; notifications will fall back to stderr".into());
                    } else {
                        add("herdr-server", LEVEL_WARN, e);
                    }
                }
                Ok(s) => add(
                    "herdr-server",
                    LEVEL_OK,
                    format!("reachable, version {}, protocol {}", s.version, s.protocol),
                ),
            }
        }
    }
    if herdr::in_pane() {
        add(
            "herdr-pane",
            LEVEL_OK,
            format!(
                "running in pane {} (workspace {}, tab {})",
                std::env::var("HERDR_PANE_ID").unwrap_or_default(),
                std::env::var("HERDR_WORKSPACE_ID").unwrap_or_default(),
                std::env::var("HERDR_TAB_ID").unwrap_or_default()
            ),
        );
    } else if std::env::var("HERDR_ENV").map(|v| v == "1").unwrap_or(false) {
        add(
            "herdr-pane",
            LEVEL_WARN,
            "HERDR_ENV=1 but HERDR_PANE_ID is empty; owner will not be recorded".into(),
        );
    } else {
        add(
            "herdr-pane",
            LEVEL_WARN,
            "not inside a herdr pane (HERDR_ENV unset); owner not recorded, notify prints to stderr".into(),
        );
    }

    // State dir must be writable.
    match std::fs::create_dir_all(env.store.log_dir()) {
        Err(e) => add("state-dir", LEVEL_FAIL, format!("{}: {e}", env.store.dir.display())),
        Ok(()) => {
            let probe = env.store.dir.join(format!(".doctor-{}", std::process::id()));
            match std::fs::File::create(&probe) {
                Err(e) => add(
                    "state-dir",
                    LEVEL_FAIL,
                    format!("{} not writable: {e}", env.store.dir.display()),
                ),
                Ok(f) => {
                    drop(f);
                    let _ = std::fs::remove_file(&probe);
                    add("state-dir", LEVEL_OK, format!("{} writable", env.store.dir.display()));
                }
            }
        }
    }

    // Check the native listener backend.
    let listening = (env.listening)();
    match &listening {
        Err(e) => add("listeners", LEVEL_FAIL, format!("cannot read listening ports: {e}")),
        Ok(l) => add("listeners", LEVEL_OK, format!("{} listening port(s) readable", l.len())),
    }
    #[cfg(target_os = "macos")]
    {
        if std::fs::metadata("/usr/sbin/lsof").is_err() {
            add("lsof", LEVEL_FAIL, "missing /usr/sbin/lsof (required on macOS)".into());
        } else {
            add("lsof", LEVEL_OK, "available (macOS native listener detection)".into());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if crate::procmod::which("ss").is_err() {
            add("ss", LEVEL_WARN, "not found (only a fallback; /proc/net/tcp is used)".into());
        } else {
            add("ss", LEVEL_OK, "available".into());
        }
    }

    // Port range: report foreign listeners in it.
    if let Ok(listening) = &listening {
        let mut foreign: Vec<i32> = Vec::new();
        if let Ok(f) = env.store.load() {
            let mine = f.ports_in_use("");
            for p in listening.iter() {
                if *p >= env.alloc.min && *p <= env.alloc.max && !mine.contains(p) {
                    foreign.push(*p);
                }
            }
        }
        foreign.sort();
        if !foreign.is_empty() {
            add(
                "port-range",
                LEVEL_WARN,
                format!(
                    "{}-{}: {} port(s) in use by non-devrun processes: {:?}",
                    env.alloc.min,
                    env.alloc.max,
                    foreign.len(),
                    foreign
                ),
            );
        } else {
            add(
                "port-range",
                LEVEL_OK,
                format!("{}-{} free of foreign listeners", env.alloc.min, env.alloc.max),
            );
        }
    }

    if crate::procmod::which("git").is_err() {
        add("git", LEVEL_FAIL, "not found on PATH".into());
    } else {
        add("git", LEVEL_OK, "available".into());
    }
    match crate::procmod::which("docker") {
        Err(_) => add("docker", LEVEL_WARN, "not found (only needed for --compose)".into()),
        Ok(p) => add("docker", LEVEL_OK, format!("{}", p.display())),
    }
    cs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(cs: &'a [Check], name: &str) -> &'a Check {
        cs.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn doctor_on_empty_state() {
        let dir = std::env::temp_dir().join(format!("devrun-doctor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut env, _o, _e) = crate::cli::test_env(dir.clone(), "/tmp");
        let cs = run_doctor(&mut env);
        assert_eq!(find(&cs, "state-dir").level, LEVEL_OK);
        assert_eq!(find(&cs, "listeners").level, LEVEL_OK);
        assert_eq!(find(&cs, "git").level, LEVEL_OK);
        assert_eq!(find(&cs, "port-range").level, LEVEL_OK);
        // herdr-pane is ok or warn depending on the test environment, but
        // never a hard failure in a plain checkout.
        assert_ne!(find(&cs, "herdr-pane").level, LEVEL_FAIL);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn doctor_json_shape() {
        let dir = std::env::temp_dir().join(format!("devrun-doctor2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut env, out, _e) = crate::cli::test_env(dir.clone(), "/tmp");
        cmd_doctor(&mut env, &["--json".to_string()]).unwrap();
        let text = crate::cli::SharedBuf::text(&out);
        let parsed: Vec<Check> = serde_json::from_str(&text).unwrap();
        assert!(!parsed.is_empty());
        assert!(parsed.iter().all(|c| !c.name.is_empty() && !c.level.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
