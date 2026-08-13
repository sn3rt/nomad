//! A fake `ssh` (and, via a `waypipe` symlink, `waypipe`) used only by the
//! integration tests in `tests/cli.rs`. Real integration tests spawn the
//! actual `nomad` binary, which shells out to whatever `ssh`/`waypipe` it
//! finds on `PATH` — the test harness prepends a temp dir containing this
//! binary (symlinked as both names) so nothing ever touches the network.
//!
//! Behavior is dispatched purely from argv shape (matching exactly what
//! `src/transport.rs`'s `OpenSshTransport` emits) and configured per test
//! via `FAKE_SSH_*` environment variables, which are inherited transitively
//! from the `nomad` child process. Every invocation is appended as one JSON
//! line to the path in `FAKE_SSH_LOG`, so tests can assert on exact argument
//! forwarding and call sequencing.
//!
//! Gated behind the `test-support` Cargo feature so it's never part of a
//! normal release build.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::Path;

const STDIN_PREVIEW_CAP: usize = 256;

#[derive(serde::Serialize)]
struct Invocation<'a> {
    shape: &'a str,
    argv: &'a [String],
    stdin_len: usize,
    stdin_preview: &'a str,
}

fn main() {
    let full_argv: Vec<String> = env::args().collect();
    let basename = Path::new(&full_argv[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&full_argv[0])
        .to_string();

    // `waypipe` invocations always carry a mandatory leading "ssh" argument
    // (transport.rs: `Command::new("waypipe").arg("ssh")`); strip it for
    // dispatch purposes but keep the original argv for logging so tests can
    // assert on the waypipe-then-ssh shape.
    let mut dispatch_args: Vec<String> = full_argv[1..].to_vec();
    if basename == "waypipe" && dispatch_args.first().map(String::as_str) == Some("ssh") {
        dispatch_args.remove(0);
    }

    let has = |flag: &str| dispatch_args.iter().any(|a| a == flag);

    let (shape, exit_code, stdout_text, drain_stdin) = if has("-MNf") && has("ControlMaster=yes") {
        (
            "control_open",
            env_i32("FAKE_SSH_CONTROL_OPEN_EXIT", 0),
            None,
            false,
        )
    } else if has("-O") && has("exit") {
        ("control_close", 0, None, false)
    } else if has("-tt") {
        let shape = if basename == "waypipe" {
            "waypipe_interactive"
        } else {
            "interactive"
        };
        (shape, env_i32("FAKE_SSH_INTERACTIVE_EXIT", 0), None, false)
    } else {
        dispatch_plain_command(dispatch_args.last().map(String::as_str).unwrap_or(""))
    };

    let (stdin_len, stdin_preview) = if drain_stdin {
        drain_stdin_capped()
    } else {
        (0, String::new())
    };

    log_invocation(shape, &full_argv, stdin_len, &stdin_preview);

    if let Some(text) = stdout_text {
        println!("{text}");
    }

    std::process::exit(exit_code);
}

/// Dispatches the trailing remote-command string carried by a plain
/// `ssh -S <socket> ... <dest> <remote_cmd>` invocation (i.e. everything
/// that isn't control_open/control_close/interactive).
fn dispatch_plain_command(remote_cmd: &str) -> (&'static str, i32, Option<String>, bool) {
    if remote_cmd.starts_with("mktemp") {
        (
            "mktemp",
            0,
            Some(env::var("FAKE_SSH_MKTEMP_OUTPUT").unwrap_or_default()),
            false,
        )
    } else if remote_cmd.starts_with("command -v zsh") {
        (
            "shell_resolve",
            0,
            Some(env::var("FAKE_SSH_REMOTE_SHELL").unwrap_or_default()),
            false,
        )
    } else if remote_cmd.starts_with("test -d") {
        ("test_d", env_i32("FAKE_SSH_TEST_D_EXIT", 1), None, false)
    } else if remote_cmd.contains("tar -C") && remote_cmd.contains("-xf") {
        // Real stdin (piped directly from the local `tar` child, bypassing
        // the Rust parent) MUST be drained or the writer can block once the
        // pipe buffer fills.
        ("tar_extract", 0, None, true)
    } else if remote_cmd.starts_with("cat >") {
        // Same stdin-draining requirement as above (marker/launcher upload).
        ("write_file", 0, None, true)
    } else if remote_cmd.starts_with("cat ") && remote_cmd.trim_end().ends_with("|| true") {
        (
            "read_marker",
            0,
            Some(env::var("FAKE_SSH_MARKER_CONTENT").unwrap_or_default()),
            false,
        )
    } else if remote_cmd.starts_with("rm -rf") {
        ("rm_rf", env_i32("FAKE_SSH_RM_EXIT", 0), None, false)
    } else {
        // Fail loudly on an unrecognized shape rather than silently
        // succeeding — an unhandled command means the fake is out of sync
        // with `transport.rs` and a test result would otherwise be a false
        // pass/fail for the wrong reason.
        ("unknown", 1, None, false)
    }
}

fn env_i32(key: &str, default: i32) -> i32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn drain_stdin_capped() -> (usize, String) {
    let mut buf = Vec::new();
    let _ = io::stdin().read_to_end(&mut buf);
    let preview_len = buf.len().min(STDIN_PREVIEW_CAP);
    let preview = String::from_utf8_lossy(&buf[..preview_len]).to_string();
    (buf.len(), preview)
}

fn log_invocation(shape: &str, argv: &[String], stdin_len: usize, stdin_preview: &str) {
    let Ok(log_path) = env::var("FAKE_SSH_LOG") else {
        return;
    };
    let invocation = Invocation {
        shape,
        argv,
        stdin_len,
        stdin_preview,
    };
    let Ok(line) = serde_json::to_string(&invocation) else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = writeln!(f, "{line}");
    }
}
