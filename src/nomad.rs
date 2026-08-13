use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::{bail, Context, Result};

use crate::destination::Destination;
use crate::profile::Profile;
use crate::session::{self, StateRecord};
use crate::transport::Transport;

pub struct Nomad<'a, T: Transport> {
    pub transport: &'a T,
    pub profile: &'a Profile,
}

pub struct PreparedSession {
    pub socket: PathBuf,
    pub remote_root: String,
    pub remote_shell_name: String,
    pub state_path: PathBuf,
}

impl<'a, T: Transport> Nomad<'a, T> {
    pub fn new(transport: &'a T, profile: &'a Profile) -> Self {
        Self { transport, profile }
    }

    /// Opens the control connection, ensures a remote temp directory exists
    /// (reusing and refreshing one from a previous session when possible),
    /// and writes the remote launcher script. Mirrors the bash script's
    /// `run_remote_shell` setup phase up to (but not including) the
    /// interactive session itself.
    pub fn prepare(&self, dest: &Destination, root: &std::path::Path) -> Result<PreparedSession> {
        let marker_prefix = session::generate_marker();
        let socket = std::env::temp_dir().join(format!("nomad-socket.{}", &marker_prefix[..12]));

        self.transport
            .control_open(&dest.host, &dest.ssh_args, &socket)
            .context("failed to open control connection")?;

        let state_dir = session::state_dir()?;
        let state_path = session::state_file_path(&state_dir, root, &dest.host, &dest.ssh_args);
        let existing = session::load_state(&state_path);

        let paths = self.profile.payload_paths(root)?;
        let settings = format!("{:?}", self.profile.environment);
        let fingerprint = session::fingerprint_payload(root, &paths, &settings)?;

        let mut remote_root = None;
        let mut marker = None;

        if let Some(record) = &existing {
            let alive = self
                .transport
                .remote_status(
                    &dest.host,
                    &dest.ssh_args,
                    &socket,
                    &format!(
                        "test -d {} && test -f {}",
                        crate::transport::shell_quote(&record.remote_root),
                        crate::transport::shell_quote(&format!(
                            "{}/.nomad-marker",
                            record.remote_root
                        )),
                    ),
                )
                .unwrap_or(false);

            if alive {
                remote_root = Some(record.remote_root.clone());
                marker = Some(record.marker.clone());
            }
        }

        let needs_payload = remote_root.is_none()
            || existing
                .as_ref()
                .map(|r| r.fingerprint != fingerprint)
                .unwrap_or(true);

        let remote_root = match remote_root {
            Some(r) => r,
            None => self
                .transport
                .remote_capture(
                    &dest.host,
                    &dest.ssh_args,
                    &socket,
                    "mktemp -d \"${TMPDIR:-/tmp}/nomad.XXXXXXXX\"",
                )
                .context("failed to create remote temp directory")?,
        };
        let marker = marker.unwrap_or_else(session::generate_marker);

        if needs_payload {
            self.transport
                .stream_tar(
                    &dest.host,
                    &dest.ssh_args,
                    &socket,
                    root,
                    &paths,
                    &remote_root,
                )
                .context("failed to stream dotfiles payload")?;

            self.transport
                .remote_write_file(
                    &dest.host,
                    &dest.ssh_args,
                    &socket,
                    &format!("{remote_root}/.nomad-marker"),
                    marker.as_bytes(),
                    false,
                )
                .context("failed to write session marker")?;

            session::save_state(
                &state_path,
                &StateRecord {
                    remote_root: remote_root.clone(),
                    marker: marker.clone(),
                    fingerprint,
                },
            )?;
        }

        let remote_shell = self
            .transport
            .remote_capture(
                &dest.host,
                &dest.ssh_args,
                &socket,
                "command -v zsh 2>/dev/null || command -v bash 2>/dev/null",
            )
            .context("failed to resolve remote shell")?;
        if remote_shell.is_empty() {
            bail!("neither zsh nor bash is installed on the remote host");
        }
        let remote_shell_name = remote_shell
            .rsplit('/')
            .next()
            .unwrap_or(&remote_shell)
            .to_string();
        if remote_shell_name != "zsh" && remote_shell_name != "bash" {
            bail!("unsupported remote shell resolved: {remote_shell}");
        }

        let launcher = render_launcher(
            self.profile,
            &remote_root,
            &remote_shell,
            &remote_shell_name,
        );
        self.transport
            .remote_write_file(
                &dest.host,
                &dest.ssh_args,
                &socket,
                &format!("{remote_root}/.nomad-shell"),
                launcher.as_bytes(),
                true,
            )
            .context("failed to write remote launcher")?;

        Ok(PreparedSession {
            socket,
            remote_root,
            remote_shell_name,
            state_path,
        })
    }

    /// Launches the interactive remote shell and returns its exit status.
    pub fn enter(
        &self,
        dest: &Destination,
        session: &PreparedSession,
        use_waypipe: bool,
    ) -> Result<ExitStatus> {
        let remote_launcher = format!("{}/.nomad-shell", session.remote_root);
        let status = self.transport.interactive(
            &dest.host,
            &dest.ssh_args,
            &session.socket,
            &remote_launcher,
            use_waypipe,
        )?;
        self.transport
            .control_close(&dest.host, &dest.ssh_args, &session.socket);
        Ok(status)
    }

