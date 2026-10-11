//! Cross-platform terminal input: raw mode (POSIX termios via crossterm),
//! line mode, key decoding and the escape-byte exit rule.

use std::io::{Read, Write};

/// Descriptors are `c_int` on every Unix we build for; Windows has none.
#[cfg(unix)]
use std::os::raw::c_int;

/// `true` when a controlling TTY is available for raw mode.
pub fn stdin_is_tty() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

/// `SIG_DFL` / `SIG_IGN`: the two sentinel handlers POSIX defines for `signal`.
#[cfg(unix)]
const SIG_DFL: usize = 0;
/// Only the test hands the ignore-bit back to the harness that owns it.
#[cfg(all(unix, test))]
const SIG_IGN: usize = 1;
/// Signal 13 on every Unix we build for (Linux, macOS, the BSDs).
#[cfg(unix)]
const SIGPIPE: i32 = 13;

#[cfg(unix)]
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

#[cfg(unix)]
fn set_sigpipe(handler: usize) {
    // SAFETY: `signal` takes a signal number and a handler and hands back the
    // previous handler; on every target this module is compiled for
    // (`cfg(unix)`), SIGPIPE is defined as 13 and both sentinel values are 0/1.
    unsafe {
        signal(SIGPIPE, handler);
    }
}

/// Put the kernel's `SIGPIPE` action back. Called once from `main`.
///
/// Rust ignores `SIGPIPE` at start-up, which turns a normal `linkr … | head`
/// into a `println!` panic — and with `panic = "abort"` in the release profile
/// that panic aborts the process with **exit status 134** instead of the pipe
/// simply closing. CPython restores the default too, so the Python CLI never
/// showed this; standard CLI behaviour is the same. Windows has no `SIGPIPE`
/// (a write to a closed pipe comes back as an error the code already handles),
/// so the function is a no-op there.
#[cfg(unix)]
pub fn restore_sigpipe() {
    set_sigpipe(SIG_DFL);
}

/// See the Unix version: nothing to restore on Windows.
#[cfg(not(unix))]
pub fn restore_sigpipe() {}

/// Marker returned by [`read_hidden`] when the user presses Ctrl-C.
pub const INTERRUPTED: &str = "interrupted";

