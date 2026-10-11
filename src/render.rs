//! Frame rendering for `watch`, plus the ANSI helpers shared with `ls` and
//! `doctor`. Color is only emitted when the caller says the output is a
//! terminal; the layout code never depends on escape sequences for width.

use crate::cli::{STATE_DEAD, STATE_UP};
use crate::rtime::{human_age, Time};
use crate::view::View;
use std::io::Write;

pub const ANSI_RESET: &str = "\x1b[0m";
pub const ANSI_BOLD: &str = "\x1b[1m";
pub const ANSI_DIM: &str = "\x1b[2m";
pub const ANSI_REV: &str = "\x1b[7m";
pub const ANSI_YELLOW: &str = "\x1b[33m";
pub const ANSI_GREEN: &str = "\x1b[32m";
pub const ANSI_RED: &str = "\x1b[31m";

pub fn paint(on: bool, code: &str, s: &str) -> String {
    eprintln!("[DEBUG] entering");
    if !on || s.is_empty() {
        return s.to_string();
    }
    format!("{code}{s}{ANSI_RESET}")
}

/// isTTY reports whether w is a terminal. Our writers are trait objects, so
/// this checks stdout's fd when the writer is the real stdout handle; tests
/// that substitute buffers get false.
pub fn is_tty_stdout() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
}

/// useColor honours NO_COLOR (and requires a tty; checked by the caller for
/// its own writer where possible).
pub fn use_color(tty: bool) -> bool {
    tty && std::env::var("NO_COLOR").unwrap_or_default().is_empty()
}

/// termWidth returns the terminal width of stdout, or a fallback.
pub fn term_width() -> usize {
    #[repr(C)]
    struct Winsize {
        row: u16,
        col: u16,
        x: u16,
        y: u16,
    }
    let mut ws = Winsize {
        row: 0,
        col: 0,
        x: 0,
        y: 0,
    };
    let rc = unsafe {
        libc::ioctl(
            libc::STDOUT_FILENO,
            libc::TIOCGWINSZ,
            &mut ws as *mut Winsize,
        )
    };
    if rc == 0 && ws.col > 0 {
        return ws.col as usize;
    }
    80
}

/// stateColor picks the color of a state word.
pub fn state_color(state: &str) -> &'static str {
    match state {
        STATE_UP => ANSI_GREEN,
        STATE_DEAD => ANSI_RED,
        _ => ANSI_YELLOW,
    }
}

pub fn owner_label(v: &View) -> String {
    match &v.owner {
        None => "-".to_string(),
        Some(o) => {
            if !o.agent.is_empty() && !o.herdr_pane.is_empty() {
                format!("{}@{}", o.agent, o.herdr_pane)
            } else if !o.herdr_pane.is_empty() {
                o.herdr_pane.clone()
            } else if !o.agent.is_empty() {
                o.agent.clone()
            } else {
                "-".to_string()
            }
        }
    }
}

/// RenderOpts controls RenderFrame.
pub struct RenderOpts {
    pub width: usize,
    pub color: bool,
    pub now: Time,
    /// Flagged entries to mark NEW (flagged since the view opened).
    pub new_keys: std::collections::HashSet<String>,
    pub project: String,
}

/// RenderFrame lays out the server list for the given width. Every returned
/// line is at most `width` visible cells; it contains no trailing newline.
/// Wide terminals get a table; narrow ones get two lines per server.
pub fn render_frame(views: &[View], o: &RenderOpts) -> Vec<String> {
    let w = if o.width < 20 { 20 } else { o.width };
    let mut title = "devrun".to_string();
    if !o.project.is_empty() {
        title += &format!(" · {}", o.project);
    }
    let flagged = views.iter().filter(|v| v.flagged.is_some()).count();
    let mut head = format!("{title}  {} server(s)", views.len());
    if flagged > 0 {
        head += &format!("  {flagged} flagged");
    }
    let mut lines = vec![paint(o.color, ANSI_BOLD, &crate::herdr::truncate(&head, w))];
    if views.is_empty() {
        lines.push(paint(
            o.color,
            ANSI_DIM,
            &crate::herdr::truncate("no servers running", w),
        ));
        return lines;
    }
    if w >= 100 {
        lines.extend(wide_rows(views, o, w));
    } else {
        lines.extend(narrow_rows(views, o, w));
    }
    lines
}