    /// Removes the remote temp root for `dest`, guarding against unsafe
    /// deletion targets and marker mismatches, and clears local session state.
    pub fn clean(&self, dest: &Destination, root: &std::path::Path) -> Result<()> {
        let state_dir = session::state_dir()?;
        let state_path = session::state_file_path(&state_dir, root, &dest.host, &dest.ssh_args);
        let record = session::load_state(&state_path)
            .with_context(|| "no active nomad session found for this destination")?;

        validate_deletion_target(&record.remote_root)?;

        let marker_prefix = session::generate_marker();
        let socket = std::env::temp_dir().join(format!("nomad-clean.{}", &marker_prefix[..12]));
        self.transport
            .control_open(&dest.host, &dest.ssh_args, &socket)
            .context("failed to open control connection")?;

        let marker_matches = self
            .transport
            .remote_capture(
                &dest.host,
                &dest.ssh_args,
                &socket,
                &format!(
                    "cat {} 2>/dev/null || true",
                    crate::transport::shell_quote(&format!("{}/.nomad-marker", record.remote_root)),
                ),
            )
            .unwrap_or_default();

        if marker_matches.trim() != record.marker {
            self.transport
                .control_close(&dest.host, &dest.ssh_args, &socket);
            bail!(
                "refusing to remove {}: session marker mismatch",
                record.remote_root
            );
        }

        let removed = self
            .transport
            .remote_status(
                &dest.host,
                &dest.ssh_args,
                &socket,
                &format!(
                    "rm -rf -- {}",
                    crate::transport::shell_quote(&record.remote_root)
                ),
            )
            .context("failed to remove remote temp directory")?;

        self.transport
            .control_close(&dest.host, &dest.ssh_args, &socket);

        if !removed {
            bail!("failed to remove remote directory {}", record.remote_root);
        }

        let _ = std::fs::remove_file(&state_path);
        Ok(())
    }
}

/// Rejects empty, relative, `$HOME`, `/`, and otherwise unsafe-looking
/// deletion targets before `nomad clean` is allowed to `rm -rf` them.
fn validate_deletion_target(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        bail!("refusing to clean an empty remote path");
    }
    if !path.starts_with('/') {
        bail!("refusing to clean a relative remote path: {path}");
    }
    if path == "/" {
        bail!("refusing to clean the remote root filesystem");
    }
    if !path.contains("nomad") {
        bail!("refusing to clean a path that doesn't look like a nomad temp dir: {path}");
    }
    Ok(())
}

fn render_launcher(
    profile: &Profile,
    remote_root: &str,
    remote_shell: &str,
    remote_shell_name: &str,
) -> String {
    let mut vars: BTreeMap<&str, String> = BTreeMap::new();
    vars.insert("remote_root", remote_root.to_string());
    vars.insert("shell", remote_shell.to_string());

    let mut script = String::from("#!/usr/bin/env sh\n");

    for (key, template) in &profile.environment {
        let value = Profile::render(template, &vars);
        script.push_str(&format!(
            "export {key}={}\n",
            crate::transport::shell_quote(&value)
        ));
    }

    for dir_template in &profile.directories.required {
        let dir = Profile::render(dir_template, &vars);
        script.push_str(&format!(
            "mkdir -p {} || exit 1\n",
            crate::transport::shell_quote(&dir)
        ));
    }

    let launcher_lines = match remote_shell_name {
        "zsh" => &profile.launchers.zsh,
        _ => &profile.launchers.bash,
    };
    for line in launcher_lines {
        script.push_str(&Profile::render(line, &vars));
        script.push('\n');
    }

    script.push_str("nomad_status=$?\nexit $nomad_status\n");
    script
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Directories, Launchers, Payload, RootSpec, Validate};

    fn test_profile() -> Profile {
        Profile {
            name: "test".into(),
            root: RootSpec::Path("/tmp".into()),
            validate: Validate::default(),
            payload: Payload {
                manifest: None,
                extra: vec![],
            },
            environment: BTreeMap::from([("DOTS".to_string(), "{remote_root}".to_string())]),
            directories: Directories { required: vec![] },
            launchers: Launchers {
                zsh: vec!["{shell} -il".to_string()],
                bash: vec!["{shell} -i".to_string()],
            },
        }
    }

    #[test]
    fn launcher_substitutes_placeholders() {
        let profile = test_profile();
        let script = render_launcher(&profile, "/tmp/nomad.abc", "/usr/bin/zsh", "zsh");
        assert!(script.contains("export DOTS='/tmp/nomad.abc'"));
        assert!(script.contains("/usr/bin/zsh -il"));
    }

    #[test]
    fn validate_deletion_target_rejects_unsafe_paths() {
        assert!(validate_deletion_target("").is_err());
        assert!(validate_deletion_target("relative/path").is_err());
        assert!(validate_deletion_target("/").is_err());
        assert!(validate_deletion_target("/home/user").is_err());
        assert!(validate_deletion_target("/tmp/nomad.abc123").is_ok());
    }
}