/// Guard that leaves the terminal in raw mode while it is alive. Dropping it
/// (including through unwinding) restores the previous mode, which is what the
/// Python CLI does with its `RawTerminal` context manager.
pub struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    /// Enable raw mode when `enabled` is set; a no-op otherwise.
    pub fn enable(enabled: bool) -> anyhow::Result<Self> {
        if !enabled {
            return Ok(Self { active: false });
        }
        crossterm::terminal::enable_raw_mode().map_err(|_| {
            anyhow::anyhow!("raw terminal mode needs POSIX termios; use --line-mode instead")
        })?;
        Ok(Self { active: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// `dup` that sets close-on-exec, so the pump's descriptors never leak into a
/// child process. Closes everything in `cleanup` when it fails, which keeps
/// each early return in [`InputPump::install`] to one line.
#[cfg(unix)]
fn dup_cloexec(fd: c_int, cleanup: &[c_int]) -> Option<c_int> {
    // SAFETY: `F_DUPFD_CLOEXEC` duplicates `fd` to the lowest free
    // descriptor and sets the close-on-exec bit; both inputs are integers.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup >= 0 {
        return Some(dup);
    }
    close_all(cleanup);
    None
}

/// Close every descriptor handed over, skipping the `-1` sentinels.
#[cfg(unix)]
fn close_all(fds: &[c_int]) {
    for &fd in fds {
        if fd >= 0 {
            // SAFETY: each fd here was opened by this module and is not owned
            // by a live thread.
            unsafe { libc::close(fd) };
        }
    }
}

/// The feeder behind [`InputPump`]: it owns the real stdin and both pty ends
/// and moves bytes from one to the other.
///
/// # Invariant
///
/// A write only ever happens while the pty queue is **empty**, and is at most
/// [`MAX_SLICE`] bytes. Together that guarantees the single `read` crossterm
/// performs per wake-up drains the queue: it reads at most `TTY_BUFFER_SIZE`
/// bytes and returns as soon as its parser yields one event
/// (`crossterm-0.29.0/src/event/source/unix/mio.rs`), so whatever it leaves
/// behind sits in the tty queue with **no new edge** to wake mio's
/// edge-triggered epoll (`mio-1.2.3/src/sys/unix/selector/epoll.rs`) — the
/// paste stalls (G3) until the next keypress happens to signal one.
#[cfg(unix)]
mod input_pump {
    use super::{c_int, close_all};
    use std::time::{Duration, Instant};

    /// Never write more than one crossterm read can swallow: that is
    /// `TTY_BUFFER_SIZE` in `event/source/unix/mio.rs`. Bigger writes re-arm
    /// G3; smaller ones only cost throughput we do not need here.
    pub(super) const MAX_SLICE: usize = 1024;

    /// How much of the real terminal to take per read — a fast paste arrives
    /// in 4 KiB slabs, and this keeps the slice loop responsive.
    const READ_BUF: usize = 4096;

    /// Let the line discipline flush before trusting an empty queue: a master
    /// write reaches it from a work queue, so `FIONREAD` reads 0 for a short
    /// while — *not yet arrived* looks exactly like *already drained*.
    /// Measured 33 us on this kernel (`/tmp/pty_probe.c`), so this is ~60x
    /// that. Only a flush later than this lets a second slice in before the
    /// first drained, which costs G3 again; it never costs corruption.
    const SETTLE: Duration = Duration::from_millis(2);

    /// Re-check this often while waiting for crossterm to read.
    const DRAIN_POLL: Duration = Duration::from_millis(1);

    /// How long to wait for the consumer before giving up on feeding.
    ///
    /// This used to be 2 s, and *that was the bug*: crossterm can hold a full
    /// 1024-event read while the TUI works through it (400 keys/s is normal —
    /// every key triggers a render), so a drain legitimately takes 2.5 s. On
    /// expiry the old code wrote anyway, the pty queue climbed to its 4096
    /// limit, crossterm read one 1024-byte slice and stranded the rest with
    /// no edge to report it: TX froze at exactly 4096 forever. So the wait is
    /// now longer than any consumer stall, and expiry **stops feeding**
    /// instead of writing blind — by then the TUI is gone, and refusing input
    /// is strictly better than a queue the reader will never drain.
    const DRAIN_BUDGET: Duration = Duration::from_secs(60);

    /// End offset (exclusive) of the slice starting at `offset`.
    pub(super) fn slice_end(total: usize, offset: usize) -> usize {
        total.min(offset + MAX_SLICE)
    }

    /// Bytes the pty still holds unread, or `None` when the tty refuses the
    /// ioctl — then there is no gate to apply and the feeder degrades to what
    /// it did before (write whenever, strand what the reader misses).
    pub(super) fn queue_len(slave: c_int) -> Option<usize> {
        let mut pending: c_int = 0;
        // SAFETY: `FIONREAD` writes one `c_int` through the pointer, and
        // `slave` is a descriptor this module opened for the pty slave.
        let rc = unsafe { libc::ioctl(slave, libc::FIONREAD, &mut pending as *mut c_int) };
        if rc < 0 || pending < 0 {
            None
        } else {
            Some(pending as usize)
        }
    }

    /// Wait until the pty queue is empty again, so the next write can never
    /// put a second slice in front of one crossterm has not read yet — that
    /// is the condition under which its one 1024-byte read strands the rest
    /// with no edge to report (G3).
    ///
    /// Two steps, because they are different facts:
    ///
    /// 1. [`SETTLE`] first — right after our own write an empty queue only
    ///    means the flush has not landed, not that the reader took the bytes;
    /// 2. then wait, **without a budget that can expire mid-consumer**, until
    ///    it is empty: a busy TUI legitimately takes seconds to come back.
    ///
    /// `false` means the consumer stopped coming back for good: the caller
    /// must stop feeding rather than write blind.
    pub(super) fn wait_until_drained(slave: c_int) -> bool {
        std::thread::sleep(SETTLE);
        if !holds_bytes(slave) {
            return true;
        }
        let deadline = Instant::now() + DRAIN_BUDGET;
        while Instant::now() < deadline {
            if !holds_bytes(slave) {
                return true;
            }
            std::thread::sleep(DRAIN_POLL);
        }
        false
    }

    /// `true` while the queue still holds readable bytes. `None` (ioctl
    /// refused) counts as empty: without a gate there is nothing to wait for.
    fn holds_bytes(slave: c_int) -> bool {
        matches!(queue_len(slave), Some(n) if n > 0)
    }

    /// `true` once unread bytes show up, `false` when the grace runs out
    /// first. Test-only: it waits for the kernel's flush, which the feeder
    /// covers with [`SETTLE`] instead.
    #[cfg(test)]
    pub(super) fn wait_for_arrival(slave: c_int) -> bool {
        let deadline = Instant::now() + Duration::from_millis(50);
        loop {
            if holds_bytes(slave) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    /// Move every byte `real` hands us into the pty, [`MAX_SLICE`] at a time.
    /// Returns when the terminal goes away or the pty stops taking writes.
    pub(super) fn feed(real: c_int, master: c_int, slave: c_int) {
        let mut buf = [0u8; READ_BUF];
        loop {
            // SAFETY: `real` is this thread's own descriptor and `buf` is a
            // live 4096-byte array.
            let got = unsafe { libc::read(real, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if got == 0 {
                return; // EOF: the terminal the user typed on is gone
            }
            if got < 0 {
                let error = std::io::Error::last_os_error();
                match error.kind() {
                    // A non-blocking stdin reporting "nothing yet" is not EOF.
                    std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(DRAIN_POLL);
                        continue;
                    }
                    // POSIX: a caught signal can interrupt `read` before any
                    // byte arrives, and retrying is the correct handling —
                    // treating it like EOF would stop the pump for the rest of
                    // the session. This is hardening, not a fix we can
                    // demonstrate: every handler in this process is registered
                    // through `signal-hook-registry`, which sets `SA_RESTART`,
                    // so the kernel restarts the read and EINTR does not
                    // surface with today's dependencies.
                    std::io::ErrorKind::Interrupted => continue,
                    _ => return,
                }
            }
            let total = got as usize;
            let mut offset = 0;
            while offset < total {
                let end = slice_end(total, offset);
                // SAFETY: `master` is this thread's own descriptor and the
                // slice lies inside `buf`.
                let wrote = unsafe {
                    libc::write(
                        master,
                        buf[offset..end].as_ptr() as *const libc::c_void,
                        end - offset,
                    )
                };
                if wrote <= 0 {
                    return;
                }
                offset += wrote as usize;
                // *After* the write, never before it: a wait in front of the
                // first slice would burn [`SETTLE`] waiting for bytes nobody
                // has sent yet, and only this write's own drain is a fact this
                // thread can vouch for. Stop feeding if the reader never
                // comes back — writing blind is what froze TX at 4096.
                if !wait_until_drained(slave) {
                    return;
                }
            }
        }
    }

    /// A pty pair with both ends ready to move bytes.
    ///
    /// The **slave** must go raw before anything writes to the master: a new
    /// pty line discipline echoes every byte back and holds input until a
    /// newline, so crossterm would see neither the bytes nor their echo.
    pub(super) fn open_pty() -> Option<(c_int, c_int)> {
        // SAFETY: `posix_openpt` takes open flags; `grantpt` and `unlockpt`
        // take the master it just returned.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if master < 0 {
            return None;
        }
        if unsafe { libc::grantpt(master) } != 0 || unsafe { libc::unlockpt(master) } != 0 {
            unsafe { libc::close(master) };
            return None;
        }
        // SAFETY: `ptsname` hands back a NUL-terminated name in a buffer libc
        // owns. It is a static buffer, so copy it right away — this is the
        // only call site in the process, but nothing else may race it later.
        let name = unsafe { libc::ptsname(master) };
        let path = match name.is_null() {
            true => None,
            false => Some(unsafe { std::ffi::CStr::from_ptr(name) }.to_owned()),
        };
        let Some(path) = path else {
            unsafe { libc::close(master) };
            return None;
        };
        // SAFETY: `path` is a NUL-terminated copy of the slave's name.
        let slave = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if slave < 0 {
            unsafe { libc::close(master) };
            return None;
        }
        if rawify(slave).is_none() {
            close_all(&[master, slave]);
            return None;
        }
        Some((master, slave))
    }

    /// Put a terminal in raw mode and return what was there before.
    pub(super) fn rawify(fd: c_int) -> Option<libc::termios> {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is a tty (the caller checked) and `original` is the
        // right size for `tcgetattr` to fill in.
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return None;
        }
        let mut raw = original;
        // SAFETY: `raw` holds the termios `tcgetattr` just filled in.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: same terminal, only the mode bits change.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return None;
        }
        Some(original)
    }
}

/// A pty pump in front of `fd0`, alive for as long as the TUI runs.
///
/// # Why
///
/// crossterm's event source takes at most one 1024-byte read per wake-up and
/// returns the moment its parser yields an event, so a paste longer than that
/// left the tail in the tty queue **with no edge-triggered epoll event left to
/// signal it**: the tail only moved when the user pressed another key (G3).
/// The feeder thread guarantees every read drains the queue, which is exactly
/// the condition under which edge-triggered epoll is reliable.
///
/// # What it changes, and what it deliberately does not
///
/// * `fd0` becomes a pty **slave**, so `isatty(0)` stays true and crossterm
///   keeps reading `fd0` through its own `tty_fd()`;
/// * `size()` opens `/dev/tty` — the real console — so the window size needs
///   no mirroring and there is no resize race;
/// * crossterm's raw-mode guard touches the slave only; this guard puts the
///   **real** stdin in raw mode and puts it back on drop;
/// * the TUI is the only reader of stdin (`read_stdin_line_or_chunk` and
///   `read_hidden` are CLI-only, and `main` exits right after the TUI), so the
///   feeder never competes for bytes.
///
/// Windows consoles read with level semantics and have no such stall, so
/// [`InputPump::install`] is a no-op there.
#[cfg(unix)]
pub struct InputPump {
    /// Our dup of the terminal the user types on: once `fd0` points at the
    /// pty it is the only handle that can put that terminal's mode back.
    restore_fd: c_int,
    /// What `tcgetattr` returned before the real stdin went raw.
    original: libc::termios,
}

#[cfg(unix)]
impl InputPump {
    /// Install the pump; `None` when there is nothing to pump — stdin is not a
    /// TTY (a pipe, a CI runner), the kernel refused a pty, or the thread
    /// could not be spawned. The caller then behaves exactly as before.
    pub fn install() -> Option<Self> {
        // SAFETY: `isatty` only inspects descriptor 0.
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return None;
        }

        // Both handles have to exist before `fd0` becomes the pty slave:
        // afterwards `fd0` no longer refers to the terminal the user types on.
        let real = dup_cloexec(libc::STDIN_FILENO, &[])?;
        let restore_fd = dup_cloexec(libc::STDIN_FILENO, &[real])?;
        let Some((master, slave)) = input_pump::open_pty() else {
            close_all(&[real, restore_fd]);
            return None;
        };

        // `fd0` becomes the slave. The feeder keeps `slave` for `FIONREAD` and
        // `real` for reading; from the TUI's point of view nothing changed —
        // `fd0` is still a tty, just a different one.
        if unsafe { libc::dup2(slave, libc::STDIN_FILENO) } < 0 {
            close_all(&[real, restore_fd, master, slave]);
            return None;
        }

        // Raw on both ends: the real terminal must stop line-editing and
        // echoing what the feeder reads, and crossterm only ever sees the
        // slave (it resolves `fd0` through `tty_fd()`).
        let Some(original) = input_pump::rawify(restore_fd) else {
            unsafe { libc::dup2(restore_fd, libc::STDIN_FILENO) }; // undo the swap
            close_all(&[real, restore_fd, master, slave]);
            return None;
        };

        // `c_int` is `Copy`, so the closure takes copies and the failure path
        // below can still close the originals.
        let feeder = std::thread::Builder::new()
            .name("linkr-stdin-pump".into())
            .spawn(move || input_pump::feed(real, master, slave));
        if feeder.is_err() {
            unsafe {
                libc::dup2(restore_fd, libc::STDIN_FILENO); // undo the swap
                libc::tcsetattr(restore_fd, libc::TCSANOW, &original);
            }
            close_all(&[restore_fd, master, slave]);
            return None;
        }
        // Detached on purpose: joining could block for the whole drain budget
        // while the process is already on its way out.

        Some(Self {
            restore_fd,
            original,
        })
    }
}

#[cfg(unix)]
impl Drop for InputPump {
    fn drop(&mut self) {
        // SAFETY: `restore_fd` is our own dup of the terminal the user typed
        // on and `original` is what `tcgetattr` returned for that same
        // terminal. The feeder is left to notice the terminal going quiet on
        // its own; nothing else in the process reads stdin any more.
        unsafe {
            libc::tcsetattr(self.restore_fd, libc::TCSANOW, &self.original);
            libc::close(self.restore_fd);
        }
    }
}

/// See the Unix version: a Windows console read has level semantics, so there
/// is no edge-triggered stall to pump around.
#[cfg(not(unix))]
pub struct InputPump;

#[cfg(not(unix))]
impl InputPump {
    /// Nothing to install on Windows.
    pub fn install() -> Option<Self> {
        None
    }
}

/// Windows console bits behind [`QuickEditGuard`].
#[cfg(windows)]
mod quick_edit {
    use winapi::shared::minwindef::DWORD;
    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
    use winapi::um::processenv::GetStdHandle;
    use winapi::um::winbase::STD_INPUT_HANDLE;
    use winapi::um::wincon::{ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE};

    /// Take `ENABLE_QUICK_EDIT_MODE` off the console input handle; returns the
    /// mode to put back, or `None` when there was nothing to change (not a
    /// console, the bit was already clear, or the runner refused).
    pub(super) fn disable() -> Option<DWORD> {
        // SAFETY: `GetStdHandle` returns a process-wide standard handle and
        // both calls only read or write that handle's console mode.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode: DWORD = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return None;
            }
            if mode & ENABLE_QUICK_EDIT_MODE == 0 {
                return None;
            }
            // conhost only honours the quick-edit bit when `ENABLE_EXTENDED_FLAGS`
            // travels with it.
            let next = (mode & !ENABLE_QUICK_EDIT_MODE) | ENABLE_EXTENDED_FLAGS;
            if SetConsoleMode(handle, next) == 0 {
                return None;
            }
            Some(mode)
        }
    }

    /// Put back exactly the mode we found.
    pub(super) fn restore(mode: DWORD) {
        unsafe {
            let _ = SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode);
        }
    }
}

