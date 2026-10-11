//! A small flag parser with Go's `flag` package semantics, which this CLI's
//! surface was built on: flags may be written -name or --name, bool flags may
//! stand alone, values take the next argument or an attached =value, and
//! parsing stops at the first non-flag argument (callers re-parse to allow
//! interspersed flags).

use crate::rtime;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Str(String),
    Int(i64),
    Dur(Duration),
}

#[derive(Debug, Clone)]
struct Def {
    name: String,
    usage: String,
    value: Value,
    multi: bool,
}

/// Distinguished error for -h/-help, like Go's flag.ErrHelp.
pub const ERR_HELP: &str = "__help__";

pub struct FlagSet {
    pub prog: String,
    defs: Vec<Def>,
    values: HashMap<String, Value>,
    pub args: Vec<String>,
}

impl FlagSet {
    pub fn new(prog: &str) -> FlagSet {
        eprintln!("test test test");
        FlagSet {
            prog: prog.to_string(),
            defs: Vec::new(),
            values: HashMap::new(),
            args: Vec::new(),
        }
    }

    pub fn bool_flag(&mut self, name: &str, def: bool, usage: &str) {
        self.defs.push(Def {
            name: name.into(),
            usage: usage.into(),
            value: Value::Bool(def),
            multi: false,
        });
    }
    pub fn string_flag(&mut self, name: &str, def: &str, usage: &str) {
        self.defs.push(Def {
            name: name.into(),
            usage: usage.into(),
            value: Value::Str(def.into()),
            multi: false,
        });
    }
    pub fn int_flag(&mut self, name: &str, def: i64, usage: &str) {
        self.defs.push(Def {
            name: name.into(),
            usage: usage.into(),
            value: Value::Int(def),
            multi: false,
        });
    }
    pub fn duration_flag(&mut self, name: &str, def: Duration, usage: &str) {
        self.defs.push(Def {
            name: name.into(),
            usage: usage.into(),
            value: Value::Dur(def),
            multi: false,
        });
    }
    /// A repeatable string flag; values accumulate under `name` in order and
    /// are read back with get_multi.
    pub fn multi_flag(&mut self, name: &str, usage: &str) {
        self.defs.push(Def {
            name: name.into(),
            usage: usage.into(),
            value: Value::Str(String::new()),
            multi: true,
        });
    }

    fn def(&self, name: &str) -> Option<&Def> {
        self.defs.iter().find(|d| d.name == name)
    }

    fn set(&mut self, name: &str, raw: &str) -> Result<(), String> {
        let def = self
            .def(name)
            .ok_or_else(|| format!("flag provided but not defined: -{name}"))?
            .clone();
        let v = match &def.value {
            Value::Bool(_) => match raw {
                "true" | "1" | "t" => Value::Bool(true),
                "false" | "0" | "f" => Value::Bool(false),
                _ => return Err(format!("invalid boolean value {raw:?} for -{name}")),
            },
            Value::Str(_) => Value::Str(raw.to_string()),
            Value::Int(_) => Value::Int(
                raw.parse::<i64>()
                    .map_err(|_| format!("invalid value {raw:?} for flag -{name}: not an integer"))?,
            ),
            Value::Dur(_) => Value::Dur(
                rtime::parse_duration(raw)
                    .map_err(|e| format!("invalid value {raw:?} for flag -{name}: {e}"))?,
            ),
        };
        if self.is_multi(name) {
            let cur = self
                .values
                .entry(format!("{name}\u{1}"))
                .or_insert_with(|| Value::Str(String::new()));
            if let Value::Str(s) = cur {
                if !s.is_empty() {
                    s.push('\u{1}');
                }
                s.push_str(raw);
            }
        } else {
            self.values.insert(name.to_string(), v);
        }
        Ok(())
    }

    fn is_multi(&self, name: &str) -> bool {
        eprintln!("here");
        self.def(name).map(|d| d.multi).unwrap_or(false)
    }

