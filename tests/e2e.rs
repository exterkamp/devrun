//! End-to-end tests: build the real binary and drive it in a temp HOME,
//! mirroring the Go suite's e2e tests. They skip with a message if python3
//! or git is missing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct Rig {
    root: PathBuf,
    state: PathBuf,
    repo: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let root = std::env::temp_dir().join(format!(
            "devrun-e2e-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let state = root.join("state");
        let repo = root.join("repo");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "x"]);
        Rig { root, state, repo }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        self.run_in(&self.repo, args)
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_devrun"))
            .args(args)
            .current_dir(dir)
            .env("DEVRUN_STATE_DIR", &self.state)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn up_url_ls_stop_cycle() {
    if !have("python3") || !have("git") {
        eprintln!("skipping: python3 or git missing");
        return;
    }
    let rig = Rig::new("cycle");
    let (ok, url, err) = rig.run(&[
        "up",
        "--",
        "python3",
        "-m",
        "http.server",
        "{port}",
        "--bind",
        "127.0.0.1",
    ]);
    assert!(ok, "up failed: {err}");
    let url = url.trim().to_string();
    assert!(url.starts_with("http://127.0.0.1:41"), "{url}");

    // The server actually serves.
    let mut resp = String::new();
    let stream = std::net::TcpStream::connect(url.trim_start_matches("http://")).unwrap();
    let mut stream = stream;
    use std::io::Write;
    stream.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    stream.read_to_string(&mut resp).unwrap();
    assert!(resp.contains("200"), "{resp}");

    // Idempotent up: same URL.
    let (ok, url2, _) = rig.run(&["up", "--", "python3", "-m", "http.server", "{port}"]);
    assert!(ok);
    assert_eq!(url2.trim(), url);

    let (ok, ls, _) = rig.run(&["ls", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&ls).unwrap();
    assert_eq!(v[0]["state"], "up");
    assert_eq!(v[0]["url"], url);

    let (ok, u, _) = rig.run(&["url"]);
    assert!(ok && u.trim() == url);

    let (ok, _, _) = rig.run(&["notify", "please look"]);
    assert!(ok);
    let (ok, ls, _) = rig.run(&["ls", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&ls).unwrap();
    assert_eq!(v[0]["flagged"]["message"], "please look");

    let (ok, _, _) = rig.run(&["clear"]);
    assert!(ok);
    let (ok, _, err) = rig.run(&["stop"]);
    assert!(ok, "{err}");
    let (ok, ls, _) = rig.run(&["ls", "--json"]);
    assert!(ok);
    assert_eq!(ls.trim(), "[]");
    // Port is stable: a fresh up reuses the reserved port.
    let (ok, url3, _) = rig.run(&[
        "up",
        "--",
        "python3",
        "-m",
        "http.server",
        "{port}",
        "--bind",
        "127.0.0.1",
    ]);
    assert!(ok);
    assert_eq!(url3.trim(), url);
    let (ok, _, _) = rig.run(&["stop"]);
    assert!(ok);
}

#[test]
fn named_servers_and_targets() {
    if !have("python3") || !have("git") {
        eprintln!("skipping: python3 or git missing");
        return;
    }
    let rig = Rig::new("named");
    for name in ["web", "api"] {
        let (ok, _, err) = rig.run(&[
            "up",
            "--name",
            name,
            "--",
            "python3",
            "-m",
            "http.server",
            "{port}",
            "--bind",
            "127.0.0.1",
        ]);
        assert!(ok, "up {name}: {err}");
    }
    // url with no target is ambiguous with two servers here.
    let (ok, _, _) = rig.run(&["url"]);
    assert!(!ok);
    let (ok, u, _) = rig.run(&["url", "api"]);
    assert!(ok && u.contains("127.0.0.1"));
    // stop by project scope.
    let (ok, _, err) = rig.run(&["stop", "--all", "--yes"]);
    assert!(ok, "{err}");
    let (_, ls, _) = rig.run(&["ls", "--json"]);
    assert_eq!(ls.trim(), "[]");
}

#[test]
fn doctor_gc_and_hook_smoke() {
    if !have("git") {
        eprintln!("skipping: git missing");
        return;
    }
    let rig = Rig::new("smoke");
    let (ok, out, _) = rig.run(&["doctor", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v.as_array().unwrap().iter().any(|c| c["name"] == "git"));
    let (ok, out, _) = rig.run(&["gc", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["removed"], serde_json::json!([]));
    let (ok, out, _) = rig.run(&["hook", "wrapper"]);
    assert!(ok && out.contains("gwt-rm()"));
    let (ok, _, err) = rig.run(&["hook", "install"]);
    assert!(ok, "{err}");
    let hook = rig.repo.join(".git/hooks/post-checkout");
    let text = std::fs::read_to_string(hook).unwrap();
    assert!(text.contains("devrun-managed hook"));
}

#[test]
fn watch_once_prints_a_frame() {
    if !have("git") {
        eprintln!("skipping: git missing");
        return;
    }
    let rig = Rig::new("watch");
    let (ok, out, _) = rig.run(&["watch", "--once"]);
    assert!(ok);
    assert!(out.contains("devrun"), "{out}");
    assert!(out.contains("no servers running"), "{out}");
}
