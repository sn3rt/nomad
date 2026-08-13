//! CLI-level integration tests that spawn the real compiled `nomad` binary
//! against a fake `ssh`/`waypipe` on `PATH` (see `src/bin/fake-ssh.rs`).
//! No network access, no real SSH server — everything is driven through
//! argv-shape dispatch and `FAKE_SSH_*` env vars.
//!
//! Requires `cargo test --features test-support` (the whole file compiles
//! away to nothing otherwise, since `fake-ssh` itself is feature-gated and
//! `CARGO_BIN_EXE_fake-ssh` wouldn't be set).
#![cfg(feature = "test-support")]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

struct Fixture {
    profile_root: TempDir,
    _config_dir: TempDir,
    bin_dir: TempDir,
    state_dir: TempDir,
    _home_dir: TempDir,
    _log_dir: TempDir,
    config_path: PathBuf,
    log_path: PathBuf,
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git must be installed to run these tests");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn setup_fixture() -> Fixture {
    let profile_root = tempfile::tempdir().unwrap();
    git(profile_root.path(), &["init", "-q"]);
    git(
        profile_root.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(profile_root.path(), &["config", "user.name", "test"]);
    fs::write(profile_root.path().join(".zshrc"), "").unwrap();
    fs::write(profile_root.path().join("manifest.links"), ".zshrc\n").unwrap();
    git(profile_root.path(), &["add", ".zshrc", "manifest.links"]);

    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[profile]
name = "test"
root = "{root}"

[profile.validate]
git = true
files = [".zshrc"]

[profile.payload]
manifest = "manifest.links"

[profile.environment]
DOTS = "{{remote_root}}"

[profile.launchers]
zsh = ["exec {{shell}} -il"]
bash = ["exec {{shell}} -i"]
"#,
            root = profile_root.path().display()
        ),
    )
    .unwrap();

    let bin_dir = tempfile::tempdir().unwrap();
    let fake_ssh = PathBuf::from(env!("CARGO_BIN_EXE_fake-ssh"));
    symlink(&fake_ssh, bin_dir.path().join("ssh")).unwrap();
    symlink(&fake_ssh, bin_dir.path().join("waypipe")).unwrap();

    let state_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();
    let log_dir = tempfile::tempdir().unwrap();
    let log_path = log_dir.path().join("fake-ssh.log");

    Fixture {
        profile_root,
        _config_dir: config_dir,
        bin_dir,
        state_dir,
        _home_dir: home_dir,
        _log_dir: log_dir,
        config_path,
        log_path,
    }
}

impl Fixture {
    /// A `nomad` invocation pre-wired with the fake ssh/waypipe on PATH,
    /// sandboxed state/home dirs, and `--config` pointed at the fixture
    /// profile (`assert_cmd` always pipes stdin and nothing in the spawned
    /// process tree reads it, so there's no need to close it explicitly).
    /// Scenario-specific ssh args/host, `FAKE_SSH_*` env, and assertions are
    /// added by the caller.
    fn nomad(&self) -> Command {
        let mut cmd = Command::cargo_bin("nomad").unwrap();
        let path = format!(
            "{}:{}",
            self.bin_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("PATH", path)
            .env("XDG_STATE_HOME", self.state_dir.path())
            .env("HOME", self._home_dir.path())
            .env("FAKE_SSH_LOG", &self.log_path)
            .env_remove("NOMAD_CONFIG")
            .arg("--config")
            .arg(&self.config_path);
        cmd
    }

    fn state_dir_path(&self) -> PathBuf {
        self.state_dir.path().join("nomad")
    }

    fn resolved_root(&self) -> PathBuf {
        let (config, _) = nomad_env::load_config(Some(&self.config_path)).unwrap();
        config.profile.resolve_root().unwrap()
    }

    /// Pre-seeds a session state record as if a prior `nomad` run had
    /// already connected to `host` with no forwarded ssh args, using the
    /// real session/profile code so the fixture can't drift from what
    /// `prepare()` itself computes.
    fn seed_state(&self, host: &str, remote_root: &str, marker: &str, matching_fingerprint: bool) {
        let (config, _) = nomad_env::load_config(Some(&self.config_path)).unwrap();
        let root = config.profile.resolve_root().unwrap();
        let state_dir = self.state_dir_path();
        fs::create_dir_all(&state_dir).unwrap();
        let state_path = nomad_env::session::state_file_path(&state_dir, &root, host, &[]);

        let fingerprint = if matching_fingerprint {
            let paths = config.profile.payload_paths(&root).unwrap();
            let settings = format!("{:?}", config.profile.environment);
            nomad_env::session::fingerprint_payload(&root, &paths, &settings).unwrap()
        } else {
            "stale-fingerprint".to_string()
        };

        nomad_env::session::save_state(
            &state_path,
            &nomad_env::session::StateRecord {
                remote_root: remote_root.to_string(),
                marker: marker.to_string(),
                fingerprint,
            },
        )
        .unwrap();
    }

    fn state_path_for(&self, host: &str) -> PathBuf {
        let root = self.resolved_root();
        nomad_env::session::state_file_path(&self.state_dir_path(), &root, host, &[])
    }

    fn read_log(&self) -> Vec<serde_json::Value> {
        match fs::read_to_string(&self.log_path) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str(l).expect("fake-ssh log line is valid JSON"))
                .collect(),
            Err(_) => vec![],
        }
    }
}

