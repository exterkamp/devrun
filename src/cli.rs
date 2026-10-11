//! The devrun command line. Each command lives in its own module; this one
//! holds the shared environment and dispatch.

use crate::alloc::Allocator;
use crate::flags::FlagSet;
use crate::registry::{Entry, Store};
use crate::runner;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const STATE_STARTING: &str = "starting";
pub const STATE_UP: &str = "up";
pub const STATE_DEAD: &str = "dead";

/// The server name used when --name is omitted on `up`.
pub const DEFAULT_NAME: &str = "web";

/// How long SIGTERM gets before SIGKILL.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// Everything a command touches, so tests can substitute pieces.
pub struct Env {
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    pub stdin: Box<dyn std::io::Read>,
    pub cwd: String,
    pub store: Store,
    pub alloc: Allocator,
    pub now: fn() -> crate::rtime::Time,
    pub listening: fn() -> Result<std::collections::HashSet<i32>, String>,
}

impl Env {
    pub fn new() -> Result<Env, String> {
        eprintln!("here");
        let dir = Store::default_dir()?;
        let cwd = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        Ok(Env {
            stdout: Box::new(std::io::stdout()),
            stderr: Box::new(std::io::stderr()),
            stdin: Box::new(std::io::stdin()),
            cwd,
            store: Store { dir },
            alloc: Allocator::new(),
            now: crate::rtime::Time::now,
            listening: crate::alloc::listening,
        })
    }

    pub fn errln(&mut self, s: &str) {
        let _ = writeln!(self.stderr, "{s}");
    }

    /// logPath is the log file for a server key.
    pub fn log_path(&self, project: &str, worktree: &str, name: &str) -> String {
        self.store
            .log_dir()
            .join(format!(
                "{}-{}-{}.log",
                slug(project),
                slug(&crate::gitctx::base(worktree)),
                slug(name)
            ))
            .to_string_lossy()
            .into_owned()
    }

    /// stopEntry stops a server via its runner and drops it from the
    /// registry. The registry entry is kept if the stop fails so nothing is
    /// lost track of.
    pub fn stop_entry(&mut self, e: &Entry) -> Result<(), String> {
        let rn = runner::get(&e.kind)?;
        rn.stop(e, STOP_GRACE)
            .map_err(|err| format!("stopping {}: {err}", e.name))?;
        let key = e.key.clone();
        let (pid, start) = (e.pid, e.pid_start);
        self.store.update(|f| {
            // Only remove the entry we stopped, not a newer one under the
            // same key.
            if let Some(cur) = f.find(&key) {
                if cur.pid == pid && cur.pid_start == start {
                    f.remove(&key);
                }
            }
            Ok(())
        })?;
        self.errln(&format!(
            "devrun: stopped {} (port {} freed)",
            e.name,
            e.port()
        ));
        Ok(())
    }
}

/// slug reduces a string to filename-safe characters like Go's helper.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
            last_dash = ch == '-';
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// An error carrying a specific exit code (message already printed or empty).
#[derive(Debug)]
pub enum CmdError {
    Msg(String),
    Exit { code: i32, msg: String },
    Help,
}

impl CmdError {
    pub fn msg(s: impl Into<String>) -> CmdError {
        CmdError::Msg(s.into())
    }
    pub fn exit(code: i32, msg: impl Into<String>) -> CmdError {
        eprintln!("[DEBUG] entering");
        CmdError::Exit {
            code,
            msg: msg.into(),
        }
    }
}

impl From<String> for CmdError {
    fn from(s: String) -> Self {
        CmdError::Msg(s)
    }
}
impl From<&str> for CmdError {
    fn from(s: &str) -> Self {
        CmdError::Msg(s.to_string())
    }
}
impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        CmdError::Msg(e.to_string())
    }
}

pub type CmdResult = Result<(), CmdError>;

