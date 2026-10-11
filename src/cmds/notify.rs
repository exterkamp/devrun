//! `devrun notify` and `devrun clear`: flag a server for human inspection.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdError, CmdResult, Env};
use crate::herdr;
use crate::registry::{Entry, Flag};
use std::io::Write;

/// splitNameMessage interprets `notify [target] "message"`: one positional
/// is the message, two are name then message.
pub fn split_name_message(pos: &[String]) -> Result<(String, String), CmdError> {
    match pos.len() {
        1 => Ok((String::new(), pos[0].clone())),
        2 => Ok((pos[0].clone(), pos[1].clone())),
        _ => Err(CmdError::msg("usage: devrun notify [target] \"message\"")),
    }
}

pub fn cmd_notify(env: &mut Env, args: &[String]) -> CmdResult {
    let mut fs = new_flag_set(env, "notify");
    fs.bool_flag("json", false, "machine-readable output");
    fs.string_flag("sound", "request", "herdr sound: none, done or request");
    let pos = {
        let mut hp = help_printer(env);
        fs.parse_interspersed(args, &mut hp).map_err(flag_err)?
    };
    let as_json = fs.get_bool("json");
    let sound = fs.get_string("sound");
    let (name, msg) = split_name_message(&pos)?;
    if msg.trim().is_empty() {
        return Err(CmdError::msg("empty message"));
    }

    // Record the flag first: it must stick even if herdr is missing or slow.
    let now = (env.now)();
    let msg2 = msg.clone();
    let entry = env.store.update(|f| {
        let cur = env.resolve_entry(f, "notify", &name).map_err(crate::cli::err_string)?;
        let idx = f
            .entries
            .iter()
            .position(|e| e.key == cur.key)
            .ok_or("entry vanished")?;
        f.entries[idx].flagged = Some(Flag {
            at: now,
            message: msg2.clone(),
        });
        Ok(f.entries[idx].clone())
    })?;

    let (title, body) = notify_text(&entry, &msg);
    let (delivered, reason) = deliver(&title, &body, &sound);
    if !delivered {
        env.errln(&format!(
            "devrun: NEEDS INSPECTION {title} | {body} | ({reason})"
        ));
    }
    if as_json {
        let obj = serde_json::json!({
            "flagged": true,
            "key": entry.key,
            "url": entry.url(),
            "notified": delivered,
            "reason": reason,
        });
        writeln!(env.stdout, "{}", serde_json::to_string(&obj).map_err(|e| e.to_string())?)?;
    }
    Ok(())
}

/// notifyText builds the toast. The URL leads the body so truncation eats
/// the message tail rather than the URL.
pub fn notify_text(e: &Entry, msg: &str) -> (String, String) {
    let title = format!(
        "{}/{}: inspect :{}",
        e.project,
        crate::gitctx::base(&e.worktree),
        e.port()
    );
    let body = format!("{} {}", e.url(), msg).trim().to_string();
    (title, body)
}

/// deliver shows the toast via herdr when running in a pane. It never fails
/// the caller: the second result says why nothing was shown.
fn deliver(title: &str, body: &str, sound: &str) -> (bool, String) {
    if !herdr::in_pane() {
        return (false, "not running inside herdr".into());
    }
    if herdr::binary().is_none() {
        return (false, "herdr not found".into());
    }
    match herdr::notify(title, body, sound) {
        Err(e) => (false, e),
        Ok(res) => {
            if !res.shown {
                (false, format!("herdr did not show it: {}", res.reason))
            } else {
                (true, res.reason)
            }
        }
    }
}

pub fn cmd_clear(env: &mut Env, args: &[String]) -> CmdResult {
    let mut fs = new_flag_set(env, "clear");
    let pos = {
        let mut hp = help_printer(env);
        fs.parse_interspersed(args, &mut hp).map_err(flag_err)?
    };
    let name = pos.first().cloned().unwrap_or_default();
    let mut note: Option<String> = None;
    env.store.update(|f| {
        let cur = env.resolve_entry(f, "clear", &name).map_err(crate::cli::err_string)?;
        let idx = f
            .entries
            .iter()
            .position(|e| e.key == cur.key)
            .ok_or("entry vanished")?;
        if f.entries[idx].flagged.is_none() {
            note = Some(format!("devrun: {} was not flagged", f.entries[idx].name));
        }
        f.entries[idx].flagged = None;
        Ok(())
    })?;
    if let Some(n) = note {
        env.errln(&n);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtime::Time;

    fn entry() -> Entry {
        Entry {
            key: "app:/home/u/app:web".into(),
            project: "app".into(),
            worktree: "/home/u/app".into(),
            branch: "main".into(),
            name: "web".into(),
            kind: "proc".into(),
            pid: 1,
            pgid: 1,
            pid_start: 0,
            cmd: vec![],
            ports: [("PORT".to_string(), 41001)].into_iter().collect(),
            primary_port: "PORT".into(),
            compose_project: String::new(),
            started_at: Time(0),
            owner: None,
            flagged: None,
            log: String::new(),
        }
    }

    #[test]
    fn split_cases() {
        let one = vec!["hello".to_string()];
        assert_eq!(split_name_message(&one).unwrap(), (String::new(), "hello".into()));
        let two = vec!["web".to_string(), "hello".to_string()];
        assert_eq!(
            split_name_message(&two).unwrap(),
            ("web".to_string(), "hello".to_string())
        );
        assert!(split_name_message(&[]).is_err());
        assert!(split_name_message(&["a".into(), "b".into(), "c".into()]).is_err());
    }

    #[test]
    fn notify_text_shape() {
        let e = entry();
        let (title, body) = notify_text(&e, "check the layout");
        assert_eq!(title, "app/app: inspect :41001");
        assert!(body.starts_with("http://127.0.0.1:41001 "), "URL leads: {body}");
        assert!(body.ends_with("check the layout"));
    }

    #[test]
    fn notify_and_clear_round_trip() {
        let dir = std::env::temp_dir().join(format!("devrun-notify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut env, out, _err) = crate::cli::test_env(dir.clone(), "/tmp");
        env.store
            .update(|f| {
                f.upsert(entry());
                Ok(())
            })
            .unwrap();
        // Outside git with exactly one entry, resolve-by-no-arg finds it.
        cmd_notify(&mut env, &["just look".to_string(), "--json".to_string()]).unwrap();
        let f = env.store.load().unwrap();
        assert_eq!(
            f.entries[0].flagged.as_ref().unwrap().message,
            "just look"
        );
        let out_text = crate::cli::SharedBuf::text(&out);
        assert!(out_text.contains("\"flagged\":true"), "{out_text}");
        assert!(out_text.contains("\"notified\":false"), "no herdr in tests: {out_text}");
        cmd_clear(&mut env, &[]).unwrap();
        let f = env.store.load().unwrap();
        assert!(f.entries[0].flagged.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
