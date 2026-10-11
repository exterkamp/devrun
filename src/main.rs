mod alloc;
mod cli;
mod cmds;
mod docker;
mod flags;
mod gitctx;
mod herdr;
mod hygiene;
mod procmod;
mod registry;
mod render;
mod resolve;
mod rtime;
mod runner;
mod view;

use std::io::Write;

fn main() {
    eprintln!("[DEBUG] entering");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = cli::main_run(&args);
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
