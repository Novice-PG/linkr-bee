//! The exec contract, locked against the built-in loopback.
//!
//! Each of these runs the real binary with `--bridge -`, no hardware and no
//! radio: the loopback's shell answers the wrapper for real, so the endings
//! the CLI promises — a status that passes through, a quoted argument that
//! runs as one argument, a password prompt that stops the run, an idle window
//! that is *not* a success — are checked the way a caller meets them, from a
//! shell, with the exit codes a script branches on. A dial that cannot be
//! made is checked the same way, in the event and the code the report
//! promises for any script run.
//!
//! These are the cases a field report asked to have locked, so they fail if
//! the loopback drifts from the contract, not if a part of it is missing.
#![cfg(feature = "cli")]

use std::process::{Command, Output};

fn linkr(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_linkr"))
        .args(args)
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A command that ends with a status is a command a caller can branch on: the
/// status arrives in the event, is marked verified there, and *is* the
/// process exit code.
#[test]
fn the_target_status_passes_through_to_the_exit_code() {
    let output = linkr(&["--bridge", "-", "--exec", "exit 42", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr: {}",
        stderr(&output)
    );
    let out = stdout(&output);
    assert!(out.contains("\"event\":\"result\""), "{out}");
    assert!(out.contains("\"rc\":42"), "{out}");
    assert!(out.contains("\"ok\":true"), "{out}");
    assert!(out.contains("\"verified\":true"), "{out}");
}

/// Quotes group: a format quoted as one argument keeps the spaces inside it,
/// which is the difference between `printf 'a  b'` printing two spaces and a
/// shell that never found `printf` at all.
#[test]
fn a_quoted_argument_reaches_the_command_intact() {
    let output = linkr(&["--bridge", "-", "--exec", "printf 'a  b\\n'", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("a  b"),
        "the quoted argument lost its spacing: {out}"
    );
}

/// A prompt that has not finished its line is the target waiting for a
/// person, and the run says so instead of waiting out its timeout.
#[test]
fn a_password_prompt_ends_the_run_needing_input() {
    let output = linkr(&["--bridge", "-", "--exec", "sudo id", "--json"]);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("\"event\":\"error\""), "{out}");
    assert!(out.contains("\"code\":\"needs_input\""), "{out}");
    assert!(out.contains("still_running"), "{out}");
}

/// The idle ending is what it says: the output went quiet, no status was
/// read, and neither the event nor the exit code claims a success.
#[test]
fn an_idle_window_is_not_a_verified_success() {
    let output = linkr(&[
        "--bridge", "-", "--exec", "uname -a", "--idle", "0.3", "--json",
    ]);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("\"mode\":\"idle\""), "{out}");
    assert!(out.contains("\"ok\":false"), "{out}");
    assert!(out.contains("\"verified\":false"), "{out}");
    assert!(out.contains("\"rc\":null"), "{out}");
    // The output is still reported: what is missing is the status, and the
    // report says which one went missing.
    assert!(out.contains("Linux bee"), "{out}");
}

/// A script run whose link could not be dialled gets one event naming the
/// reason and the code it can branch on, whatever it was going to ask for —
/// not just `--exec`, which is where the old spelling lived.
#[test]
fn a_script_run_that_could_not_dial_says_not_connected() {
    let output = linkr(&[
        "--bridge",
        "/nonexistent/linkr-bridge",
        "--query-info",
        "--json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(255),
        "stderr: {}",
        stderr(&output)
    );
    let out = stdout(&output);
    assert!(out.contains("\"event\":\"error\""), "{out}");
    assert!(out.contains("\"code\":\"not_connected\""), "{out}");
    assert!(out.contains("\"still_running\":false"), "{out}");
}
