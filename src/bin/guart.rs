use clap::Parser;
use glasgow_tool::cli::GuartCli;
use glasgow_tool::glasgow;

fn main() {
    let cli = GuartCli::parse();
    if let Err(error) = glasgow::run(cli) {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code().as_i32());
    }
}