/// Keeps a mouse click from dropping the console into Quick Edit (`选择`)
/// mode while the TUI owns the screen. Dropping the guard restores the mode it
/// found, so copying out of the console works again once the TUI exits.
///
/// While a selection is active conhost **stops painting its window and
/// swallows every keystroke**: the TUI looks frozen, Ctrl+L never even reaches
/// the app, and whatever conhost leaves on screen when the selection ends no
/// longer matches ratatui's back buffer — its cell diff then skips those cells
/// for the rest of the session (F2). crossterm's raw mode is no help: it only
/// clears `LINE | ECHO | PROCESSED_INPUT` and never touches the quick-edit
/// bit. Nothing to do outside Windows, and nothing to do when the handle is
/// not a console (a CI runner, a redirected stdin).
pub struct QuickEditGuard {
    /// The console input mode to restore; `None` when we changed nothing.
    #[cfg(windows)]
    previous: Option<u32>,
    /// Never constructed outside Windows; keeps the struct non-empty.
    #[cfg(not(windows))]
    _private: (),
}

impl QuickEditGuard {
    /// Clear quick edit on the console input handle.
    #[cfg(windows)]
    pub fn disable() -> Self {
        Self {
            previous: quick_edit::disable(),
        }
    }

    /// No-op: Quick Edit is a Windows console concept.
    #[cfg(not(windows))]
    pub fn disable() -> Self {
        Self { _private: () }
    }
}