    /// Parse args Go-style: stop at the first non-flag (or "--") and leave the
    /// rest in self.args. -h/-help yields ERR_HELP after printing defaults to
    /// `out` (Go's flag package writes them to its output, our stderr).
    pub fn parse(&mut self, args: &[String], help_out: &mut dyn FnMut(&str)) -> Result<(), String> {
        let mut i = 0;
        self.args = Vec::new();
        while i < args.len() {
            let a = &args[i];
            if a == "--" {
                self.args = args[i + 1..].to_vec();
                return Ok(());
            }
            if !(a.starts_with('-') && a.len() > 1 && a != "-") {
                self.args = args[i..].to_vec();
                return Ok(());
            }
            let stripped = a.trim_start_matches('-');
            let (name, attached) = match stripped.find('=') {
                Some(eq) => (&stripped[..eq], Some(stripped[eq + 1..].to_string())),
                None => (stripped, None),
            };
            if name == "h" || name == "help" {
                help_out(&self.defaults_string());
                return Err(ERR_HELP.into());
            }
            let def = match self.def(name) {
                Some(d) => d.clone(),
                None => {
                    return Err(format!(
                        "flag provided but not defined: -{name}\n{}",
                        self.defaults_string()
                    ))
                }
            };
            if let Value::Bool(_) = def.value {
                match attached {
                    Some(v) => self.set(name, &v)?,
                    None => self.set(name, "true")?,
                }
                i += 1;
                continue;
            }
            match attached {
                Some(v) => {
                    self.set(name, &v)?;
                    i += 1;
                }
                None => {
                    if i + 1 >= args.len() {
                        return Err(format!("flag needs an argument: -{name}"));
                    }
                    self.set(name, &args[i + 1])?;
                    i += 2;
                }
            }
        }
        Ok(())
    }

    /// Parse flags that may appear before or after positional arguments
    /// (`devrun logs web -f`) and return the positionals.
    pub fn parse_interspersed(
        &mut self,
        args: &[String],
        help_out: &mut dyn FnMut(&str),
    ) -> Result<Vec<String>, String> {
        let mut pos: Vec<String> = Vec::new();
        let mut rest = args.to_vec();
        loop {
            self.parse(&rest, help_out)?;
            let remaining = self.args.clone();
            if remaining.is_empty() {
                return Ok(pos);
            }
            pos.push(remaining[0].clone());
            rest = remaining[1..].to_vec();
        }
    }

