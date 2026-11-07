//! The settings of the daemon, read from `config.toml`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;

/// Where the daemon works and for whom.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub email: String,
    pub maildir: PathBuf,
}

/// The place of `config.toml` for the current user.
///
/// # Errors
///
/// Fails when the home directory cannot be found.
pub fn default_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "tutad").context("cannot find the home directory")?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// The directory for the session and the record of synced mail.
///
/// # Errors
///
/// Fails when neither systemd nor the home directory gives a place.
pub fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("STATE_DIRECTORY") {
        return Ok(PathBuf::from(dir));
    }
    let dirs = ProjectDirs::from("", "", "tutad").context("cannot find the home directory")?;
    Ok(dirs
        .state_dir()
        .unwrap_or_else(|| dirs.data_local_dir())
        .to_path_buf())
}

/// Reads the settings from a file.
///
/// # Errors
///
/// Fails when the file is missing or is not valid settings.
pub fn load(path: &Path) -> Result<Config> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "email = \"a@tuta.com\"\nmaildir = \"/var/lib/tutad\"\n",
        )
        .unwrap();
        let config = load(&path).unwrap();
        assert_eq!(config.email, "a@tuta.com");
        assert_eq!(config.maildir, PathBuf::from("/var/lib/tutad"));
    }

    #[test]
    fn unknown_keys_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "email = \"a\"\nmaildir = \"/m\"\nfoo = 1\n").unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn a_missing_file_is_an_error() {
        assert!(load(Path::new("/nonexistent/config.toml")).is_err());
    }
}
