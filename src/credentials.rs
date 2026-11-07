//! The secrets systemd hands to the service, so they never sit in the settings.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The Tuta password and the `otpauth://` URI of its second factor.
pub struct Credentials {
    pub password: String,
    pub totp: String,
}

/// The directory systemd put the credentials in.
///
/// # Errors
///
/// Fails when the service was not started with credentials.
pub fn directory() -> Result<PathBuf> {
    env::var_os("CREDENTIALS_DIRECTORY")
        .map(PathBuf::from)
        .context("CREDENTIALS_DIRECTORY is not set")
}

/// Reads the `password` and `totp` credentials from a directory.
///
/// # Errors
///
/// Fails when either file is missing or unreadable.
pub fn load(dir: &Path) -> Result<Credentials> {
    Ok(Credentials {
        password: read(&dir.join("password"))?,
        totp: read(&dir.join("totp"))?,
    })
}

fn read(path: &Path) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(text.trim_end_matches(['\n', '\r']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_lose_only_the_final_newline() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("password"), " pa ss \n").unwrap();
        fs::write(dir.path().join("totp"), "otpauth://totp/x?secret=GEZA\r\n").unwrap();
        let credentials = load(dir.path()).unwrap();
        assert_eq!(credentials.password, " pa ss ");
        assert_eq!(credentials.totp, "otpauth://totp/x?secret=GEZA");
    }

    #[test]
    fn a_missing_credential_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("password"), "x").unwrap();
        assert!(load(dir.path()).is_err());
    }
}
