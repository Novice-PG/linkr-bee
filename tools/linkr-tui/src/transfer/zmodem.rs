//! The engine behind a ZMODEM transfer: the codec, a directory, and a file.
//!
//! [`crate::zmodem`] is a pure codec — bytes in, instructions out — and this is
//! the layer that gives it somewhere to put them. It is the one module under
//! `transfer/` that touches storage, which is why the module docs call it out:
//! everything there is a pure function of the wire, and this is a function of the
//! wire *and the disk*.
//!
//! Two things are deliberate.
//!
//! **The link is a byte stream, not a socket.** [`Engine::feed`] takes whatever
//! the transport produced and returns whatever should go back, so the engine owns
//! no socket, never blocks, and does not care whether the far end is a serial
//! line, a BLE notification stream, or a subprocess. That is what lets it be
//! driven from the TUI's event loop one tick at a time, and what lets the tests
//! drive it with a made-up link.
//!
//! **The far end's file name is not a path.** It becomes a path on this machine,
//! so it is reduced to its last component and checked before anything is opened.
//! See [`safe_name`].
//!
//! **A session is more than a codec.** [`Engine`] is the protocol; [`LiveTransfer`]
//! is the session an operator started — which direction it is and where the files
//! are going — because a banner that describes a transfer is not the codec's
//! business. The interface holds one `LiveTransfer` and asks it for everything.
//!
//! ZMODEM checks every block with a CRC, so a file that arrives is verified by
//! construction — which is why there is no verify step here, unlike the base64
//! shell protocol in the sibling modules, where nothing checks the bytes at all
//! and an unverified upload is a real possibility.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::zmodem::{FileInfo, Instruction, Parser, Policy, Receiver, Request, Sender};

/// What the transfer is doing, for whoever paints it. Cheap to read, so a view
/// can ask once a frame instead of being told about every kilobyte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The name being moved, as this machine would write it.
    pub name: String,
    /// Bytes moved so far.
    pub bytes: u64,
    /// The size the far end declared, when it said.
    pub total: Option<u64>,
    /// Whether this is going to the far end.
    pub upload: bool,
}

/// Something that happened, and will not happen again. Byte counts are not
/// events; they are [`Engine::progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A file is in flight.
    Started {
        name: String,
        total: Option<u64>,
        upload: bool,
    },
    /// A file finished. For a receive, it is on disk and every block checked.
    Saved { name: String, bytes: u64 },
    /// A file began and did not finish. For a receive, nothing is left behind.
    Failed { name: String, reason: String },
}

/// The file in flight and the record of it.
///
/// Kept apart from the codec so that the codec and the file can be borrowed at
/// the same time: `feed` holds a `&mut` to each, and a single struct holding both
/// would make that impossible without a dance of `take` and restore.
#[derive(Debug, Default)]
struct Store {
    /// The file being written to, or read from.
    file: Option<File>,
    /// The file in flight, as a view should show it.
    current: Option<Progress>,
    /// The name of a file the far end is sending that cannot be written here.
    ///
    /// Kept so the failure names the file, and so the end of a file that was
    /// never written is not reported as saved.
    refused: Option<String>,
    events: Vec<Event>,
    /// Whether the session has finished, as far as this layer can tell.
    done: bool,
}

impl Store {
    fn record(&mut self, bytes: u64) {
        if let Some(progress) = &mut self.current {
            progress.bytes = bytes;
        }
    }

    fn counted(&self) -> u64 {
        self.current.as_ref().map_or(0, |progress| progress.bytes)
    }

    fn name_now(&self) -> String {
        self.current
            .as_ref()
            .map(|progress| progress.name.clone())
            .unwrap_or_default()
    }

    /// Start writing a file the far end is sending.
    fn start_download(&mut self, name: String, total: u64) {
        self.current = Some(Progress {
            name: name.clone(),
            bytes: 0,
            total: Some(total),
            upload: false,
        });
        self.events.push(Event::Started {
            name,
            total: Some(total),
            upload: false,
        });
    }

    /// Start reading a file for the far end.
    fn start_upload(&mut self, name: String, file: File) {
        let total = file.metadata().ok().map(|meta| meta.len());
        self.file = Some(file);
        self.current = Some(Progress {
            name: name.clone(),
            bytes: 0,
            total,
            upload: true,
        });
        self.events.push(Event::Started {
            name,
            total,
            upload: true,
        });
    }

