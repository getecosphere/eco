fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match eco::cli::run_cli(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
