fn main() {
    if let Err(error) = h12tiny_app::run() {
        eprintln!("h12tiny-twelve failed: {error}");
        std::process::exit(1);
    }
}