    fn fail(&mut self, name: String, reason: String) {
        self.events.push(Event::Failed { name, reason });
    }
}

/// What the engine is doing.
enum Mode {
    /// Taking files into a directory.
    Receive {
        dir: PathBuf,
        codec: Box<Receiver>,
        store: Store,
    },
    /// Offering files from wherever they are.
    Send {
        queue: VecDeque<PathBuf>,
        codec: Box<Sender>,
        store: Store,
    },
}

/// One ZMODEM session, over a link the caller owns.
pub struct Engine {
    mode: Mode,
    parser: Parser,
}

/// A `Debug` that says what the session *is* rather than dumping the codec.
///
/// `App` is `Debug`, so this has to be — and a frame or a test that prints a
/// transfer wants "receiving into /tmp/x, 2 files left", not several hundred
/// bytes of parser state.
impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match &self.mode {
            Mode::Receive { dir, .. } => format!("receive into {}", dir.display()),
            Mode::Send { queue, .. } => format!("send, {} file(s) left", queue.len()),
        };
        f.debug_struct("Engine")
            .field("mode", &what)
            .field("done", &self.is_done())
            .finish()
    }
}

impl Engine {
    /// Take whatever the far end sends into `dir`.
    ///
    /// Directories and anything that is not an ordinary file are refused with
    /// `ZSKIP`: a receive cannot recreate them, and a far end offering one is
    /// either confused or trying something.
    pub fn receiving(dir: impl Into<PathBuf>) -> Engine {
        Engine {
            mode: Mode::Receive {
                dir: dir.into(),
                codec: Box::new(Receiver::with_policy(Policy::RegularFilesOnly)),
                store: Store::default(),
            },
            parser: Parser::new(),
        }
    }

    /// Offer these paths, in this order. Their sizes are read now, because a
    /// sender has to name a file before the far end can agree to take it.
    ///
    /// The far end is assumed to be *already* in a ZMODEM receive — a real `rz`
    /// with the dialogue running. When it is instead a shell prompt, use
    /// [`Engine::sending_over_shell`].
    pub fn sending(paths: impl IntoIterator<Item = PathBuf>) -> std::io::Result<Engine> {
        Engine::sending_with(paths, false)
    }

    /// Offer these paths to a shell, which has to be asked to start a receiver.
    ///
    /// The `rz\r` wakeup is written before the first frame, exactly as a real
    /// `sz` does — the recorded `sz → rz` dialogue this codec is tested against
    /// opens with those three bytes. That is what makes the send direction usable
    /// without the operator first typing `rz` on the device.
    pub fn sending_over_shell(paths: impl IntoIterator<Item = PathBuf>) -> std::io::Result<Engine> {
        Engine::sending_with(paths, true)
    }

    fn sending_with(
        paths: impl IntoIterator<Item = PathBuf>,
        wakeup: bool,
    ) -> std::io::Result<Engine> {
        let mut infos = Vec::new();
        let mut queue = VecDeque::new();
        for path in paths {
            infos.push(describe(&path)?);
            queue.push_back(path);
        }
        Ok(Engine {
            mode: Mode::Send {
                queue,
                codec: Box::new(Sender::new(infos).wakeup(wakeup)),
                store: Store::default(),
            },
            parser: Parser::new(),
        })
    }

    /// The bytes that open the session: a receiver announces itself, a sender
    /// says hello. Nothing else happens until the far end answers.
    pub fn start(&mut self) -> Vec<u8> {
        match &mut self.mode {
            Mode::Receive { codec, .. } => {
                let mut out = Vec::new();
                for instruction in codec.announce() {
                    if let Instruction::Send(bytes) = instruction {
                        out.extend(bytes);
                    }
                }
                out
            }
            Mode::Send { codec, .. } => {
                let mut out = Vec::new();
                for request in codec.start() {
                    if let Request::Send(bytes) = request {
                        out.extend(bytes);
                    }
                }
                out
            }
        }
    }

    /// Fold in whatever the link produced, and get back what to write to it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        // A sender reads the file the far end has just agreed to, so the handle
        // is opened here rather than when the session began.
        self.open_source();