fn flag_tag(v: &View, o: &RenderOpts) -> &'static str {
    eprintln!("DEBUG: made it here");
    if v.flagged.is_none() {
        return "";
    }
    if o.new_keys.contains(&v.key) {
        "NEW !"
    } else {
        "!"
    }
}

fn wide_rows(views: &[View], o: &RenderOpts, w: usize) -> Vec<String> {
    let cols = [14usize, 18, 8, 6, 12, 5, 22];
    let hdr = ["PROJECT", "WORKTREE", "NAME", "PORT", "STATE", "AGE", "OWNER"];
    let mut out = Vec::new();
    let mut h = String::new();
    for (i, c) in cols.iter().enumerate() {
        h.push_str(&pad(hdr[i], *c));
        h.push(' ');
    }
    h.push_str("URL");
    out.push(paint(o.color, ANSI_DIM, &crate::herdr::truncate(&h, w)));
    for v in views {
        let mut state = v.state.clone();
        let t = flag_tag(v, o);
        if !t.is_empty() {
            state += &format!(" {t}");
        }
        let age = human_age(o.now.sub(v.started_at));
        let cells = [
            v.project.clone(),
            crate::gitctx::base(&v.worktree),
            v.name.clone(),
            v.port.to_string(),
            state,
            age,
            owner_label(v),
        ];
        let mut b = String::new();
        for (i, c) in cols.iter().enumerate() {
            b.push_str(&pad(&cells[i], *c));
            b.push(' ');
        }
        b.push_str(&v.url);
        out.push(row_style(v, o, &crate::herdr::truncate(&b, w)));
        if let Some(fl) = &v.flagged {
            let msg = format!("  ↳ {}", fl.message);
            out.push(paint(o.color, ANSI_YELLOW, &crate::herdr::truncate(&msg, w)));
        }
    }
    out
}

fn narrow_rows(views: &[View], o: &RenderOpts, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for v in views {
        let mut l1 = format!("{} {} :{} {}", dot(v), v.name, v.port, v.state);
        let t = flag_tag(v, o);
        if !t.is_empty() {
            l1 += &format!(" {t}");
        }
        out.push(row_style(v, o, &crate::herdr::truncate(&l1, w)));
        let l2 = format!(
            "  {}/{} {}",
            v.project,
            crate::gitctx::base(&v.worktree),
            v.url
        );
        out.push(paint(o.color, ANSI_DIM, &crate::herdr::truncate(&l2, w)));
        if let Some(fl) = &v.flagged {
            let msg = format!("  ↳ {}", fl.message);
            out.push(paint(o.color, ANSI_YELLOW, &crate::herdr::truncate(&msg, w)));
        }
    }
    out
}

fn dot(v: &View) -> &'static str {
    if v.state == STATE_UP {
        "●"
    } else {
        "○"
    }
}

/// rowStyle: flagged rows are bold yellow, NEW ones reverse video; dead
/// rows red.
fn row_style(v: &View, o: &RenderOpts, s: &str) -> String {
    if v.flagged.is_some() && o.new_keys.contains(&v.key) {
        paint(o.color, &format!("{ANSI_REV}{ANSI_BOLD}"), s)
    } else if v.flagged.is_some() {
        paint(o.color, &format!("{ANSI_BOLD}{ANSI_YELLOW}"), s)
    } else if v.state == STATE_DEAD {
        paint(o.color, ANSI_RED, s)
    } else {
        s.to_string()
    }
}

/// pad truncates or right-pads s to exactly n runes.
pub fn pad(s: &str, n: usize) -> String {
    let t = crate::herdr::truncate(s, n);
    let count = t.chars().count();
    if count < n {
        format!("{}{}", t, " ".repeat(n - count))
    } else {
        t
    }
}