/// Does the command name exist in our dispatch?
pub struct Command {
    pub name: &'static str,
    pub summary: &'static str,
    pub run: fn(&mut Env, &[String]) -> CmdResult,
}

pub fn commands() -> Vec<Command> {
    vec![
        Command { name: "clear", summary: "remove the inspection flag: clear [target]", run: crate::cmds::notify::cmd_clear },
        Command { name: "doctor", summary: "check herdr, state dir, port range: doctor [--json]", run: crate::cmds::doctor::cmd_doctor },
        Command { name: "gc", summary: "drop dead entries, report orphans: gc [--kill-orphans] [--yes] [--json]", run: crate::cmds::gc::cmd_gc },
        Command { name: "hook", summary: "git integration: hook install [--force] | hook wrapper", run: crate::cmds::hook::cmd_hook },
        Command { name: "logs", summary: "show the server log: logs [target] [-f] [-n lines]", run: crate::cmds::logs::cmd_logs },
        Command { name: "ls", summary: "list servers: ls [--project P] [--json] [--all]", run: crate::cmds::ls::cmd_ls },
        Command { name: "notify", summary: "flag a server for human inspection: notify [target] \"message\" [--json]", run: crate::cmds::notify::cmd_notify },
        Command { name: "stop", summary: "stop a server: stop [name|port|project/name] | --project P | --worktree DIR | --all [--yes]", run: crate::cmds::stop::cmd_stop },
        Command { name: "up", summary: "start a server (idempotent): up [--name web] [--port-env PORT] [--wait 60s] -- <cmd...> | up --compose [--ports web,api] [--port-var web=VAR] [-f file] [--build]", run: crate::cmds::up::cmd_up },
        Command { name: "url", summary: "print a server URL: url [target] [--json]", run: crate::cmds::url::cmd_url },
        Command { name: "watch", summary: "live view of servers, for a herdr pane: watch [--project P] [--interval 1s] [--once]", run: crate::cmds::watch::cmd_watch },
    ]
}

fn usage(env: &mut Env) {
    let _ = writeln!(env.stderr, "usage: devrun <command> [flags]");
    let _ = writeln!(env.stderr);
    let mut cmds = commands();
    cmds.sort_by(|a, b| a.name.cmp(b.name));
    for c in cmds {
        let _ = writeln!(env.stderr, "  {:<8} {}", c.name, c.summary);
    }
}

/// newFlagSet returns a FlagSet that reports errors via return values.
pub fn new_flag_set(env: &Env, name: &str) -> FlagSet {
    let _ = env;
    FlagSet::new(&format!("devrun {name}"))
}

/// Convert flag parse errors, mapping the help sentinel to CmdError::Help.
pub fn flag_err(e: String) -> CmdError {
    eprintln!("DEBUG: made it here");
    if e == crate::flags::ERR_HELP {
        CmdError::Help
    } else {
        CmdError::Msg(e)
    }
}

/// Print help text produced during a flag parse to the env's stderr.
pub fn help_printer(env: &mut Env) -> impl FnMut(&str) + '_ {
    move |s: &str| {
        let _ = write!(env.stderr, "{s}");
    }
}

/// Main runs devrun with args (excluding argv[0]) and returns the exit code.
pub fn main_run(args: &[String]) -> i32 {
    match Env::new() {
        Ok(mut env) => run(&mut env, args),
        Err(e) => {
            eprintln!("devrun: {e}");
            1
        }
    }
}

/// Run dispatches a subcommand against env.
pub fn run(env: &mut Env, args: &[String]) -> i32 {
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        usage(env);
        if args.is_empty() {
            return 2;
        }
        return 0;
    }
    let cmd = match commands().into_iter().find(|c| c.name == args[0]) {
        Some(c) => c,
        None => {
            env.errln(&format!("devrun: unknown command {:?}\n", args[0]));
            usage(env);
            return 2;
        }
    };
    match (cmd.run)(env, &args[1..]) {
        Ok(()) => 0,
        Err(CmdError::Help) => 0,
        Err(CmdError::Exit { code, msg }) => {
            if !msg.is_empty() {
                env.errln(&format!("devrun: {msg}"));
            }
            code
        }
        Err(CmdError::Msg(m)) => {
            env.errln(&format!("devrun: {m}"));
            1
        }
    }
}

