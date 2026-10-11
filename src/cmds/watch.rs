//! `devrun watch`: live view of servers, for a herdr pane.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdResult, Env, STATE_DEAD};
use crate::render::{render_frame, term_width, RenderOpts};
use crate::rtime::Time;
use crate::view::{view_sort_key, View};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::time::Duration;

// Terminal control sequences used by watch.
pub const ALT_SCREEN_ON: &str = "\x1b[?1049h";
pub const ALT_SCREEN_OFF: &str = "\x1b[?1049l";
pub const HIDE_CURSOR: &str = "\x1b[?25l";
pub const SHOW_CURSOR: &str = "\x1b[?25h";
pub const CURSOR_HOME: &str = "\x1b[H";
pub const CLEAR_TO_EOL: &str = "\x1b[K";
pub const CLEAR_BELOW: &str = "\x1b[J";

pub fn cmd_watch(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("DEBUG: made it here");
    let mut fs = new_flag_set(env, "watch");
    fs.string_flag("project", "", "only show this project");
    fs.duration_flag("interval", Duration::from_secs(1), "refresh interval");
    fs.bool_flag("once", false, "print one frame and exit");
    {
        let mut hp = help_printer(env);
        fs.parse(args, &mut hp).map_err(flag_err)?;
    }
    let project = fs.get_string("project");
    let mut interval = fs.get_duration("interval");
    let once = fs.get_bool("once");
    if interval < Duration::from_millis(100) {
        interval = Duration::from_millis(100);
    }
    crate::cli::install_signal_handlers();
    let tty = crate::render::is_tty_stdout();
    watch_loop(env, &project, interval, once || !tty, tty)
}

impl Env {
    /// views loads the current server list, sorted like ls, without dead
    /// entries.
    pub fn views(&self, project: &str) -> Result<Vec<View>, String> {
        let f = self.store.load()?;
        let listening = (self.listening)()?;
        let mut vs: Vec<View> = Vec::new();
        for e in &f.entries {
            if !project.is_empty() && e.project != project {
                continue;
            }
            let v = self.view(e, &listening);
            if v.state == STATE_DEAD {
                continue;
            }
            vs.push(v);
        }
        vs.sort_by(|a, b| view_sort_key(a).cmp(&view_sort_key(b)));
        Ok(vs)
    }
}

/// watchLoop redraws until interrupted. With `single` it prints one plain
/// frame (no screen control) and returns. Newly flagged entries, those that
/// were not flagged when the view opened or on the previous refresh, are
/// marked NEW until they are cleared.
pub fn watch_loop(
    env: &mut Env,
    project: &str,
    interval: Duration,
    single: bool,
    tty: bool,
) -> CmdResult {
    let color = tty && std::env::var("NO_COLOR").unwrap_or_default().is_empty();
    let mut seen: HashMap<String, Time> = HashMap::new();
    let mut new_keys: HashSet<String> = HashSet::new();
    let mut first = true;

    let frame = |env: &Env,
                     seen: &mut HashMap<String, Time>,
                     new_keys: &mut HashSet<String>,
                     first: &mut bool|
     -> Result<String, String> {
        let vs = env.views(project)?;
        let mut live: HashSet<String> = HashSet::new();
        for v in &vs {
            match &v.flagged {
                None => {
                    new_keys.remove(&v.key);
                    seen.remove(&v.key);
                }
                Some(fl) => {
                    live.insert(v.key.clone());
                    let is_new = match seen.get(&v.key) {
                        None => !*first,
                        Some(prev) => *prev != fl.at,
                    };
                    if is_new {
                        new_keys.insert(v.key.clone());
                    }
                    seen.insert(v.key.clone(), fl.at);
                }
            }
        }
        new_keys.retain(|k| live.contains(k));
        *first = false;
        let w = if !tty {
            match std::env::var("COLUMNS") {
                Ok(c) if !c.is_empty() => c.trim().parse::<usize>().unwrap_or(term_width()),
                _ => term_width(),
            }
        } else {
            term_width()
        };
        let lines = render_frame(
            &vs,
            &RenderOpts {
                width: w,
                color,
                now: (env.now)(),
                new_keys: new_keys.clone(),
                project: project.to_string(),
            },
        );
        Ok(lines.join("\n"))
    };

    if single {
        let f = frame(env, &mut seen, &mut new_keys, &mut first)?;
        writeln!(env.stdout, "{f}")?;
        return Ok(());
    }

    write!(env.stdout, "{ALT_SCREEN_ON}{HIDE_CURSOR}")?;
    env.stdout.flush()?;
    let result = (|| -> CmdResult {
        let mut last_frame = String::new();
        loop {
            let f = match frame(env, &mut seen, &mut new_keys, &mut first) {
                Ok(f) => f,
                Err(e) => format!("devrun: {e}"),
            };
            if f != last_frame {
                last_frame = f.clone();
                // One write per frame: home, overwrite each line in place
                // (clearing only its tail), then clear what is left below.
                // No full-screen clear, so nothing flashes.
                let mut b = String::from(CURSOR_HOME);
                for l in f.split('\n') {
                    b.push_str(l);
                    b.push_str(CLEAR_TO_EOL);
                    b.push('\n');
                }
                b.push_str(CLEAR_BELOW);
                env.stdout.write_all(b.as_bytes())?;
                env.stdout.flush()?;
            }
            if crate::cli::interrupted() {
                return Ok(());
            }
            std::thread::sleep(interval);
            if crate::cli::interrupted() {
                return Ok(());
            }
        }
    })();
    write!(env.stdout, "{SHOW_CURSOR}{ALT_SCREEN_OFF}")?;
    env.stdout.flush()?;
    result
}
