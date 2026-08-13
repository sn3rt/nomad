use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{bail, Context, Result};

/// A remote SSH connection abstraction. `OpenSshTransport` is the production
/// implementation shelling out to the system `ssh`/`tar`; tests can
/// substitute a fake to drive the orchestration logic in [`crate::nomad::Nomad`]
/// without a real network.
pub trait Transport {
    /// Opens a background SSH ControlMaster socket to `dest`.
    fn control_open(&self, dest: &str, ssh_args: &[String], socket: &Path) -> Result<()>;

    /// Closes a previously opened ControlMaster socket. Best-effort.
    fn control_close(&self, dest: &str, ssh_args: &[String], socket: &Path);

    /// Runs `remote_cmd` over the control socket and returns whether it exited successfully.
    fn remote_status(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_cmd: &str,
    ) -> Result<bool>;

    /// Runs `remote_cmd` over the control socket and returns its captured stdout.
    fn remote_capture(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_cmd: &str,
    ) -> Result<String>;

    /// Writes `contents` to `remote_path` on the remote host and marks it executable
    /// when `executable` is set.
    fn remote_write_file(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_path: &str,
        contents: &[u8],
        executable: bool,
    ) -> Result<()>;

    /// Streams a tar archive of `paths` (relative to `local_root`) to `remote_dir`
    /// on the remote host, extracting it there.
    fn stream_tar(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        local_root: &Path,
        paths: &[PathBuf],
        remote_dir: &str,
    ) -> Result<()>;

    /// Launches an interactive TTY session running `remote_command`, inheriting
    /// stdio, and returns its exit status.
    fn interactive(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_command: &str,
        use_waypipe: bool,
    ) -> Result<ExitStatus>;
}

pub struct OpenSshTransport;

impl OpenSshTransport {
    fn ssh_command(&self, socket: &Path, dest: &str, ssh_args: &[String]) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-S").arg(socket);
        cmd.arg("-o")
            .arg(format!("ControlPath={}", socket.display()));
        cmd.args(ssh_args);
        cmd.arg(dest);
        cmd
    }
}

impl Transport for OpenSshTransport {
    fn control_open(&self, dest: &str, ssh_args: &[String], socket: &Path) -> Result<()> {
        let status = Command::new("ssh")
            .arg("-MNf")
            .arg("-o")
            .arg("ControlMaster=yes")
            .arg("-o")
            .arg("ControlPersist=yes")
            .arg("-o")
            .arg(format!("ControlPath={}", socket.display()))
            .args(ssh_args)
            .arg(dest)
            .status()
            .context("failed to launch ssh")?;
        if !status.success() {
            bail!("ssh control connection to {dest} failed");
        }
        Ok(())
    }

    fn control_close(&self, dest: &str, ssh_args: &[String], socket: &Path) {
        let _ = self
            .ssh_command(socket, dest, ssh_args)
            .arg("-O")
            .arg("exit")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn remote_status(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_cmd: &str,
    ) -> Result<bool> {
        let status = self
            .ssh_command(socket, dest, ssh_args)
            .arg(remote_cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to run remote command")?;
        Ok(status.success())
    }

    fn remote_capture(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_cmd: &str,
    ) -> Result<String> {
        let output = self
            .ssh_command(socket, dest, ssh_args)
            .arg(remote_cmd)
            .output()
            .context("failed to run remote command")?;
        if !output.status.success() {
            bail!(
                "remote command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    }

    fn remote_write_file(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_path: &str,
        contents: &[u8],
        executable: bool,
    ) -> Result<()> {
        let remote_cmd = if executable {
            format!(
                "cat > {} && chmod +x {}",
                shell_quote(remote_path),
                shell_quote(remote_path)
            )
        } else {
            format!("cat > {}", shell_quote(remote_path))
        };

        let mut child = self
            .ssh_command(socket, dest, ssh_args)
            .arg(remote_cmd)
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to spawn ssh for remote file write")?;

        child
            .stdin
            .take()
            .context("missing stdin handle")?
            .write_all(contents)
            .context("failed to stream file contents to remote host")?;

        let status = child.wait().context("failed to wait for ssh")?;
        if !status.success() {
            bail!("failed to write remote file {remote_path}");
        }
        Ok(())
    }

    fn stream_tar(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        local_root: &Path,
        paths: &[PathBuf],
        remote_dir: &str,
    ) -> Result<()> {
        let mut tar = Command::new("tar")
            .arg("-C")
            .arg(local_root)
            .arg("-cf")
            .arg("-")
            .args(paths)
            .stdout(Stdio::piped())
            .spawn()
            .context("failed to spawn tar")?;

        let tar_stdout = tar.stdout.take().context("missing tar stdout")?;

        let remote_cmd = format!("tar -C {} -xf -", shell_quote(remote_dir));
        let mut ssh = self
            .ssh_command(socket, dest, ssh_args)
            .arg(remote_cmd)
            .stdin(Stdio::from(tar_stdout))
            .spawn()
            .context("failed to spawn ssh for tar stream")?;

        let tar_status = tar.wait().context("failed to wait for tar")?;
        let ssh_status = ssh.wait().context("failed to wait for ssh")?;

        if !tar_status.success() {
            bail!("tar failed while packaging payload");
        }
        if !ssh_status.success() {
            bail!("failed to extract payload on remote host");
        }
        Ok(())
    }

    fn interactive(
        &self,
        dest: &str,
        ssh_args: &[String],
        socket: &Path,
        remote_command: &str,
        use_waypipe: bool,
    ) -> Result<ExitStatus> {
        let mut cmd = if use_waypipe {
            let mut c = Command::new("waypipe");
            c.arg("ssh");
            c
        } else {
            Command::new("ssh")
        };

        cmd.arg("-tt");
        cmd.arg("-S").arg(socket);
        cmd.arg("-o")
            .arg(format!("ControlPath={}", socket.display()));
        cmd.args(ssh_args);
        cmd.arg(dest);
        cmd.arg(remote_command);

        cmd.status()
            .context("failed to launch interactive ssh session")
    }
}

/// POSIX shell single-quoting, mirroring the bash script's `printf '%q'` usage
/// for remote paths embedded in shell command strings.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_plain_values() {
        assert_eq!(shell_quote("/tmp/foo"), "'/tmp/foo'");
    }

    #[test]
    fn shell_quote_escapes_embedded_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
