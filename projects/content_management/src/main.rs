fn main() {
    if let Err(e) = editor_server::serve() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