        self.parser.feed(bytes);
        let mut out = Vec::new();
        while let Some(event) = self.parser.next_event() {
            match &mut self.mode {
                Mode::Receive { dir, codec, store } => {
                    for instruction in codec.on_event(event) {
                        out.extend(receive(dir, store, instruction));
                    }
                }
                Mode::Send { codec, store, .. } => {
                    let mut work: VecDeque<Request> = codec.on_event(event).into();
                    while let Some(request) = work.pop_front() {
                        match request {
                            Request::Send(bytes) => out.extend(bytes),
                            // Supplying a block is what asks for the next one, so
                            // the work list grows as the file goes out.
                            Request::Read { round, at, len } => {
                                let block = read_block(&mut store.file, at, len);
                                work.extend(codec.supply(round, &block));
                            }
                            other => send(store, other),
                        }
                    }
                }
            }
        }
        out
    }

    /// Whether the session is over.
    pub fn is_done(&self) -> bool {
        let (store, codec_done) = match &self.mode {
            Mode::Receive { codec, store, .. } => (store, codec.is_done()),
            Mode::Send { codec, store, .. } => (store, codec.is_done()),
        };
        store.done || codec_done
    }

    /// The file in flight, as a view should show it.
    pub fn progress(&self) -> Option<&Progress> {
        self.store().current.as_ref()
    }

    /// Everything that happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.store_mut().events)
    }

    fn store(&self) -> &Store {
        match &self.mode {
            Mode::Receive { store, .. } | Mode::Send { store, .. } => store,
        }
    }

    fn store_mut(&mut self) -> &mut Store {
        match &mut self.mode {
            Mode::Receive { store, .. } | Mode::Send { store, .. } => store,
        }
    }

    /// Open the next file, once the far end has agreed to take it.
    fn open_source(&mut self) {
        let Mode::Send {
            queue,
            codec,
            store,
        } = &mut self.mode
        else {
            return;
        };
        if store.current.is_some() {
            return;
        }
        let Some(path) = queue.front().cloned() else {
            return;
        };
        let name = file_name_of(&path);
        // The sender names a file when it offers it and moves on only when the
        // far end says it is ready for more, so this is the moment the handle is
        // needed — and the name on the wire is what says which file it is.
        let Some(offered) = codec.current() else {
            return;
        };
        if offered.name() != name {
            return;
        }
        queue.pop_front();
        match File::open(&path) {
            Ok(file) => store.start_upload(name, file),
            Err(err) => store.fail(name, err.to_string()),
        }
    }
}

/// A transfer an operator started: the engine, plus what a banner has to say.
///
/// The two are one value on purpose. A direction kept beside the engine is a
/// direction that can outlive it, and a banner reading "receiving…" over a
/// session that has already closed is worse than no banner at all.
#[derive(Debug)]
pub struct LiveTransfer {
    engine: Engine,
    /// Whether this host is the sending end.
    pub upload: bool,
    /// What the operator named: the local file a send offers, the directory a
    /// receive writes into. Shown as it was typed, so the notice names the thing
    /// they asked for rather than a path we made up.
    pub target: String,
    /// How many files a send is offering. Zero for a receive, which takes
    /// whatever arrives.
    pub files: usize,
}

impl LiveTransfer {
    /// Offer one local file to a device whose shell is at a prompt.
    ///
    /// The wakeup is on: over this bridge the far end *is* a shell, so a
    /// receiver has to be started before it can be spoken to. Nothing is read
    /// yet — the file is opened when the far end agrees to take it.
    pub fn send(paths: Vec<PathBuf>) -> std::io::Result<LiveTransfer> {
        let target = match paths.as_slice() {
            [only] => only.display().to_string(),
            many => format!("{} files", many.len()),
        };
        Ok(LiveTransfer {
            files: paths.len(),
            engine: Engine::sending_over_shell(paths)?,
            upload: true,
            target,
        })
    }

    /// Take whatever the device offers into `dir`.
    ///
    /// The directory is not created: a path the operator typed that does not
    /// exist fails on the first file, with the name of the file, which is a
    /// better report than a transfer that silently went somewhere else.
    pub fn receive(dir: impl Into<PathBuf>) -> LiveTransfer {
        let dir = dir.into();
        LiveTransfer {
            engine: Engine::receiving(dir.clone()),
            upload: false,
            target: dir.display().to_string(),
            files: 0,
        }
    }

    /// The frames that open the session, for the caller to write.
    pub fn opening_bytes(&mut self) -> Vec<u8> {
        self.engine.start()
    }

