//! `tohdr` command line: produce and inspect HDR gain-map HEICs.
//!
//! Subcommands dispatch to modules that each own one concern:
//! [`convert`] encodes, [`inspect`] and [`verify`] read back, [`bench`]
//! compares engines. [`cli`] is parsing only, so `--help` works even while
//! every engine crate underneath is still `todo!()`.

mod bench;
mod cli;
mod convert;
mod engine;
mod inspect;
mod panic_guard;
mod verify;

use clap::Parser;

use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Convert(args) => convert::run(args),
        Command::Inspect(args) => inspect::run(args),
        Command::Verify(args) => verify::run(args),
        Command::Bench(args) => bench::run(args),
    };

    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("tohdr: error: {e:#}");
            std::process::exit(1);
        }
    }
}
