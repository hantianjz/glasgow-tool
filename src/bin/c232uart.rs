#[path = "common/c232uart.rs"]
mod c232uart;
mod common;

use clap::Parser;

fn main() {
    let cli = c232uart::C232Cli::parse();
    if let Err(error) = cli.validate().and_then(|()| c232uart::run(cli)) {
        eprintln!("error: {error}");
        std::process::exit(common::exit_code(&error));
    }
}
