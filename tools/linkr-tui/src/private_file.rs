//! Files that hold a secret: one place that creates one owner-only, and one
//! that tightens a file already on disk.
//!
//! Three stores keep a secret in a file — the agent store's API key, the LAN
//! token store, and a `--log-file` trace — and each grew its own copy of the
//! `chmod` with its own idea of when to run it. Trusting the umask leaves the
//! secret group-readable; running `chmod` after the write leaves it readable
//! for as long as the write takes. Asking for the mode at creation is the
//! whole of the fix, and now it is one fix.

use std::io::Write;
use std::path::Path;

/// Open `path` for appending, created owner-only where the platform has
/// permissions and it does not exist yet.
///
/// The trace log appends on purpose — a log that lost yesterday's session when
/// today's started is a log nobody can compare against — so this is the door
/// for a file that must keep what it already has.
pub fn open_private_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Write `bytes` to `path`, replacing what is there, with the file created
/// owner-only.
///
/// The mode is asked for at creation rather than set afterwards: a file that
/// was born readable and tightened a line later was readable for that line —
/// and the line is one crash away from being forever.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Tighten `path` to owner-only, where the platform has permissions.
///
/// For a file that already exists: one written before the mode was asked for
/// at creation, or carried in from somewhere whose umask was generous.
#[cfg(unix)]
pub fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Nothing to do where the file's ACL is the user's own business.
#[cfg(not(unix))]
pub fn restrict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Replace `path` with `bytes` in one atomic rename, both files owner-only.
///
/// The temp file is written beside the real one and renamed over it, so a
/// reader never sees a half-written store: a partial JSON object read back as
/// an empty map is a bridge that will not authenticate, or a task list that
/// has lost its history. The parent directory is created first, because the
/// stores that use this are the first thing a fresh machine writes.
///
/// The temp name carries the writing process's id, because two `bee`
/// instances on one bench are normal and a fixed `.tmp` name is a file they
/// would take turns truncating — one instance's rename would then move the
/// other's half-written copy into place. A name that is somehow taken is
/// retried under the next one, and the temp is removed rather than left behind
/// holding a secret.
pub fn write_private_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut tmp = path.to_path_buf();
    let mut stamp = std::process::id();
    loop {
        tmp.set_extension(format!("tmp{stamp:08x}"));
        if write_private(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok() {
            // Again after the rename, not before: a file that was already
            // there keeps whatever mode it had until this moment, and this one
            // holds a secret.
            let _ = restrict(path);
            return Ok(());
        }
        let _ = std::fs::remove_file(&tmp);
        // A collision on the temp name is the only recoverable failure; a full
        // disk or a read-only directory is reported rather than spun on.
        if stamp > u32::MAX - 1024 {
            return Err(std::io::Error::other(format!(
                "could not write {}: every temporary name was taken or the \
                 directory refused the write",
                path.display()
            )));
        }
        stamp += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mode is the mechanism: a file made for a secret is made
    /// owner-only, an appended-to one is made the same way, and a file that
    /// predates the rule is tightened when the program notices it.
    #[cfg(unix)]
    #[test]
    fn a_secret_file_is_owner_only_from_the_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of =
            |path: &Path| std::fs::metadata(path).expect("stat").permissions().mode() & 0o777;
        let dir = std::env::temp_dir().join(format!("linkr-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");

        let written = dir.join("secret");
        write_private(&written, b"hunter2").expect("written");
        assert_eq!(mode_of(&written), 0o600, "the umask must not open it");
        assert_eq!(
            std::fs::read(&written).expect("read"),
            b"hunter2",
            "and the bytes are the bytes"
        );

        let appended = dir.join("trace");
        {
            let mut file = open_private_append(&appended).expect("opened");
            file.write_all(b"line\n").expect("appended");
        }
        assert_eq!(mode_of(&appended), 0o600, "an append log is the same kind");

        let loose = dir.join("loose");
        std::fs::write(&loose, b"old").expect("written");
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644))
            .expect("chmod to the umask's habit");
        restrict(&loose).expect("tightened");
        assert_eq!(mode_of(&loose), 0o600, "an old file is repaired");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