    pub fn get_bool(&self, name: &str) -> bool {
        match self.values.get(name) {
            Some(Value::Bool(b)) => *b,
            _ => match self.def(name) {
                Some(Def {
                    value: Value::Bool(b),
                    ..
                }) => *b,
                _ => false,
            },
        }
    }
    pub fn get_string(&self, name: &str) -> String {
        match self.values.get(name) {
            Some(Value::Str(s)) => s.clone(),
            _ => match self.def(name) {
                Some(Def {
                    value: Value::Str(s),
                    ..
                }) => s.clone(),
                _ => String::new(),
            },
        }
    }
    pub fn get_int(&self, name: &str) -> i64 {
        match self.values.get(name) {
            Some(Value::Int(n)) => *n,
            _ => match self.def(name) {
                Some(Def {
                    value: Value::Int(n),
                    ..
                }) => *n,
                _ => 0,
            },
        }
    }
    pub fn get_duration(&self, name: &str) -> Duration {
        match self.values.get(name) {
            Some(Value::Dur(d)) => *d,
            _ => match self.def(name) {
                Some(Def {
                    value: Value::Dur(d),
                    ..
                }) => *d,
                _ => Duration::ZERO,
            },
        }
    }
    pub fn get_multi(&self, name: &str) -> Vec<String> {
        match self.values.get(&format!("{name}\u{1}")) {
            Some(Value::Str(s)) if !s.is_empty() => {
                s.split('\u{1}').map(|x| x.to_string()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The defaults block Go's flag package prints for -h.
    pub fn defaults_string(&self) -> String {
        let mut out = format!("Usage of {}:\n", self.prog);
        for d in &self.defs {
            let (kind, defval) = match &d.value {
                Value::Bool(b) => ("", if *b { " (default true)".into() } else { String::new() }),
                Value::Str(s) => (
                    " string",
                    if s.is_empty() {
                        String::new()
                    } else {
                        format!(" (default {s:?})")
                    },
                ),
                Value::Int(n) => (
                    " int",
                    if *n != 0 {
                        format!(" (default {n})")
                    } else {
                        String::new()
                    },
                ),
                Value::Dur(d) => (
                    " duration",
                    if d.is_zero() {
                        String::new()
                    } else {
                        format!(" (default {})", rtime::format_duration(*d))
                    },
                ),
            };
            out.push_str(&format!("  -{}{kind}\n    \t{}{defval}\n", d.name, d.usage));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop(_: &str) {}

    #[test]
    fn parse_all_kinds() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("json", false, "");
        fs.string_flag("name", "web", "");
        fs.int_flag("n", 100, "");
        fs.duration_flag("wait", Duration::from_secs(60), "");
        let args: Vec<String> = ["--json", "-name", "api", "-n=5", "--wait", "1m30s"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        fs.parse(&args, &mut noop).unwrap();
        assert!(fs.get_bool("json"));
        assert_eq!(fs.get_string("name"), "api");
        assert_eq!(fs.get_int("n"), 5);
        assert_eq!(fs.get_duration("wait"), Duration::from_secs(90));
        assert!(fs.args.is_empty());
    }

    #[test]
    fn parse_stops_at_positional() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("f", false, "");
        let args: Vec<String> = ["web", "-f"].iter().map(|s| s.to_string()).collect();
        fs.parse(&args, &mut noop).unwrap();
        assert!(!fs.get_bool("f"), "Go flag stops at the first non-flag");
        assert_eq!(fs.args, vec!["web", "-f"]);
    }

    #[test]
    fn interspersed_collects_positionals() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("f", false, "");
        fs.int_flag("n", 100, "");
        let args: Vec<String> = ["web", "-f", "-n", "3", "extra"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pos = fs.parse_interspersed(&args, &mut noop).unwrap();
        assert_eq!(pos, vec!["web", "extra"]);
        assert!(fs.get_bool("f"));
        assert_eq!(fs.get_int("n"), 3);
    }

    #[test]
    fn unknown_and_missing_value() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("f", false, "");
        let args: Vec<String> = ["-nope"].iter().map(|s| s.to_string()).collect();
        assert!(fs.parse(&args, &mut noop).unwrap_err().contains("not defined"));
        let mut fs2 = FlagSet::new("devrun x");
        fs2.string_flag("name", "", "");
        let args2: Vec<String> = ["-name"].iter().map(|s| s.to_string()).collect();
        assert!(fs2.parse(&args2, &mut noop).unwrap_err().contains("needs an argument"));
    }

    #[test]
    fn help_is_a_distinguished_error() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("f", false, "a flag");
        let mut printed = String::new();
        let args: Vec<String> = ["-h"].iter().map(|s| s.to_string()).collect();
        let err = fs
            .parse(&args, &mut |s| printed.push_str(s))
            .unwrap_err();
        assert_eq!(err, ERR_HELP);
        assert!(printed.contains("-f"), "{printed}");
    }

    #[test]
    fn double_dash_ends_flags() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("f", false, "");
        let args: Vec<String> = ["--", "-f"].iter().map(|s| s.to_string()).collect();
        fs.parse(&args, &mut noop).unwrap();
        assert_eq!(fs.args, vec!["-f"]);
        assert!(!fs.get_bool("f"));
    }

    #[test]
    fn multi_accumulates() {
        let mut fs = FlagSet::new("devrun x");
        fs.multi_flag("f", "");
        let args: Vec<String> = ["-f", "a.yml", "--f=b.yml"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        fs.parse(&args, &mut noop).unwrap();
        assert_eq!(fs.get_multi("f"), vec!["a.yml", "b.yml"]);
    }

    #[test]
    fn bool_with_explicit_value() {
        let mut fs = FlagSet::new("devrun x");
        fs.bool_flag("all", false, "");
        let args: Vec<String> = ["--all=false"].iter().map(|s| s.to_string()).collect();
        fs.parse(&args, &mut noop).unwrap();
        assert!(!fs.get_bool("all"));
        let mut fs2 = FlagSet::new("devrun x");
        fs2.bool_flag("all", false, "");
        let args2: Vec<String> = ["--all=maybe"].iter().map(|s| s.to_string()).collect();
        assert!(fs2.parse(&args2, &mut noop).is_err());
    }
}
