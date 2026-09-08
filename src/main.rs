use clap::Parser;

fn main() {
    let cli = mindspace::cli::Cli::parse();
    if let Err(e) = mindspace::cli::execute(&cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