fn argv_of(entry: &serde_json::Value) -> Vec<String> {
    serde_json::from_value(entry["argv"].clone()).unwrap()
}

#[test]
fn fresh_connect_happy_path() {
    let fx = setup_fixture();

    fx.nomad()
        .env("FAKE_SSH_MKTEMP_OUTPUT", "/tmp/nomad.fake123")
        .env("FAKE_SSH_REMOTE_SHELL", "/bin/zsh")
        .args(["-p", "2222", "host"])
        .assert()
        .success();

    let log = fx.read_log();
    let control_open = log
        .iter()
        .find(|e| e["shape"] == "control_open")
        .expect("control_open should be logged");
    let argv = argv_of(control_open);
    assert!(
        argv.windows(2).any(|w| w == ["-p", "2222"]),
        "forwarded ssh args should reach control_open: {argv:?}"
    );
    assert!(log.iter().any(|e| e["shape"] == "tar_extract"));

    let entries: Vec<_> = fs::read_dir(fx.state_dir_path()).unwrap().collect();
    assert_eq!(entries.len(), 1, "a session state file should be created");
}

#[test]
fn reconnect_reuses_remote_root_without_restreaming_payload() {
    let fx = setup_fixture();
    fx.seed_state("host", "/tmp/nomad.cached", "cached-marker", true);

    fx.nomad()
        .env("FAKE_SSH_TEST_D_EXIT", "0") // simulate remote dir + marker still present
        .env("FAKE_SSH_REMOTE_SHELL", "/bin/zsh")
        .arg("host")
        .assert()
        .success();

    let log = fx.read_log();
    assert!(
        !log.iter().any(|e| e["shape"] == "tar_extract"),
        "payload should not be re-streamed for an unchanged, still-present remote root"
    );
}

#[test]
fn interactive_exit_code_propagates() {
    let fx = setup_fixture();

    fx.nomad()
        .env("FAKE_SSH_MKTEMP_OUTPUT", "/tmp/nomad.fake")
        .env("FAKE_SSH_REMOTE_SHELL", "/bin/zsh")
        .env("FAKE_SSH_INTERACTIVE_EXIT", "3")
        .arg("host")
        .assert()
        .code(3);
}

#[test]
fn clean_removes_matching_remote_root_and_local_state() {
    let fx = setup_fixture();
    fx.seed_state("host", "/tmp/nomad.cleanme", "marker-xyz", true);
    let state_path = fx.state_path_for("host");
    assert!(state_path.exists());

    fx.nomad()
        .env("FAKE_SSH_MARKER_CONTENT", "marker-xyz")
        .args(["clean", "host"])
        .assert()
        .success();

    assert!(
        !state_path.exists(),
        "local session record should be cleared"
    );
    let log = fx.read_log();
    assert!(log.iter().any(|e| e["shape"] == "rm_rf"));
}

#[test]
fn clean_refuses_on_marker_mismatch() {
    let fx = setup_fixture();
    fx.seed_state("host", "/tmp/nomad.mismatched", "expected-marker", true);
    let state_path = fx.state_path_for("host");

    fx.nomad()
        .env("FAKE_SSH_MARKER_CONTENT", "different-marker")
        .args(["clean", "host"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("marker mismatch"));

    let log = fx.read_log();
    assert!(
        !log.iter().any(|e| e["shape"] == "rm_rf"),
        "must not delete the remote directory on a marker mismatch"
    );
    assert!(
        state_path.exists(),
        "local state must be left alone on failure"
    );
}

#[test]
fn bad_usage_missing_host_exits_nonzero_with_usage() {
    let fx = setup_fixture();

    fx.nomad()
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: nomad"));
}

#[test]
fn control_connection_closes_when_remote_shell_resolution_fails() {
    let fx = setup_fixture();

    fx.nomad()
        .env("FAKE_SSH_MKTEMP_OUTPUT", "/tmp/nomad.fake")
        .env("FAKE_SSH_REMOTE_SHELL", "") // neither zsh nor bash "installed"
        .arg("host")
        .assert()
        .failure()
        .stderr(predicate::str::contains("neither zsh nor bash"));

    let log = fx.read_log();
    assert!(log.iter().any(|e| e["shape"] == "control_open"));
    assert!(
        log.iter().any(|e| e["shape"] == "control_close"),
        "the control connection must be closed even when prepare() fails mid-setup"
    );
}

#[test]
fn waypipe_mode_invokes_waypipe_then_ssh() {
    let fx = setup_fixture();

    fx.nomad()
        .env("FAKE_SSH_MKTEMP_OUTPUT", "/tmp/nomad.fake")
        .env("FAKE_SSH_REMOTE_SHELL", "/bin/zsh")
        .args(["--waypipe", "host"])
        .assert()
        .success();

    let log = fx.read_log();
    let entry = log
        .iter()
        .find(|e| e["shape"] == "waypipe_interactive")
        .expect("a waypipe interactive invocation should be logged");
    let argv = argv_of(entry);
    assert_eq!(argv[0], "waypipe");
    assert_eq!(argv[1], "ssh");
}

// Sanity check on the fixture itself, independent of `nomad`: proves
// `profile_root`/`bin_dir` are actually wired the way the other tests assume.
#[test]
fn fixture_profile_root_is_a_valid_git_worktree() {
    let fx = setup_fixture();
    assert!(fx.profile_root.path().join(".git").is_dir());
    assert!(fx.bin_dir.path().join("ssh").exists());
}
