fn main() {
    // B3: Rust starts with SIGPIPE ignored, which makes `linkr … | head` panic
    // (exit 134 with `panic = "abort"`). Put the kernel default back before
    // anything is printed; a no-op on Windows.
    linkr_cli::term::restore_sigpipe();
    let code = linkr_cli::cli::run();
    std::process::exit(code);
}
