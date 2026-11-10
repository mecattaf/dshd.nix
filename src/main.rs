//! `dshd`: the lifecycle CLI. Parses argv, runs one verb and maps the result
//! to the documented exit code. This is the only place the CLI calls
//! `process::exit`.

use std::process;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    process::exit(i32::from(dshd::cli::main(args)));
}
