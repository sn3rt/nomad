use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub name: String,
    pub root: RootSpec,
    #[serde(default)]
    pub validate: Validate,
    pub payload: Payload,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub directories: Directories,
    pub launchers: Launchers,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RootSpec {
    Path(PathBuf),
    Env { root_env: String },
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Validate {
    #[serde(default = "default_true")]
    pub git: bool,
    #[serde(default)]
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub dirs: Vec<PathBuf>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct Payload {
    /// Line-based manifest file (relative to repo root), one tracked path per line.
    /// Lines starting with `#` and blank lines are ignored.
    #[serde(default)]
    pub manifest: Option<PathBuf>,
    /// Additional fixed paths always included in the payload.
    #[serde(default)]
    pub extra: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Directories {
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Launchers {
    pub zsh: Vec<String>,
    pub bash: Vec<String>,
}

impl Profile {
    /// Resolves and validates the repo root against this profile's `validate` rules,
    /// mirroring the bash script's `resolve_repo_root`.
    pub fn resolve_root(&self) -> Result<PathBuf> {
        let root = match &self.root {
            RootSpec::Path(p) => p.clone(),
            RootSpec::Env { root_env } => {
                let value = std::env::var(root_env)
                    .with_context(|| format!("environment variable {root_env} is not set"))?;
                PathBuf::from(value)
            }
        };

        let root = root
            .canonicalize()
            .with_context(|| format!("profile root does not exist: {}", root.display()))?;

        if let Some(home) = dirs::home_dir() {
            if root == home {
                bail!("refusing to use {} as the profile root", home.display());
            }
        }

        for rel in &self.validate.files {
            if !root.join(rel).is_file() {
                bail!(
                    "profile root does not look valid: {} (missing file {})",
                    root.display(),
                    rel.display()
                );
            }
        }
        for rel in &self.validate.dirs {
            if !root.join(rel).is_dir() {
                bail!(
                    "profile root does not look valid: {} (missing directory {})",
                    root.display(),
                    rel.display()
                );
            }
        }

        if self.validate.git {
            let status = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "--is-inside-work-tree"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .context("git must be installed locally")?;
            if !status.success() {
                bail!(
                    "profile root must be a Git working tree: {}",
                    root.display()
                );
            }
        }

        Ok(root)
    }

    /// Builds the full list of repo-relative payload paths, mirroring the bash
    /// script's `terminal_payload`: manifest entries (validated as Git-tracked
    /// when `validate.git` is set) followed by the fixed extra paths.
    pub fn payload_paths(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();

        if let Some(manifest) = &self.payload.manifest {
            let manifest_path = root.join(manifest);
            let text = std::fs::read_to_string(&manifest_path).with_context(|| {
                format!(
                    "failed to read payload manifest {}",
                    manifest_path.display()
                )
            })?;

            for line in text.lines() {
                let item = line.trim();
                if item.is_empty() || item.starts_with('#') {
                    continue;
                }

                if self.validate.git {
                    let output = Command::new("git")
                        .arg("-C")
                        .arg(root)
                        .args(["ls-files", "--"])
                        .arg(item)
                        .output()
                        .context("git must be installed locally")?;
                    let tracked = String::from_utf8_lossy(&output.stdout);
                    if tracked.trim().is_empty() {
                        bail!("payload manifest entry is not tracked: {item}");
                    }
                    for tracked_path in tracked.lines() {
                        paths.push(PathBuf::from(tracked_path));
                    }
                } else {
                    paths.push(PathBuf::from(item));
                }
            }
        }

        paths.extend(self.payload.extra.iter().cloned());
        Ok(paths)
    }

    /// Renders an environment/launcher template string, substituting the
    /// supported `{placeholder}` tokens with their shell-quoted values.
    ///
    /// Only the substituted fragments are quoted (mirroring the bash
    /// script's per-value `printf '%q'`) so that literal shell syntax left
    /// in the template around them — e.g. a trailing `:$PATH` — still
    /// expands at remote export time instead of being neutralized by an
    /// outer quote wrapping the whole value.
    pub fn render(template: &str, vars: &BTreeMap<&str, String>) -> String {
        let mut out = template.to_string();
        for (key, value) in vars {
            out = out.replace(&format!("{{{key}}}"), &crate::transport::shell_quote(value));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn base_profile() -> Profile {
        Profile {
            name: "test".into(),
            root: RootSpec::Path("/tmp".into()),
            validate: Validate {
                git: false,
                files: vec![],
                dirs: vec![],
            },
            payload: Payload {
                manifest: None,
                extra: vec![],
            },
            environment: BTreeMap::new(),
            directories: Directories { required: vec![] },
            launchers: Launchers {
                zsh: vec![],
                bash: vec![],
            },
        }
    }

    fn init_git_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
    }

    #[test]
    fn render_substitutes_known_placeholders() {
        let mut vars = BTreeMap::new();
        vars.insert("remote_root", "/tmp/nomad.abc".to_string());
        vars.insert("shell", "/usr/bin/zsh".to_string());
        assert_eq!(
            Profile::render("export DOTS={remote_root}; exec {shell} -il", &vars),
            "export DOTS='/tmp/nomad.abc'; exec '/usr/bin/zsh' -il"
        );
    }

    #[test]
    fn render_leaves_unknown_placeholders_untouched() {
        let vars = BTreeMap::new();
        assert_eq!(Profile::render("{unknown}", &vars), "{unknown}");
    }

    #[test]
    fn resolve_root_rejects_home_directory() {
        let mut profile = base_profile();
        profile.root = RootSpec::Path(dirs::home_dir().unwrap());
        let err = profile.resolve_root().unwrap_err();
        assert!(err.to_string().contains("refusing to use"));
    }

    #[test]
    fn resolve_root_rejects_missing_validation_files() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let mut profile = base_profile();
        profile.root = RootSpec::Path(tmp.path().to_path_buf());
        profile.validate.git = true;
        profile.validate.files = vec![PathBuf::from(".zshrc")];

        let err = profile.resolve_root().unwrap_err();
        assert!(err.to_string().contains("does not look valid"));
    }

    #[test]
    fn resolve_root_accepts_valid_git_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        std::fs::write(tmp.path().join(".zshrc"), "").unwrap();

        let mut profile = base_profile();
        profile.root = RootSpec::Path(tmp.path().to_path_buf());
        profile.validate.git = true;
        profile.validate.files = vec![PathBuf::from(".zshrc")];

        let resolved = profile.resolve_root().unwrap();
        assert_eq!(resolved, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn payload_paths_rejects_untracked_manifest_entries() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        std::fs::write(tmp.path().join("terminal.links"), "not-tracked.txt\n").unwrap();

        let mut profile = base_profile();
        profile.validate.git = true;
        profile.payload.manifest = Some(PathBuf::from("terminal.links"));

        let err = profile.payload_paths(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("not tracked"));
    }

    #[test]
    fn payload_paths_includes_tracked_manifest_entries_and_extras() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        std::fs::write(tmp.path().join(".zshrc"), "").unwrap();
        std::fs::write(tmp.path().join("terminal.links"), "# comment\n.zshrc\n\n").unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", ".zshrc", "terminal.links"])
            .status()
            .unwrap();
        assert!(status.success());

        let mut profile = base_profile();
        profile.validate.git = true;
        profile.payload.manifest = Some(PathBuf::from("terminal.links"));
        profile.payload.extra = vec![PathBuf::from("versions.toml")];

        let paths = profile.payload_paths(tmp.path()).unwrap();
        assert_eq!(
            paths,
            vec![PathBuf::from(".zshrc"), PathBuf::from("versions.toml")]
        );
    }

    #[test]
    fn payload_paths_skips_git_check_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("terminal.links"), "some/file\n").unwrap();

        let mut profile = base_profile();
        profile.validate.git = false;
        profile.payload.manifest = Some(PathBuf::from("terminal.links"));

        let paths = profile.payload_paths(tmp.path()).unwrap();
        assert_eq!(paths, vec![PathBuf::from("some/file")]);
    }
}
