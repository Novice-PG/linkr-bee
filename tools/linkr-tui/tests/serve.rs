//! The resident session, end to end.
//!
//! A real process is started with `--serve` and fed requests on its standard
//! input. The point of the mode is that one link answers many requests — a
//! radio dial is the expensive part, and a caller working through a problem
//! should pay for it once — so the test asks several things in a row and
//! checks that the link is still up for the last of them, that each answer is
//! wrapped in the frame carrying the request's own id and the exit code its
//! one-shot equivalent would have had, and that a request the session cannot
//! serve leaves the session running.
#![cfg(feature = "cli")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How long any one line may take. Generous — a dial on a busy radio is
/// seconds — because a test that gets it wrong should fail an assertion, not
/// hang the suite.
const PATIENCE: Duration = Duration::from_secs(20);

/// One run of the binary, for the refusals that exit before a session opens.
fn linkr(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_linkr"))
        .args(args)
        .output()
        .expect("the binary runs")
}

/// A session under test, with its output read on a thread of its own.
struct Session {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
}

impl Session {
    fn start() -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_linkr"))
            .args(["--bridge", "-", "--serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Nothing here writes enough to a pipe to block on it, and a
            // session that dies with a filled stderr would hide the reason
            // behind a deadlock.
            .stderr(Stdio::null())
            .spawn()
            .expect("the binary starts");
        let input = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Session {
            child,
            input,
            lines,
        }
    }

    fn send(&mut self, request: &str) {
        writeln!(self.input, "{request}").expect("the request is written");
        self.input.flush().expect("the request is sent");
    }

    fn next(&self) -> String {
        self.lines
            .recv_timeout(PATIENCE)
            .expect("the session answers")
    }

    /// Read until a line contains `needle`, and return it.
    fn until(&self, needle: &str) -> String {
        loop {
            let line = self.next();
            if line.contains(needle) {
                return line;
            }
        }
    }

    /// Close the input — the signal a session hangs up on — and wait for the
    /// process, returning its exit code.
    fn finish(mut self) -> i32 {
        drop(self.input);
        self.child
            .wait()
            .expect("the session exits")
            .code()
            .expect("the session exits with a code")
    }
}

#[test]
fn a_session_answers_many_requests_on_one_link() {
    let mut session = Session::start();
    let ready = session.until("\"event\":\"ready\"");
    assert!(ready.contains("\"program\":\"linkr\""), "{ready}");
    assert!(ready.contains("\"ops\":["), "{ready}");
    assert!(ready.contains("\"exec\""), "{ready}");

    // Before anything needs the board there is no link: dialling lazily is
    // what lets a session start before the board is powered.
    session.send(r#"{"id":1,"op":"state"}"#);
    let begin = session.until("\"event\":\"begin\"");
    assert!(
        begin.contains("\"id\":1") && begin.contains("\"op\":\"state\""),
        "{begin}"
    );
    let state = session.until("\"what\":\"state\"");
    assert!(state.contains("\"live\":false"), "{state}");
    assert!(
        session.until("\"event\":\"end\"").contains("\"code\":0"),
        "the state request ends clean"
    );

    // A command dials the link and runs on it.
    session.send(r#"{"id":2,"op":"exec","cmd":"uname -a"}"#);
    assert!(session.until("\"op\":\"exec\"").contains("\"id\":2"));
    let result = session.until("\"event\":\"result\"");
    assert!(
        result.contains("\"ok\":true") && result.contains("\"rc\":0"),
        "{result}"
    );
    assert!(session.until("\"event\":\"end\"").contains("\"code\":0"));

    // The link is still up for the request after it — the whole point of the
    // mode.
    session.send(r#"{"id":3,"op":"state"}"#);
    let state = session.until("\"what\":\"state\"");
    assert!(
        state.contains("\"live\":true") && state.contains("\"connected\":true"),
        "{state}"
    );
    session.until("\"event\":\"end\"");

    // The status a one-shot `--exec` would have exited with arrives as the
    // frame's code, per request.
    session.send(r#"{"id":4,"op":"exec","cmd":"exit 42"}"#);
    session.until("\"op\":\"exec\"");
    let end = session.until("\"event\":\"end\"");
    assert!(end.contains("\"code\":42"), "{end}");

    // A line that is not a request is named and the session goes on.
    session.send("{not a request}");
    let error = session.until("\"event\":\"error\"");
    assert!(error.contains("\"code\":\"bad_request\""), "{error}");

    // As is an operation this build does not answer, framed like any other
    // request so a caller waiting on its id is not left waiting.
    session.send(r#"{"id":5,"op":"telnet"}"#);
    let error = session.until("\"event\":\"error\"");
    assert!(
        error.contains("\"id\":5") && error.contains("\"bad_request\""),
        "{error}"
    );
    assert!(session.until("\"event\":\"end\"").contains("\"code\":2"));

    session.send(r#"{"id":6,"op":"quit"}"#);
    let end = session.until("\"event\":\"end\"");
    assert!(end.contains("\"code\":0"), "{end}");
    assert_eq!(session.finish(), 0);
}

/// End of input is the session's hang-up: the caller closed the pipe because
/// it is done, and the process leaves with the code that says so.
#[test]
fn end_of_input_ends_the_session_cleanly() {
    let mut session = Session::start();
    session.until("\"event\":\"ready\"");
    session.send(r#"{"id":1,"op":"state"}"#);
    session.until("\"event\":\"end\"");
    assert_eq!(session.finish(), 0);
}

/// A session is a mode, and the flags that would be answered once and exit are
/// named as the contradiction they are: a caller passing `--scan` with
/// `--serve` wants two things, and neither silently winning is what this
/// checks. A session with no link named cannot serve anything that needs one
/// and says so rather than starting an empty one.
#[test]
fn a_session_that_names_something_else_to_do_is_a_usage_error() {
    for argv in [
        vec!["--bridge", "-", "--serve", "--scan"],
        vec!["--bridge", "-", "--serve", "--exec", "true"],
        vec!["--bridge", "-", "--serve", "--query-info"],
        vec!["--bridge", "-", "--serve", "--print-frame", "80x24"],
        vec!["--bridge", "-", "--serve", "--cli-then-tui"],
        vec!["--bridge", "-", "--serve", "--capabilities"],
        vec!["--serve"],
    ] {
        let output = linkr(&argv);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{argv:?} was not refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reported = String::from_utf8_lossy(&output.stderr);
        assert!(
            reported.starts_with("error:"),
            "{argv:?} was refused without saying why: {reported}"
        );
    }
}