    /// Fold in whatever the link produced, and get back what to write to it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.engine.feed(bytes)
    }

    /// Everything that happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.engine.take_events()
    }

    /// The file in flight, as a view should show it.
    pub fn progress(&self) -> Option<&Progress> {
        self.engine.progress()
    }

    /// Whether the session is over.
    pub fn is_done(&self) -> bool {
        self.engine.is_done()
    }

    /// Throw away a half-written download.
    ///
    /// Called when a transfer is given up on — a cancel, or a link that went
    /// away. Only a receive has anything to undo *here*: the bytes landed in a
    /// file this machine will otherwise show as a finished one. An upload's
    /// half file is on the far end, which is the far end's to clean up.
    pub fn discard_partial(&self) {
        if self.upload {
            return;
        }
        let Some(progress) = self.progress() else {
            return;
        };
        // The name in `Progress` is the one `open_destination` already put
        // through `safe_name`, so joining it cannot leave the directory.
        let _ = std::fs::remove_file(Path::new(&self.target).join(&progress.name));
    }
}

/// Act on one instruction a receiver gave us, returning any bytes to write.
fn receive(dir: &Path, store: &mut Store, instruction: Instruction) -> Vec<u8> {
    match instruction {
        Instruction::Send(bytes) => bytes,
        Instruction::Start(info) => {
            open_destination(dir, store, &info);
            Vec::new()
        }
        Instruction::Chunk(data) => {
            write_chunk(store, &data);
            Vec::new()
        }
        Instruction::Finish(info) => {
            finish_received(store, &info);
            Vec::new()
        }
        Instruction::Resume { at } => {
            // The far end is resending from here, so everything after this offset
            // is suspect. Truncating is what makes the retry land in the right
            // place instead of appending to a corrupt tail.
            if let Some(file) = store.file.as_mut() {
                let _ = file.set_len(at);
                let _ = file.seek(SeekFrom::Start(at));
            }
            store.record(at);
            Vec::new()
        }
        Instruction::Fail(why) => {
            fail_received(dir, store, format!("{why:?}"));
            Vec::new()
        }
    }
}

/// Open the file a receiver just agreed to take.
fn open_destination(dir: &Path, store: &mut Store, info: &FileInfo) {
    let offered = info.name();
    store.refused = None;

    let Some(name) = safe_name(&offered) else {
        store.refused = Some(offered.clone());
        store.fail(
            offered,
            "the far end's name is not usable as a file name".to_string(),
        );
        return;
    };

    match File::create(dir.join(&name)) {
        Ok(file) => {
            store.file = Some(file);
            store.start_download(name, info.length);
        }
        Err(err) => {
            store.refused = Some(name.clone());
            store.fail(name, err.to_string());
        }
    }
}

fn write_chunk(store: &mut Store, data: &[u8]) {
    if let Some(file) = store.file.as_mut() {
        if let Err(err) = file.write_all(data) {
            let name = store.name_now();
            store.file = None;
            store.current = None;
            store.refused = Some(name.clone());
            store.fail(name, err.to_string());
        }
    }
    // Counted either way: the far end really did send these bytes, and a
    // progress line that froze would look like a stalled link.
    let bytes = store.counted() + data.len() as u64;
    store.record(bytes);
}

fn finish_received(store: &mut Store, info: &FileInfo) {
    if store.refused.take().is_some() {
        // Already reported when it happened. A second failure for the same file
        // would read as two things having gone wrong.
        store.file = None;
        store.current = None;
        return;
    }
    if let Some(mut file) = store.file.take() {
        if let Err(err) = file.flush() {
            store.current = None;
            store.fail(info.name(), err.to_string());
            return;
        }
    }
    let bytes = info.length;
    store.record(bytes);
    store.current = None;
    store.events.push(Event::Saved {
        name: info.name(),
        bytes,
    });
}

/// A receive that failed leaves nothing behind. A partial file that looks like a
/// finished one is worse than no file at all, because nothing about it says the
/// end is missing.
fn fail_received(dir: &Path, store: &mut Store, reason: String) {
    store.file = None;
    store.refused = None;
    let Some(name) = store.current.take().map(|progress| progress.name) else {
        return;
    };
    let _ = std::fs::remove_file(dir.join(&name));
    store.fail(name, reason);
}