impl Drop for QuickEditGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mode) = self.previous.take() {
            quick_edit::restore(mode);
        }
    }
}

/// Runs `body` with raw mode enabled on TTYs (no-op in line mode / no TTY).
pub fn with_raw_mode<R>(raw: bool, body: impl FnOnce() -> R) -> anyhow::Result<R> {
    let _guard = RawModeGuard::enable(raw)?;
    Ok(body())
}

/// Blocking stdin reader used by the CLI loop: raw chunks in raw mode, whole
/// lines in line mode. Returns `None` on EOF.
pub fn read_stdin_line_or_chunk(line_mode: bool) -> Option<Vec<u8>> {
    let mut stdin = std::io::stdin().lock();
    if line_mode {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        if buf.is_empty() {
            None
        } else {
            Some(buf)
        }
    } else {
        let mut buf = [0u8; 1024];
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(buf[..n].to_vec()),
        }
    }
}

/// Terminal size of stdin, if available.
pub fn terminal_size() -> Option<(u16, u16)> {
    crossterm::terminal::size().ok()
}

/// Prompt for a secret without echoing it back (the `getpass.getpass`
/// replacement used for `--wifi` on a TTY). Reads from stdin in raw mode, so
/// the keystrokes never reach the screen.
pub fn read_hidden(prompt: &str) -> anyhow::Result<String> {
    if !stdin_is_tty() {
        return Err(anyhow::anyhow!(INTERACTIVE_UNAVAILABLE));
    }
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "{prompt}");
    let _ = stderr.flush();
    let result = (|| -> anyhow::Result<String> {
        let _guard = RawModeGuard::enable(true)?;
        let mut stdin = std::io::stdin().lock();
        let mut out = std::io::stdout();
        let mut secret = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => match byte[0] {
                    b'\r' | b'\n' => break,
                    0x03 => return Err(anyhow::anyhow!(INTERRUPTED)),
                    0x08 | 0x7f => {
                        if secret.pop().is_some() {
                            // Rub out the echoed-free backspace: no echo, so
                            // nothing to erase on screen; keep the buffer true.
                        }
                    }
                    _ => secret.push(byte[0]),
                },
            }
        }
        let _ = out.flush();
        Ok(String::from_utf8_lossy(&secret).into_owned())
    })();
    let _ = writeln!(std::io::stderr());
    result
}

