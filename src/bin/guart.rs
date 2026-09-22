mod common;
#[path = "common/guart.rs"]
mod guart;

use clap::Parser;

fn main() {
    let cli = guart::GuartCli::parse();
    if let Err(error) = guart::run(cli) {
        eprintln!("error: {error}");
        std::process::exit(common::exit_code(&error));
    }
}
