//! Send one file with [`linkr_tui::zmodem::Sender`] over stdin/stdout.
//!
//! This is the harness half of the sender's reference test, and it exists for
//! the same reason `dev/zmodem-capture.py` does: the receiving side of ZMODEM has
//! no upstream port to inherit a test from, so the thing that decides whether the
//! sender is right is a real `rz` at the far end. `dev/zmodem-send-check.py`
//! wires this program to one and checks the file that comes out the far side.
//!
//! It speaks the link on fd 0 and fd 1, exactly as `sz` does — ZMODEM is in band
//! over one bidirectional channel — so it can be handed a socketpair end as both
//! of its descriptors and dropped into a bridge with no idea it is not talking to
//! a serial line.
//!
//! ```text
//! cargo run --example zmodem-send -- /path/to/file
//! ```
//!
//! Unix only, and not because of the protocol: it reads the file at the offsets
//! the sender asks for, which is `pread(2)`. On any other target this compiles to
//! a stub, so that the cross-target check still builds the example.

#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt};

#[cfg(unix)]
use linkr_tui::zmodem::{FileInfo, Parser, Request, Sender};

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: zmodem-send <file>");
            std::process::exit(2);
        }
    };

    let meta = std::fs::metadata(&path)?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let mut info = FileInfo::new(name.into_bytes(), meta.len());
    info.modified = meta.mtime().max(0) as u64;
    // The mode goes on the wire including the file-type bits, which is what lrzsz
    // sends; `rz` applies only the permission bits.
    info.mode = meta.mode();
    info.remaining = 1;

    // `read_at` takes `&self`, so the handle is shared rather than advanced: the
    // sender asks for offsets, and nothing here tracks a position.
    let file = File::open(&path)?;
    let mut serial = Sender::new([info]);
    let mut parser = Parser::new();
    let mut pending: VecDeque<Request> = serial.start().into();
    let mut stdout = std::io::stdout().lock();
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 8192];

    loop {
        while let Some(request) = pending.pop_front() {
            match request {
                Request::Send(bytes) => stdout.write_all(&bytes)?,
                Request::Read { round, at, len } => {
                    let mut block = vec![0u8; len];
                    read_exactly(&file, &mut block, at)?;
                    pending.extend(serial.supply(round, &block));
                }
                Request::Sent(info) => {
                    eprintln!("sent {} ({} bytes)", info.name(), info.length);
                }
                Request::Skipped(info) => {
                    eprintln!("refused by the far end: {}", info.name());
                }
                Request::Failed(why) => {
                    eprintln!("failed: {why:?}");
                    stdout.flush()?;
                    std::process::exit(1);
                }
                Request::Finished => {}
            }
        }
        stdout.flush()?;
        if serial.is_done() {
            return Ok(());
        }

        let read = stdin.read(&mut buf)?;
        if read == 0 {
            eprintln!("the far end closed the link before the session ended");
            std::process::exit(1);
        }
        parser.feed(&buf[..read]);
        while let Some(event) = parser.next_event() {
            pending.extend(serial.on_event(event));
        }
    }
}

/// Read exactly `len` bytes at `at`, or fail. A short read here would go out as a
/// short block, which the far end would take for the end of the file.
#[cfg(unix)]
fn read_exactly(file: &File, out: &mut [u8], at: u64) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < out.len() {
        let got = file.read_at(&mut out[filled..], at + filled as u64)?;
        if got == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "file ended after {} of {} bytes",
                    at + filled as u64,
                    at + out.len() as u64
                ),
            ));
        }
        filled += got;
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("zmodem-send is a unix harness; dev/zmodem-send-check.py drives it");
    std::process::exit(2);
}