/// Message used when a hidden prompt is impossible (mirrors the Python CLI,
/// which only prompts when stdin is a TTY).
pub const INTERACTIVE_UNAVAILABLE: &str = "no interactive terminal for a password prompt";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_tty_matches_std() {
        use std::io::IsTerminal;
        assert_eq!(stdin_is_tty(), std::io::stdin().is_terminal());
    }

    /// B3: `SIGPIPE` must stop being ignored at start-up, or `linkr … | head`
    /// panics in `println!` — and `panic = "abort"` turns that panic into exit
    /// status 134 instead of the pipe simply closing.
    #[cfg(target_os = "linux")]
    #[test]
    fn restore_sigpipe_clears_the_ignored_bit() {
        restore_sigpipe();
        let status = std::fs::read_to_string("/proc/self/status").expect("no /proc mounted");
        let mask = status
            .lines()
            .find(|line| line.starts_with("SigIgn:"))
            .and_then(|line| line.split(':').nth(1))
            .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())
            .expect("no SigIgn line");
        assert_eq!(
            mask & (1 << (SIGPIPE - 1)),
            0,
            "SIGPIPE is still ignored ({mask:#x}): a closed pipe would abort"
        );
        // Hand the ignore-bit back: the test harness owns the disposition.
        set_sigpipe(SIG_IGN);
    }

    #[test]
    fn raw_guard_is_a_noop_when_disabled() {
        let guard = RawModeGuard::enable(false).unwrap();
        assert!(!guard.active);
        drop(guard);
    }

    #[test]
    fn with_raw_mode_runs_body() {
        let value = with_raw_mode(false, || 7).unwrap();
        assert_eq!(value, 7);
    }

    /// G3's slicing rule: one crossterm read swallows at most `TTY_BUFFER_SIZE`
    /// (1024) bytes, so a slice may never be larger — and the walk over a big
    /// paste has to end exactly on the last byte.
    #[cfg(unix)]
    #[test]
    fn slices_fit_inside_one_crossterm_read() {
        assert_eq!(input_pump::slice_end(1621, 0), 1024);
        assert_eq!(input_pump::slice_end(1621, 1024), 1621);
        assert_eq!(input_pump::slice_end(100, 0), 100);

        let total = 10_000;
        let mut offset = 0;
        let mut slices = 0;
        while offset < total {
            let end = input_pump::slice_end(total, offset);
            assert!(end > offset, "a slice would write nothing");
            assert!(
                end - offset <= input_pump::MAX_SLICE,
                "slice of {} bytes exceeds one read",
                end - offset
            );
            offset = end;
            slices += 1;
        }
        assert_eq!(offset, total, "the last slice fell short");
        assert_eq!(slices, 10);
    }

    /// The whole trick rests on `FIONREAD` counting bytes the master wrote
    /// **before** any newline: in canonical mode a partial line reads as 0 and
    /// the feeder would never open the gate. This pins both halves — the count
    /// and "the slave really is raw".
    #[cfg(unix)]
    #[test]
    fn pty_queue_counts_unread_bytes_without_a_newline() {
        let Some((master, slave)) = input_pump::open_pty() else {
            return; // no /dev/ptmx in this sandbox: nothing to prove
        };
        assert_eq!(input_pump::queue_len(slave), Some(0));
        let payload = b"abc";
        // SAFETY: both descriptors came from `open_pty`; the buffer outlives
        // the call.
        let wrote = unsafe {
            libc::write(
                master,
                payload.as_ptr() as *const libc::c_void,
                payload.len(),
            )
        };
        assert_eq!(wrote, 3);
        // The line discipline only sees the bytes once the kernel flushes the
        // flip buffer — the gate has to wait that out before the count means
        // anything.
        assert!(
            input_pump::wait_for_arrival(slave),
            "the write never reached the slave"
        );
        assert_eq!(
            input_pump::queue_len(slave),
            Some(3),
            "the gate would stay shut forever"
        );
        let mut buf = [0u8; 3];
        // SAFETY: `slave` is ours and `buf` outlives the read.
        let got = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert_eq!(got, 3);
        assert_eq!(&buf, payload, "the pty changed the bytes");
        assert_eq!(input_pump::queue_len(slave), Some(0));
        close_all(&[master, slave]);
    }

    /// A 3000-byte paste has to arrive complete, in order, in slices no larger
    /// than one crossterm read — otherwise G3 comes straight back.
    #[cfg(unix)]
    #[test]
    fn feeder_hands_over_a_large_paste_in_order() {
        let mut pipe_fds: [c_int; 2] = [-1; 2];
        // SAFETY: `pipe` fills in two fresh descriptors.
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            return;
        }
        let Some((master, slave)) = input_pump::open_pty() else {
            close_all(&pipe_fds);
            return;
        };

        const TOTAL: usize = 3000;
        let payload: Vec<u8> = (0..TOTAL).map(|i| (i % 251) as u8).collect();
        // SAFETY: the write end is ours and `payload` outlives the call.
        let wrote =
            unsafe { libc::write(pipe_fds[1], payload.as_ptr() as *const libc::c_void, TOTAL) };
        assert_eq!(wrote as usize, TOTAL);

        // The thread owns the read end; `c_int` is `Copy`, so it took copies.
        let feeder = std::thread::spawn(move || input_pump::feed(pipe_fds[0], master, slave));

        let mut got = Vec::with_capacity(TOTAL);
        while got.len() < TOTAL {
            let mut fds = libc::pollfd {
                fd: slave,
                events: libc::POLLIN,
                revents: 0,
            };
            // Bounded so a wedged feeder fails the test instead of hanging it.
            // SAFETY: exactly one `pollfd`, valid for the duration of the call.
            if unsafe { libc::poll(&mut fds, 1, 2_000) } <= 0 {
                break;
            }
            // Deliberately larger than one slice: a read that comes back
            // bigger than `MAX_SLICE` means the feeder overfilled the queue.
            let mut buf = [0u8; 4096];
            // SAFETY: `slave` is ours and `buf` is a live array.
            let n = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            assert!(
                n as usize <= input_pump::MAX_SLICE,
                "the feeder wrote {n} bytes at once, past one crossterm read"
            );
            got.extend_from_slice(&buf[..n as usize]);
        }

        assert_eq!(got.len(), TOTAL, "the feeder lost bytes");
        assert_eq!(got, payload, "the feeder reordered bytes");

        // Closing the write end ends `feed`; the read end belongs to the
        // thread, which exits on its own — never close it from here.
        unsafe { libc::close(pipe_fds[1]) };
        let _ = feeder.join();
        close_all(&[master, slave]);
    }
}
