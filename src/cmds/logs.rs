//! `devrun logs`: show the server log.

use crate::cli::{flag_err, help_printer, new_flag_set, CmdResult, Env};
use crate::runner::{self, LogOpts};

pub fn cmd_logs(env: &mut Env, args: &[String]) -> CmdResult {
    eprintln!("DEBUG: made it here");
    let mut fs = new_flag_set(env, "logs");
    fs.bool_flag("f", false, "follow the log");
    fs.int_flag("n", 100, "number of trailing lines to show (0 for all)");
    let pos = {
        let mut hp = help_printer(env);
        fs.parse_interspersed(args, &mut hp).map_err(flag_err)?
    };
    let follow = fs.get_bool("f");
    let lines = fs.get_int("n");
    let name = pos.first().cloned().unwrap_or_default();
    let f = env.store.load()?;
    let e = env.resolve_entry(&f, "logs", &name)?;
    let rn = runner::get(&e.kind)?;
    crate::cli::install_signal_handlers();
    let stop = || crate::cli::interrupted();
    rn.logs(&e, &LogOpts { follow, lines }, &mut env.stdout, &stop)?;
    Ok(())
}
