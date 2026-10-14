//! Take whatever `sz` offers, with [`linkr_tui::transfer::zmodem`].
//!
//! The mirror of [`zmodem-send`](../zmodem-send/index.html), and here for the
//! same reason: there is no upstream receiver to inherit a test from, so the
//! thing that decides whether ours is right is a real `sz` at the far end.
//! `dev/zmodem-recv-check.py` wires this program to one and compares the file
//! that lands on disk with the one that went in.
//!
//! Unlike the send example this drives the whole `Engine`, not the bare codec:
//! a receive is not finished until the bytes are on disk under the name the far
//! end chose, and that is exactly the half a codec test cannot see — the path
//! handling, the file it opens, the partial it leaves behind.
//!
//! ```text
//! cargo run --example zmodem-recv -- /tmp/incoming
//! ```
//!
//! It speaks ZMODEM on fd 0 and fd 1, so either descriptor can be an end of a
//! socketpair and this program will not know the difference. Unix only, because
//! `dev/zmodem-recv-check.py` is; on any other target it compiles to a stub so
//! that the cross-target check still builds the example.

#[cfg(unix)]
use std::io::{Read, Write};

#[cfg(unix)]
use linkr_tui::transfer::zmodem::{Event, LiveTransfer};

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    let dir = match std::env::args().nth(1) {
        Some(dir) => dir,
        None => {
            eprintln!("usage: zmodem-recv <directory>");
            std::process::exit(2);
        }
    };
    std::fs::create_dir_all(&dir)?;

    let mut live = LiveTransfer::receive(std::path::PathBuf::from(&dir));
    let mut stdout = std::io::stdout().lock();
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 8192];

    // The receiver speaks first: it announces itself and waits to be offered
    // something. A real `rz` does the same, which is why this can be dropped
    // straight into a `sz`'s line.
    let opening = live.opening_bytes();
    stdout.write_all(&opening)?;
    stdout.flush()?;

    loop {
        for event in live.take_events() {
            report(&event);
        }
        stdout.flush()?;
        if live.is_done() {
            return Ok(());
        }

        let read = stdin.read(&mut buf)?;
        if read == 0 {
            eprintln!("the far end closed the link before the session ended");
            // What arrived is still on disk; leaving it is the honest outcome,
            // because a half-written file *looks* like a complete one and only
            // the operator can tell. The harness checks for this instead.
            return Ok(());
        }
        let answers = live.feed(&buf[..read]);
        if !answers.is_empty() {
            stdout.write_all(&answers)?;
        }
    }
}

/// Say what happened on stderr, so it can never be mistaken for protocol bytes:
/// fd 1 *is* the wire.
#[cfg(unix)]
fn report(event: &Event) {
    match event {
        Event::Started { name, total, .. } => {
            let size = match total {
                Some(total) => format!("{total} bytes"),
                None => "unknown size".to_string(),
            };
            eprintln!("receiving {name} ({size})");
        }
        Event::Saved { name, bytes } => eprintln!("saved {name} ({bytes} bytes)"),
        Event::Failed { name, reason } => {
            if name.is_empty() {
                eprintln!("failed: {reason}");
            } else {
                eprintln!("failed: {name} · {reason}");
            }
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("zmodem-recv is a unix harness; dev/zmodem-recv-check.py drives it");
    std::process::exit(2);
}
