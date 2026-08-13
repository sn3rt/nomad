# nomad

Temporary remote shells carrying your dotfiles over SSH.

`nomad` opens an SSH session to a remote host, ships a payload of tracked
files from a local Git-backed profile (dotfiles, install scripts, version
pins, ...) into a throwaway directory on the remote, and drops you into an
interactive shell configured to use them. Disconnect and the remote copy is
left in place for reuse on the next connection; run `nomad clean` to remove
it.

This is a Rust rewrite of a bash script of the same name originally built
for the [Buoy](https://github.com/sn3rt/buoy) dotfiles repo (see
[`nomad`](./nomad) and [`plan.md`](./plan.md) for the original implementation
and the rewrite plan). It is standalone: any Git-tracked directory can be
used as a profile by pointing a `config.toml` at it — see
[`examples/buoy.config.toml`](./examples/buoy.config.toml) for how Buoy
itself would wire in.

## Usage

```sh
nomad [--waypipe|-wp] [ssh options] host
nomad clean [ssh options] host
nomad --help
```

- `[ssh options]` are forwarded to `ssh` as-is (`-p 2222`, `-J bastion`,
  `-i ~/.ssh/id_ed25519`, ...).
- `--help` / `-h` prints usage and exits — works even with no config file
  present.
- `--waypipe` / `-wp` runs the interactive session through `waypipe` (which
  must be installed locally and on the remote host).
- `nomad clean` removes the remote temp directory tracked for that
  destination and forgets the local session record. It refuses to run
  against a target whose path doesn't look like one `nomad` created, and
  refuses if the remote's session marker no longer matches.
- `nomad` never runs a remote command — it always launches the configured
  shell. Passing a command after `host`, or `--`, is rejected.

Remote temp directories are reused across reconnects: `nomad` remembers the
remote path per (profile root, destination, forwarded ssh args) under
`$XDG_STATE_HOME/nomad`, and only re-streams the payload when its content or
the profile's own settings have changed.

## Configuration

`nomad` reads a single profile from a TOML config file, resolved in this
order:

1. `--config PATH`
2. `$NOMAD_CONFIG`
3. `$XDG_CONFIG_HOME/nomad/config.toml`
4. `~/.config/nomad/config.toml`

See [`examples/buoy.config.toml`](./examples/buoy.config.toml) for a
complete, commented example. In short:

```toml
[profile]
name = "buoy"
root = { root_env = "DOTFILES_DIR" }   # or: root = "/path/to/dotfiles"

[profile.validate]
git = true                              # require root to be a Git worktree
files = [".zshrc"]                      # required files (relative to root)
dirs = [".config"]                      # required directories

[profile.payload]
manifest = "profiles/terminal.links"    # line-based list of tracked paths
extra = ["versions.toml"]               # always-included fixed paths

[profile.environment]
DOTS = "{remote_root}"                  # {remote_root} and {shell} are
PATH = "{remote_root}/.local/bin:$PATH" # substituted per-fragment and
                                         # shell-quoted; surrounding literal
                                         # shell syntax (like `:$PATH`) is
                                         # left to expand remotely.

[profile.directories]
required = ["{remote_root}/.codex"]     # mkdir -p'd before the shell starts

[profile.launchers]
zsh = ["export ZDOTDIR={remote_root}", "exec {shell} -il"]
bash = ["unset ZDOTDIR", "exec {shell} -i"]
```

## Development

A Nix flake + direnv config provide a dev shell with the Rust toolchain,
`rustfmt`, `clippy`, and `openssh`:

```sh
direnv allow    # first time only
cargo build
cargo test --features test-support   # runs the CLI-against-fake-ssh suite too
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
```

`test-support` gates a `fake-ssh` binary (used only by `tests/cli.rs` to
simulate `ssh`/`waypipe` without any real network access) so it's never part
of a normal `cargo build`/release. Plain `cargo test` skips that suite.

Without Nix/direnv, any Rust 2021 toolchain (`rustup`) and a system `ssh` +
`tar` are sufficient — `cargo build`/`cargo test` work the same way. CI
(`.github/workflows/ci.yml`) runs `fmt`, `clippy -D warnings`, and the full
test suite on Linux and macOS for every push/PR.

## Status

This is an in-progress standalone extraction (see [`plan.md`](./plan.md)).
Implemented so far: config loading, profile resolution and payload
packaging, ssh-argument-aware CLI parsing, session state/fingerprinting,
the `OpenSshTransport` (ControlMaster reuse, tar streaming, remote launcher
generation) with guaranteed control-connection cleanup on every error path,
`nomad clean`, `--help`, CLI-level integration tests against a fake
`ssh`/`waypipe`, and CI. Not yet done: real end-to-end integration tests
against a live SSH server, and the actual Buoy repo migration (removing the
bash script and wiring in `.config/nomad/config.toml` there).