/// Note the end of a file for the sender's own bookkeeping.
fn send(store: &mut Store, request: Request) {
    match request {
        // `Send` and `Read` are handled by `feed`, which owns the bytes.
        Request::Send(_) | Request::Read { .. } => {}
        // The queue is popped when a file is *opened*, not when it is finished:
        // popping here as well removed the next file's entry, and nothing was
        // ever read for it.
        Request::Sent(info) => {
            store.file = None;
            store.current = None;
            store.events.push(Event::Saved {
                name: info.name(),
                bytes: info.length,
            });
        }
        Request::Skipped(info) => {
            store.file = None;
            store.current = None;
            store.fail(info.name(), "refused by the far end".to_string());
        }
        Request::Failed(why) => {
            store.done = true;
            store.file = None;
            let name = store.name_now();
            store.current = None;
            store.fail(name, format!("{why:?}"));
        }
        Request::Finished => {
            store.done = true;
            store.current = None;
        }
    }
}

/// A file's size and metadata, as the far end has to hear them.
fn describe(path: &Path) -> std::io::Result<FileInfo> {
    let meta = std::fs::metadata(path)?;
    let mut info = FileInfo::new(file_name_of(path).into_bytes(), meta.len());
    info.modified = modified(&meta);
    info.mode = mode(&meta);
    info.remaining = 1;
    Ok(info)
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The file name to write, or `None` if the far end's name is not one.
///
/// The name comes from the far end and becomes a path here, so it is not taken at
/// face value: `../../.ssh/authorized_keys` and `/etc/shadow` are both names a
/// device could offer. Only the last component is ever written, which drops every
/// directory walk at once, and `.`, `..`, the empty name and a NUL are refused.
/// A backslash is not a separator on this platform but is on another, so it is
/// treated as one here as well rather than becoming a file whose name is a
/// surprise.
pub fn safe_name(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    if last.is_empty() || last == "." || last == ".." || last.contains('\0') {
        return None;
    }
    Some(last.to_string())
}

/// Read `len` bytes at `at` from a file the engine is holding open. A short read
/// is returned short, and the sender turns that into a failed session rather than
/// a silently truncated file.
fn read_block(file: &mut Option<File>, at: u64, len: usize) -> Vec<u8> {
    let mut block = vec![0u8; len];
    let Some(file) = file.as_mut() else {
        return Vec::new();
    };
    let mut filled = 0;
    while filled < block.len() {
        match read_at(file, &mut block[filled..], at + filled as u64) {
            Ok(0) | Err(_) => break,
            Ok(got) => filled += got,
        }
    }
    block.truncate(filled);
    block
}

/// Read into `buf` from an absolute offset, without moving a shared file handle.
///
/// The three arms exist because `pread(2)` and its Windows counterpart have
/// different names and the fallback has to move the handle. The engine holds one
/// file open across the many reads of a transfer, so a seek-and-read would be
/// correct too; this keeps the sender's offsets independent of anything else that
/// might read the handle.
#[cfg(unix)]
fn read_at(file: &mut File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(&*file, buf, at)
}

#[cfg(windows)]
fn read_at(file: &mut File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(&*file, buf, at)
}

#[cfg(not(any(unix, windows)))]
fn read_at(file: &mut File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    use std::io::Read;
    file.seek(SeekFrom::Start(at))?;
    file.read(buf)
}

#[cfg(unix)]
fn mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode()
}

#[cfg(not(unix))]
fn mode(_meta: &std::fs::Metadata) -> u32 {
    // ZMODEM's mode field is the unix one. A platform without it says nothing,
    // which a receiver reads as "did not say" rather than as "mode 000".
    0
}

#[cfg(unix)]
fn modified(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime().max(0) as u64
}

