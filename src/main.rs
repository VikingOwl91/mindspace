fn main() {
    let cli = mindspace::cli::parse();
    match mindspace::cli::run(&cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            mindspace::cli::render::error(&e);
            std::process::exit(1);
        }
    }
}
