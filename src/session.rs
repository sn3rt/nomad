use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Returns `$XDG_STATE_HOME/nomad` (or `~/.local/state/nomad`), creating it
/// if necessary.
pub fn state_dir() -> Result<PathBuf> {
    let dir = if let Some(xdg) = env::var_os("XDG_STATE_HOME") {
        PathBuf::from(xdg).join("nomad")
    } else {
        dirs::home_dir()
            .context("cannot determine home directory")?
            .join(".local/state/nomad")
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create state directory {}", dir.display()))?;
    Ok(dir)
}

/// Computes the on-disk path used to remember the remote temp directory for
/// a given profile root + destination + forwarded ssh args, mirroring the
/// bash script's `session_state_file` (sha256 of the newline-joined key).
pub fn state_file_path(
    state_dir: &Path,
    profile_root: &Path,
    ssh_dest: &str,
    ssh_args: &[String],
) -> PathBuf {
    let mut key = String::new();
    key.push_str(&profile_root.to_string_lossy());
    key.push('\n');
    key.push_str(ssh_dest);
    for arg in ssh_args {
        key.push('\n');
        key.push_str(arg);
    }

    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hash = hex_encode(&hasher.finalize());

    state_dir.join(hash)
}

/// Fingerprints the payload contents plus arbitrary profile settings, used
/// to decide whether a reused remote root needs its managed files refreshed.
pub fn fingerprint_payload(
    root: &Path,
    paths: &[PathBuf],
    profile_settings: &str,
) -> Result<String> {
    let mut hasher = Sha256::new();

    let mut sorted = paths.to_vec();
    sorted.sort();

    for rel in &sorted {
        let full = root.join(rel);
        let bytes = std::fs::read(&full)
            .with_context(|| format!("failed to read payload file {}", full.display()))?;
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
    }

    hasher.update(profile_settings.as_bytes());
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Generates an unpredictable-enough session marker used to guard `nomad clean`
/// against deleting a directory it did not create, without needing a true CSPRNG
/// since the remote host is already trusted.
pub fn generate_marker() -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(elapsed.as_nanos().to_le_bytes());
    }
    hex_encode(&hasher.finalize())[..32].to_string()
}

/// What's persisted per (profile root, destination, ssh args) key so a
/// reconnect can reuse the remote temp directory and know whether to refresh it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateRecord {
    pub remote_root: String,
    pub marker: String,
    pub fingerprint: String,
}

pub fn load_state(path: &Path) -> Option<StateRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

pub fn save_state(path: &Path, record: &StateRecord) -> Result<()> {
    let text = toml::to_string(record).context("failed to serialize session state")?;
    std::fs::write(path, text)
        .with_context(|| format!("failed to write session state {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_produce_same_key() {
        let dir = PathBuf::from("/state");
        let root = PathBuf::from("/repo");
        let a = state_file_path(&dir, &root, "host", &["-p".into(), "22".into()]);
        let b = state_file_path(&dir, &root, "host", &["-p".into(), "22".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn different_ssh_args_produce_different_keys() {
        let dir = PathBuf::from("/state");
        let root = PathBuf::from("/repo");
        let a = state_file_path(&dir, &root, "host", &["-p".into(), "22".into()]);
        let b = state_file_path(&dir, &root, "host", &["-p".into(), "2222".into()]);
        assert_ne!(a, b);
    }

    #[test]
    fn different_destinations_produce_different_keys() {
        let dir = PathBuf::from("/state");
        let root = PathBuf::from("/repo");
        let a = state_file_path(&dir, &root, "host-a", &[]);
        let b = state_file_path(&dir, &root, "host-b", &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_changes_when_file_contents_change() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let paths = vec![PathBuf::from("a.txt")];

        let first = fingerprint_payload(tmp.path(), &paths, "settings").unwrap();
        std::fs::write(&file, "two").unwrap();
        let second = fingerprint_payload(tmp.path(), &paths, "settings").unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn fingerprint_changes_when_profile_settings_change() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let paths = vec![PathBuf::from("a.txt")];

        let first = fingerprint_payload(tmp.path(), &paths, "settings-a").unwrap();
        let second = fingerprint_payload(tmp.path(), &paths, "settings-b").unwrap();

        assert_ne!(first, second);
    }
}
