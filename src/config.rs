use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::profile::Profile;

#[derive(Debug)]
pub enum ConfigError {
    NotFound {
        tried: Vec<PathBuf>,
    },
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::NotFound { tried } => {
                write!(f, "no config file found, tried: ")?;
                for (i, p) in tried.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p.display())?;
                }
                Ok(())
            }
            ConfigError::Read { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Resolves the config file path in precedence order:
/// 1. `--config PATH` (explicit CLI flag)
/// 2. `NOMAD_CONFIG` environment variable
/// 3. `$XDG_CONFIG_HOME/nomad/config.toml`
/// 4. `~/.config/nomad/config.toml`
pub fn resolve_config_path(cli_path: Option<&Path>) -> Result<PathBuf, ConfigError> {
    let mut tried = Vec::new();

    if let Some(p) = cli_path {
        return Ok(p.to_path_buf());
    }

    if let Ok(p) = env::var("NOMAD_CONFIG") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }

    if let Some(xdg_config) = env::var_os("XDG_CONFIG_HOME") {
        let candidate = PathBuf::from(xdg_config).join("nomad/config.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".config/nomad/config.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    Err(ConfigError::NotFound { tried })
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Config {
    pub profile: Profile,
}

pub fn load_config(cli_path: Option<&Path>) -> Result<(Config, PathBuf), ConfigError> {
    let path = resolve_config_path(cli_path)?;
    let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.clone(),
        source,
    })?;
    Ok((config, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn cli_path_wins_over_everything() {
        let _guard = ENV_LOCK.lock().unwrap();
        let explicit = PathBuf::from("/explicit/config.toml");
        let resolved = resolve_config_path(Some(&explicit)).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn env_var_wins_over_xdg_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("NOMAD_CONFIG", "/env/config.toml");
        let resolved = resolve_config_path(None).unwrap();
        env::remove_var("NOMAD_CONFIG");
        assert_eq!(resolved, PathBuf::from("/env/config.toml"));
    }

    #[test]
    fn missing_config_lists_tried_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::remove_var("NOMAD_CONFIG");
        let tmp_home = tempfile::tempdir().unwrap();
        let prev_xdg = env::var_os("XDG_CONFIG_HOME");
        env::remove_var("XDG_CONFIG_HOME");
        let prev_home = env::var_os("HOME");
        env::set_var("HOME", tmp_home.path());

        let err = resolve_config_path(None).unwrap_err();
        match err {
            ConfigError::NotFound { tried } => assert!(!tried.is_empty()),
            _ => panic!("expected NotFound"),
        }

        if let Some(v) = prev_xdg {
            env::set_var("XDG_CONFIG_HOME", v);
        }
        if let Some(v) = prev_home {
            env::set_var("HOME", v);
        }
    }
}