#[cfg(not(unix))]
fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::{subpacket, Encoding, FrameKind, Framend, Header, Width};

    /// A directory that cleans itself up, so a failing test does not leave files
    /// in the way of the next run.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> TempDir {
            let unique = format!("linkr-zmodem-{label}-{}", std::process::id());
            let path = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a temp directory");
            TempDir(path)
        }

        fn sub(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).expect("a subdirectory");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Every byte value in turn, past several block boundaries, so the escape
    /// layer and the block splitting both get exercised.
    fn payload(length: usize) -> Vec<u8> {
        (0..length).map(|index| (index % 251) as u8).collect()
    }

    /// Run two engines against each other until neither has anything to say,
    /// passing the bytes the way a link would.
    fn converse(sending: &mut Engine, receiving: &mut Engine) {
        let mut to_receiver = sending.start();
        let mut to_sender = receiving.start();
        let mut steps = 0;
        while !(to_receiver.is_empty() && to_sender.is_empty()) {
            steps += 1;
            assert!(
                steps < 100_000,
                "a dialogue that does not end is not a dialogue"
            );
            if !to_receiver.is_empty() {
                to_sender.extend(receiving.feed(&to_receiver));
                to_receiver = Vec::new();
            }
            if !to_sender.is_empty() {
                to_receiver.extend(sending.feed(&to_sender));
                to_sender = Vec::new();
            }
        }
    }

    fn binary_header(kind: FrameKind, data: [u8; 4]) -> Vec<u8> {
        crate::zmodem::encode_header(Header::new(kind, data), Encoding::Bin32)
    }

    #[test]
    fn a_name_from_the_far_end_cannot_climb_out_of_the_directory() {
        assert_eq!(safe_name("notes.txt").as_deref(), Some("notes.txt"));
        assert_eq!(
            safe_name("logs/app.txt").as_deref(),
            Some("app.txt"),
            "a far end naming a subdirectory means the file, not the directory"
        );
        assert_eq!(
            safe_name("../../.ssh/authorized_keys").as_deref(),
            Some("authorized_keys")
        );
        assert_eq!(safe_name("/etc/shadow").as_deref(), Some("shadow"));
        assert_eq!(
            safe_name("..\\..\\windows\\evil.dll").as_deref(),
            Some("evil.dll")
        );
        assert_eq!(
            safe_name("/").as_deref(),
            None,
            "a bare path has no name in it"
        );
        assert_eq!(safe_name(".."), None);
        assert_eq!(safe_name("."), None);
        assert_eq!(safe_name(""), None);
        assert_eq!(safe_name("a\0b"), None, "a NUL cannot be in a path");
    }

    /// The whole receive path, driven by the bytes a sender really writes — and
    /// that sender is itself checked against a real `rz`. What lands on disk has
    /// to be the file that went in.
    #[test]
    fn a_received_file_lands_in_the_directory_with_the_bytes_that_were_sent() {
        let root = TempDir::new("receive");
        let source_dir = root.sub("send");
        let dest_dir = root.sub("recv");
        let body = payload(2500);
        std::fs::write(source_dir.join("report.bin"), &body).unwrap();

        let mut sending = Engine::sending([source_dir.join("report.bin")]).expect("a sender");
        let mut receiving = Engine::receiving(&dest_dir);
        converse(&mut sending, &mut receiving);

        let arrived = std::fs::read(dest_dir.join("report.bin")).expect("the file landed");
        assert_eq!(arrived, body, "and it arrived as it was sent");

        let saved = receiving.take_events();
        assert!(
            saved
                .iter()
                .any(|event| matches!(event, Event::Saved { name, bytes } if name == "report.bin" && *bytes == 2500)),
            "the caller is told the file was saved and how big it was"
        );
        assert!(
            saved
                .iter()
                .any(|event| matches!(event, Event::Started { upload: false, .. })),
            "and that it started"
        );
        assert!(receiving.is_done() && sending.is_done());
        assert_eq!(
            sending.take_events().last(),
            Some(&Event::Saved {
                name: "report.bin".to_string(),
                bytes: 2500,
            }),
            "the sender reports the same file, so both ends can say so"
        );
    }

    /// A file that stops short is not left on disk looking complete.
    #[test]
    fn a_receive_that_fails_leaves_nothing_behind() {
        let root = TempDir::new("failed");
        let dest_dir = root.sub("recv");
        let mut receiving = Engine::receiving(&dest_dir);
        receiving.start();

        // A name block declaring 100 bytes, nine bytes of file, then a `ZEOF`
        // that repeats the claim.
        let info = FileInfo::new(*b"short.txt", 100);
        let width = Width::ThirtyTwo;
        let mut stream = binary_header(FrameKind::File, [1, 0, 0, 0]);
        stream.extend(subpacket::encode(&info.to_block(), Framend::Zcrcw, width));
        stream.extend(binary_header(FrameKind::Data, [0, 0, 0, 0]));
        stream.extend(subpacket::encode(b"only nine", Framend::Zcrce, width));
        stream.extend(binary_header(FrameKind::Eof, 100u32.to_le_bytes()));
        receiving.feed(&stream);

        assert!(
            !dest_dir.join("short.txt").exists(),
            "a short file is removed rather than left looking complete"
        );
        let events = receiving.take_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Failed { name, .. } if name == "short.txt")),
            "and the caller is told which file failed"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Saved { .. })),
            "nothing is ever reported saved that was not"
        );
    }

    #[test]
    fn sending_names_the_file_from_its_own_metadata() {
        let root = TempDir::new("describe");
        let path = root.0.join("report.bin");
        std::fs::write(&path, payload(1234)).unwrap();

        let info = describe(&path).expect("a description");
        assert_eq!(info.name(), "report.bin");
        assert_eq!(info.length, 1234);
        assert_eq!(info.remaining, 1, "one file in this batch");
        assert!(info.is_regular(), "and it is named as an ordinary file");
    }

    /// Two files in one session: the second has to be opened when the far end
    /// gets to it, not when the session began.
    #[test]
    fn a_second_file_is_opened_when_the_far_end_gets_to_it() {
        let root = TempDir::new("two");
        let source_dir = root.sub("send");
        let dest_dir = root.sub("recv");
        let first = payload(1200);
        let second = payload(3000);
        std::fs::write(source_dir.join("a.bin"), &first).unwrap();
        std::fs::write(source_dir.join("b.bin"), &second).unwrap();

        let mut sending = Engine::sending([source_dir.join("a.bin"), source_dir.join("b.bin")])
            .expect("a sender");
        let mut receiving = Engine::receiving(&dest_dir);
        converse(&mut sending, &mut receiving);

        assert_eq!(std::fs::read(dest_dir.join("a.bin")).unwrap(), first);
        assert_eq!(std::fs::read(dest_dir.join("b.bin")).unwrap(), second);
        let saved: Vec<String> = receiving
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Saved { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(
            saved,
            ["a.bin", "b.bin"],
            "both, in the order they were offered"
        );
    }

    /// The status line reads the progress rather than a stream of events, so it
    /// has to be right while a file is in flight.
    ///
    /// The bytes are handed over in pieces, the way a link delivers them. Feeding
    /// the sender's whole output at once would not show a count climbing: one
    /// call drains the entire file, which is correct and would make this test say
    /// nothing.
    #[test]
    fn progress_names_the_file_and_counts_the_bytes_as_they_arrive() {
        let root = TempDir::new("progress");
        let source_dir = root.sub("send");
        let dest_dir = root.sub("recv");
        std::fs::write(source_dir.join("big.bin"), payload(2500)).unwrap();

        let mut sending = Engine::sending([source_dir.join("big.bin")]).expect("a sender");
        let mut receiving = Engine::receiving(&dest_dir);

        let mut to_receiver = sending.start();
        let mut to_sender = receiving.start();
        let mut seen = Vec::new();
        let mut steps = 0;
        loop {
            steps += 1;
            assert!(
                steps < 100_000,
                "a dialogue that does not end is not a dialogue"
            );
            if !to_receiver.is_empty() {
                for piece in to_receiver.chunks(700) {
                    to_sender.extend(receiving.feed(piece));
                    if let Some(progress) = receiving.progress() {
                        seen.push((
                            progress.name.clone(),
                            progress.bytes,
                            progress.total,
                            progress.upload,
                        ));
                    }
                }
                to_receiver.clear();
            }
            if !to_sender.is_empty() {
                to_receiver.extend(sending.feed(&to_sender));
                to_sender.clear();
            }
            if to_receiver.is_empty() && to_sender.is_empty() {
                break;
            }
        }

        assert!(
            seen.iter().all(|(name, _, total, upload)| name == "big.bin"
                && *total == Some(2500)
                && !*upload),
            "the far end's name and the size it declared are known from the start"
        );
        assert!(
            seen.windows(2).all(|pair| pair[0].1 <= pair[1].1),
            "a count that goes backwards would be a lie"
        );
        assert!(
            seen.iter()
                .any(|(_, bytes, ..)| *bytes > 0 && *bytes < 2500),
            "and it is observably part-way through, not just started or done"
        );
        assert!(
            seen.iter()
                .all(|(_, bytes, total, _)| total.is_none_or(|total| *bytes <= total)),
            "a count past the size the far end declared would be a lie too"
        );
        // The final count is not visible here: the last piece carries the
        // end-of-file, and the file in flight is cleared as it lands. That count
        // is what the `Saved` event carries, and the test above checks it.
        assert!(
            receiving.progress().is_none(),
            "and the file in flight is cleared once it is done"
        );
    }

    /// A send asks the far end's shell for a receiver before it says anything
    /// else, and does it *without* being driven by a `rz` that was already
    /// running: the opening frames alone have to wake one up.
    #[test]
    fn a_send_asks_the_shell_for_a_receiver_before_it_speaks() {
        let root = TempDir::new("wakeup");
        let dest_dir = root.sub("recv");
        let body = payload(1500);
        std::fs::write(root.0.join("fw.bin"), &body).unwrap();

        let mut live = LiveTransfer::send(vec![root.0.join("fw.bin")]).expect("a sender");
        let opening = live.opening_bytes();
        assert!(
            opening.starts_with(b"rz\r"),
            "the wakeup is missing: {:?}",
            String::from_utf8_lossy(&opening[..opening.len().min(16)])
        );
        assert!(live.upload);
        assert_eq!(live.files, 1);
        assert_eq!(live.target, root.0.join("fw.bin").display().to_string());
        assert!(!live.is_done());

        // The far end answers as a receiver, and the file goes through.
        let mut receiving = Engine::receiving(&dest_dir);
        let mut to_receiver = opening;
        let mut to_sender = receiving.start();
        let mut steps = 0;
        while !(to_receiver.is_empty() && to_sender.is_empty()) {
            steps += 1;
            assert!(
                steps < 100_000,
                "a dialogue that does not end is not a dialogue"
            );
            if !to_receiver.is_empty() {
                to_sender.extend(receiving.feed(&to_receiver));
                to_receiver = Vec::new();
            }
            if !to_sender.is_empty() {
                to_receiver.extend(live.feed(&to_sender));
                to_sender = Vec::new();
            }
        }

        assert_eq!(std::fs::read(dest_dir.join("fw.bin")).unwrap(), body);
        assert!(live.is_done() && receiving.is_done());
        assert!(
            live.take_events()
                .iter()
                .any(|event| matches!(event, Event::Saved { name, .. } if name == "fw.bin")),
            "the sender says which file went"
        );
    }

    /// Giving up on a receive takes the half file with it. A partial file that
    /// looks whole is the failure mode a cancel must not leave behind.
    #[test]
    fn giving_up_on_a_receive_removes_the_half_file() {
        let root = TempDir::new("discard");
        let dest_dir = root.sub("recv");
        let mut live = LiveTransfer::receive(&dest_dir);
        assert!(!live.upload);
        assert_eq!(live.target, dest_dir.display().to_string());
        live.opening_bytes();

        // A name block declaring 100 bytes and nine bytes of it, with no `ZEOF`:
        // the transfer is mid-file, which is exactly when a cancel happens.
        let info = FileInfo::new(*b"part.txt", 100);
        let mut stream = binary_header(FrameKind::File, [1, 0, 0, 0]);
        stream.extend(subpacket::encode(
            &info.to_block(),
            Framend::Zcrcw,
            Width::ThirtyTwo,
        ));
        stream.extend(binary_header(FrameKind::Data, [0, 0, 0, 0]));
        stream.extend(subpacket::encode(
            b"only nine",
            Framend::Zcrce,
            Width::ThirtyTwo,
        ));
        live.feed(&stream);

        let half = dest_dir.join("part.txt");
        assert!(half.exists(), "the half file is there to be cleaned up");
        assert_eq!(
            live.progress().map(|progress| progress.name.as_str()),
            Some("part.txt")
        );

        live.discard_partial();
        assert!(
            !half.exists(),
            "a cancelled receive leaves no file that looks complete"
        );
    }

    /// A send has nothing to clean up on this side of the wire: its half file is
    /// on the far end. Asking anyway must not touch the source file.
    #[test]
    fn giving_up_on_a_send_leaves_the_source_alone() {
        let root = TempDir::new("keep");
        let source = root.0.join("fw.bin");
        std::fs::write(&source, payload(64)).unwrap();

        let live = LiveTransfer::send(vec![source.clone()]).expect("a sender");
        live.discard_partial();
        assert!(source.exists(), "the file being sent was deleted");
    }
}
