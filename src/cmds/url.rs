//! `devrun url`: print a server's URL.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdError, CmdResult, Env, STATE_DEAD};
use std::io::Write;

pub fn cmd_url(env: &mut Env, args: &[String]) -> CmdResult {
    let mut fs = new_flag_set(env, "url");
    fs.bool_flag("json", false, "machine-readable output");
    let pos = {
        let mut hp = help_printer(env);
        fs.parse_interspersed(args, &mut hp).map_err(flag_err)?
    };
    let as_json = fs.get_bool("json");
    let name = pos.first().cloned().unwrap_or_default();
    let f = env.store.load()?;
    let e = env.resolve_entry(&f, "url", &name)?;
    let listening = (env.listening)()?;
    let v = env.view(&e, &listening);
    if v.state == STATE_DEAD {
        return Err(CmdError::msg(format!("server {:?} is not running", e.name)));
    }
    if as_json {
        let obj = serde_json::json!({
            "name": v.name,
            "url": v.url,
            "port": v.port,
            "ports": v.ports,
            "state": v.state,
        });
        writeln!(env.stdout, "{}", serde_json::to_string(&obj).map_err(|e| e.to_string())?)?;
        return Ok(());
    }
    writeln!(env.stdout, "{}", v.url)?;
    Ok(())
}
