# Extract Nomad into a Standalone Rust Tool

  ## Summary

  This is a moderate rewrite: several focused days for parity and roughly one to two weeks for a polished public v0.1.

  - Create a fresh public repository at github.com/sn3rt/nomad.
  - Use one Cargo package named nomad-env, containing:
      - Library crate imported as nomad_env.
      - Executable named nomad.

  - Keep the package unpublished with publish = false; distribute binaries through GitHub Releases only.
  - Add no license initially. Document that third-party reuse and redistribution remain deferred until a license is selected.
  - Use blocking Rust APIs and the system OpenSSH client on Linux/macOS. Do not implement SSH itself.

  ## Standalone Implementation

  - Expose a reusable library centered around:
      - Profile: payload, environment, directories, launchers, and refresh policy.
      - Destination: SSH destination and forwarded SSH arguments.
      - Nomad<T: Transport>: prepare, enter, and clean.
      - Session: resolved remote root, profile identity, and payload fingerprint.
      - OpenSshTransport: production transport, with a trait allowing tests or other tools to substitute one.

  - Read configuration in this precedence order:
      1. --config PATH
      2. NOMAD_CONFIG
      3. $XDG_CONFIG_HOME/nomad/config.toml
      4. ~/.config/nomad/config.toml

  - Resolve profile-relative paths against the canonical config location. Support explicit files, a line-based manifest, optional Git-tracked validation, environment templates, required directories, and ordered Zsh/Bash launchers.
  - Preserve the existing CLI:
      - nomad [--waypipe|-wp] [ssh options] host
      - nomad clean [ssh options] host
      - Reject remote-command arguments.

  - Inside an active environment, inject a lightweight shell function so nomad clean validates the environment marker, leaves its working directory, removes only that temporary root, and exits. Other nomad ... calls fall through to
    HashiCorp Nomad if installed.

  - Retain OpenSSH config, agents, ProxyJump, hardware keys, ControlMaster reuse, Waypipe support, interactive TTY behavior, and child exit statuses.
  - Build the tar stream in Rust. Require only OpenSSH locally and POSIX sh, tar, and mktemp remotely, plus Zsh or Bash for the configured Buoy profile.
  - Store session mappings under $XDG_STATE_HOME/nomad. Key them by stable profile ID, destination, and SSH arguments.
  - Fingerprint payload contents and relevant profile settings. On reconnect:
      - Reuse the existing remote root.
      - Refresh changed managed files automatically.
      - Remove files dropped from the managed manifest.
      - Preserve history, caches, credentials, and installed tools outside the managed-file list.

  - Guard cleanup with an unpredictable session identifier and marker file. Reject empty, relative, home, root, marker-mismatched, or otherwise unsafe deletion targets.

  ## Buoy Migration

  - Add .config/nomad/config.toml as the Buoy profile, referencing the terminal manifest, extra installer/version files, current environment variables, Zsh setup, and Bash fallback.
  - Keep .config/nomad/bashrc as Buoy-owned profile content, but remove damon.
  - Remove .local/bin/nomad from Buoy and from profiles/terminal.links.
  - Link the Nomad configuration through the terminal profile.
  - Pin Nomad in versions.toml, add sn3rt/nomad to version checking, and install verified release binaries through install-tools.sh.
  - Include Nomad in local terminal installs, but skip installing it inside an active Nomad guest environment.
  - Update the Starship Nomad marker to use the new environment marker and retain the existing visual indicator.
  - Update documentation for configuration, automatic refresh, nomad clean, Waypipe, dependencies, and the intentional HashiCorp executable-name collision.
  - Release standalone v0.1.0 before deleting the Bash implementation, then update Buoy to that exact release.

  ## Testing and Release

  - Unit-test configuration precedence, template expansion, manifest validation, hashing, state keys, SSH argument forwarding, shell quoting, refresh decisions, and deletion guards.
  - Test the CLI against fake ssh and waypipe executables to verify exact arguments, streams, exit codes, and failure messages.
  - Run Linux OpenSSH integration tests covering first deployment, reuse, changed payload refresh, stale-file removal, Zsh/Bash fallback, remote nomad clean, and local nomad clean host.
  - Verify Buoy parity for normal SSH options, Waypipe, Codex isolation, XDG directories, tool installation, and retained state.
  - CI runs formatting, Clippy with warnings denied, unit tests on Linux/macOS, and Linux SSH integration tests.
  - GitHub releases provide checksummed archives for x86_64/aarch64 Linux and Intel/Apple Silicon macOS.
  - Use fresh repository history. Publishing nomad-env to crates.io and granting reuse rights are explicitly deferred until a license and stable library API are chosen.
