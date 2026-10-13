fn main() {
    if let Err(e) = publisher_cli::run() {
        eprintln!("error: {e}");
        std::process::exit(e.exit_code());
    }
}
