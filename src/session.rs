//! The saved Tuta session, so later starts resume it instead of logging in.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tutasdk::login::{CredentialType, Credentials};
use tutasdk::GeneratedId;

const FILE: &str = "session.json";

#[derive(Serialize, Deserialize)]
struct Stored {
    login: String,
    user_id: String,
    access_token: String,
    encrypted_passphrase_key: String,
    external: bool,
}

/// Reads the saved session, if there is one.
///
/// # Errors
///
/// Fails when the file exists but cannot be read or understood.
pub fn load(dir: &Path) -> Result<Option<Credentials>> {
    let path = dir.join(FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let stored: Stored = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a saved session", path.display()))?;
    Ok(Some(Credentials {
        login: stored.login,
        user_id: GeneratedId(stored.user_id),
        access_token: stored.access_token,
        encrypted_passphrase_key: BASE64_STANDARD
            .decode(&stored.encrypted_passphrase_key)
            .with_context(|| format!("{} holds a bad key", path.display()))?,
        credential_type: if stored.external {
            CredentialType::External
        } else {
            CredentialType::Internal
        },
    }))
}

/// Saves a session readable by the owner only, replacing any earlier one whole.
///
/// # Errors
///
/// Fails when the directory or the file cannot be written.
pub fn save(dir: &Path, credentials: &Credentials) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let stored = Stored {
        login: credentials.login.clone(),
        user_id: credentials.user_id.0.clone(),
        access_token: credentials.access_token.clone(),
        encrypted_passphrase_key: BASE64_STANDARD.encode(&credentials.encrypted_passphrase_key),
        external: credentials.credential_type == CredentialType::External,
    };
    let temporary = dir.join(format!("{FILE}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    file.write_all(serde_json::to_string(&stored)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, dir.join(FILE)).context("cannot replace the saved session")
}

/// Removes the saved session, if there is one.
///
/// # Errors
///
/// Fails when the file exists but cannot be removed.
pub fn forget(dir: &Path) -> Result<()> {
    match fs::remove_file(dir.join(FILE)) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).context("cannot remove the saved session")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn credentials(token: &str) -> Credentials {
        Credentials {
            login: "a@tuta.com".to_owned(),
            user_id: GeneratedId("uid".to_owned()),
            access_token: token.to_owned(),
            encrypted_passphrase_key: vec![0, 1, 2, 255],
            credential_type: CredentialType::Internal,
        }
    }

    #[test]
    fn a_session_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &credentials("token")).unwrap();
        let loaded = load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.login, "a@tuta.com");
        assert_eq!(loaded.user_id.0, "uid");
        assert_eq!(loaded.access_token, "token");
        assert_eq!(loaded.encrypted_passphrase_key, vec![0, 1, 2, 255]);
        assert_eq!(loaded.credential_type, CredentialType::Internal);
    }

    #[test]
    fn the_file_is_private_and_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &credentials("first")).unwrap();
        save(dir.path(), &credentials("second")).unwrap();
        let mode = fs::metadata(dir.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!dir.path().join("session.json.tmp").exists());
        assert_eq!(load(dir.path()).unwrap().unwrap().access_token, "second");
    }

    #[test]
    fn no_file_means_no_session() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn a_corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), "{ nope").unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn forgetting_removes_the_file_and_forgives_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &credentials("t")).unwrap();
        forget(dir.path()).unwrap();
        assert!(load(dir.path()).unwrap().is_none());
        forget(dir.path()).unwrap();
    }
}