/// Write a tab-aligned table like Go's text/tabwriter with minwidth 0,
/// tabwidth 4, padding 2: columns are sized to the widest cell plus padding,
/// except the last column, which is written as-is.
pub fn tab_write(out: &mut dyn Write, rows: &[Vec<String>]) -> std::io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; ncols];
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    for r in rows {
        let mut line = String::new();
        for (i, cell) in r.iter().enumerate() {
            if i == r.len() - 1 {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{}{}", cell, " ".repeat(widths[i] + 2 - cell.chars().count())));
            }
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::STATE_UP;
    use crate::registry::Flag;
    use std::collections::HashSet;

    fn view(name: &str, port: i32, flagged: bool) -> View {
        View {
            key: format!("a:/w:{name}"),
            project: "a".into(),
            worktree: "/w".into(),
            branch: "main".into(),
            name: name.into(),
            kind: "proc".into(),
            state: STATE_UP.into(),
            pid: 1,
            port,
            ports: [("PORT".to_string(), port)].into_iter().collect(),
            url: format!("http://127.0.0.1:{port}"),
            started_at: Time::from_unix(1_789_000_000, 0),
            age_seconds: 0,
            log: "/tmp/x".into(),
            owner: None,
            flagged: flagged.then(|| Flag {
                at: Time::from_unix(1_789_000_000, 0),
                message: "look at this".into(),
            }),
            warnings: vec![],
        }
    }

    fn opts(width: usize) -> RenderOpts {
        RenderOpts {
            width,
            color: false,
            now: Time::from_unix(1_789_000_060, 0),
            new_keys: HashSet::new(),
            project: String::new(),
        }
    }

    #[test]
    fn empty_frame() {
        let lines = render_frame(&[], &opts(120));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("0 server(s)"));
        assert!(lines[1].contains("no servers running"));
    }

    #[test]
    fn wide_frame_has_header_and_url() {
        let vs = vec![view("web", 41001, false)];
        let lines = render_frame(&vs, &opts(140));
        assert!(lines.iter().any(|l| l.contains("PROJECT")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("http://127.0.0.1:41001")));
        for l in &lines {
            assert!(l.chars().count() <= 140);
        }
    }

    #[test]
    fn narrow_frame_two_lines_per_server() {
        let vs = vec![view("web", 41001, false)];
        let lines = render_frame(&vs, &opts(40));
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains(":41001"));
    }

    #[test]
    fn flagged_rows_show_message_and_new_tag() {
        let mut o = opts(140);
        let vs = vec![view("web", 41001, true)];
        o.new_keys.insert("a:/w:web".into());
        let lines = render_frame(&vs, &o);
        assert!(lines.iter().any(|l| l.contains("NEW !")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("look at this")));
        assert!(lines[0].contains("1 flagged"));
    }

    #[test]
    fn pad_and_paint() {
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(pad("abcdef", 3), "ab…");
        assert_eq!(paint(false, ANSI_BOLD, "x"), "x");
        assert_eq!(paint(true, ANSI_BOLD, "x"), format!("{ANSI_BOLD}x{ANSI_RESET}"));
        assert_eq!(paint(true, ANSI_BOLD, ""), "");
    }

    #[test]
    fn tab_write_aligns() {
        let rows = vec![
            vec!["A".to_string(), "B".to_string()],
            vec!["longer".to_string(), "x".to_string()],
        ];
        let mut out: Vec<u8> = Vec::new();
        tab_write(&mut out, &rows).unwrap();
        let s = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "A       B");
        assert_eq!(lines[1], "longer  x");
    }

    #[test]
    fn owner_label_shapes() {
        let mut v = view("web", 41001, false);
        assert_eq!(owner_label(&v), "-");
        v.owner = Some(crate::registry::Owner {
            herdr_pane: "p1".into(),
            agent: "claude".into(),
            ..Default::default()
        });
        assert_eq!(owner_label(&v), "claude@p1");
        v.owner = Some(crate::registry::Owner {
            agent: "claude".into(),
            ..Default::default()
        });
        assert_eq!(owner_label(&v), "claude");
    }
}