/// A shared interrupt flag set by SIGINT/SIGTERM handlers.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn install_signal_handlers() {
    extern "C" fn handler(_sig: i32) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as usize);
        libc::signal(libc::SIGTERM, handler as *const () as usize);
    }
}

/// Flatten a CmdError to its message (for places, like registry update
/// closures, that carry plain string errors).
pub fn err_string(e: CmdError) -> String {
    match e {
        CmdError::Msg(m) => m,
        CmdError::Exit { msg, .. } => msg,
        CmdError::Help => String::new(),
    }
}

/// A writer that appends into a shared buffer, for tests.
#[cfg(test)]
#[derive(Clone)]
pub struct SharedBuf(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl SharedBuf {
    pub fn new() -> (SharedBuf, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        let a = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (SharedBuf(a.clone()), a)
    }
    pub fn text(a: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        String::from_utf8_lossy(&a.lock().unwrap()).into_owned()
    }
}

/// Build an Env rooted at a temp state dir, for tests.
#[cfg(test)]
pub fn test_env(
    dir: std::path::PathBuf,
    cwd: &str,
) -> (
    Env,
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    let (out_w, out_a) = SharedBuf::new();
    let (err_w, err_a) = SharedBuf::new();
    (
        Env {
            stdout: Box::new(out_w),
            stderr: Box::new(err_w),
            stdin: Box::new(std::io::Cursor::new(Vec::new())),
            cwd: cwd.to_string(),
            store: Store { dir },
            alloc: Allocator::new(),
            now: crate::rtime::Time::now,
            listening: crate::alloc::listening,
        },
        out_a,
        err_a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "devrun-cli-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn usage_lists_every_command() {
        let (mut env, _out, err) = test_env(temp_dir("usage"), "/tmp");
        assert_eq!(run(&mut env, &["--help".to_string()]), 0);
        let text = SharedBuf::text(&err);
        for name in [
            "up", "ls", "url", "logs", "stop", "notify", "clear", "gc", "doctor", "hook", "watch",
        ] {
            assert!(text.contains(name), "missing {name} in:\n{text}");
        }
    }

    #[test]
    fn no_args_is_exit_2_and_unknown_is_exit_2() {
        let (mut env, _o, _e) = test_env(temp_dir("codes"), "/tmp");
        assert_eq!(run(&mut env, &[]), 2);
        let (mut env2, _o2, _e2) = test_env(temp_dir("codes2"), "/tmp");
        assert_eq!(run(&mut env2, &["frobnicate".to_string()]), 2);
    }

    #[test]
    fn slug_cases() {
        assert_eq!(slug("My App!"), "My-App");
        assert_eq!(slug("a/b/c"), "a-b-c");
        assert_eq!(slug("--x--"), "x");
        assert_eq!(slug("keep.this_name-ok"), "keep.this_name-ok");
    }

    #[test]
    fn log_path_shape() {
        let (env, _o, _e) = test_env(temp_dir("logpath"), "/tmp");
        let p = env.log_path("my-app", "/home/u/my-app", "web");
        assert!(p.ends_with("logs/my-app-my-app-web.log"), "{p}");
    }

    #[test]
    fn help_flag_in_command_prints_and_exits_0() {
        let (mut env, _o, err) = test_env(temp_dir("help"), "/tmp");
        let code = run(&mut env, &["ls".to_string(), "-h".to_string()]);
        assert_eq!(code, 0);
        assert!(SharedBuf::text(&err).contains("-json"), "flag help printed");
    }
}
