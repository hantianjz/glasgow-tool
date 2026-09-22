use clap::Parser;
use glasgow_tool::c232;
use glasgow_tool::cli::C232Cli;

fn main() {
    let cli = C232Cli::parse();
    if let Err(error) = cli.validate().and_then(|()| c232::run(cli)) {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code().as_i32());
    }
}
