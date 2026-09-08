//! Console-subsystem entry point: pipes work without launching a desktop window.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == sightocr::worker::SUBPROCESS_ARG {
        if sightocr::worker::run_subprocess(std::path::PathBuf::from(&args[1])).is_err() {
            std::process::exit(1);
        }
        return;
    }
    std::process::exit(sightocr::cli::run(args));
}
