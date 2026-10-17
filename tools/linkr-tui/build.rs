//! The Windows executable's resource section: the project's mark.
//!
//! Only Windows carries resources, and only a Windows build should pay for it
//! — but "is the target Windows" is a question for the environment Cargo sets
//! and not for `#[cfg(windows)]`: a build script is compiled for the *host*,
//! and the GNU-ABI Windows binaries this project ships are cross-compiled
//! from Linux, where that attribute is false and the icon would go missing
//! without a word.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // The target's operating system, whatever this script was built on.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Relative to this crate's root, which is where a build script runs.
    winresource::WindowsResource::new()
        .set_icon("assets/icon.ico")
        .compile()
        .expect("the Windows icon could not be embedded");
}
