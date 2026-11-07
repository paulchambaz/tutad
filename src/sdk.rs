//! The Tuta SDK, built so that all its traffic goes through the [`Governor`].

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tutasdk::bindings::file_client::{FileClient, FileClientError};
use tutasdk::bindings::rest_client::RestClient;
use tutasdk::net::native_rest_client::NativeRestClient;
use tutasdk::Sdk;

use crate::governor::Governor;

/// Keeps what the SDK caches, such as Tuta's type model, in the state directory.
pub struct StateFiles {
    dir: PathBuf,
}

#[async_trait]
impl FileClient for StateFiles {
    async fn persist_content(&self, name: String, content: Vec<u8>) -> Result<(), FileClientError> {
        let path = self.path(&name)?;
        fs::create_dir_all(&self.dir).map_err(|_| FileClientError::IoError)?;
        fs::write(path, content).map_err(|_| FileClientError::IoError)
    }

    async fn read_content(&self, name: String) -> Result<Vec<u8>, FileClientError> {
        fs::read(self.path(&name)?).map_err(|_| FileClientError::NotFound)
    }
}

impl StateFiles {
    fn path(&self, name: &str) -> Result<PathBuf, FileClientError> {
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(FileClientError::Unknown);
        }
        Ok(self.dir.join(name))
    }
}

/// An SDK that talks to Tuta only through the governor, with its own suspension off.
pub fn new(
    base_url: String,
    rest_client: Arc<dyn RestClient>,
    file_client: Arc<dyn FileClient>,
) -> Sdk {
    Sdk::new_without_suspension(base_url, Arc::new(Governor::new(rest_client)), file_client)
}

/// The SDK as it runs for real, caching in a directory.
///
/// # Errors
///
/// Fails when the system's TLS roots cannot be loaded.
pub fn native(base_url: &str, dir: PathBuf) -> Result<Sdk> {
    let rest_client = NativeRestClient::try_new().context("cannot set up TLS")?;
    Ok(new(
        base_url.to_owned(),
        Arc::new(rest_client),
        Arc::new(StateFiles { dir }),
    ))
}

#[cfg(test)]
pub mod fixture {
    use std::collections::HashMap;

    use base64::prelude::BASE64_STANDARD;
    use base64::Engine;
    use tutasdk::bindings::rest_client::HttpMethod;
    use tutasdk::bindings::test_rest_client::TestRestClient;
    use tutasdk::login::{CredentialType, Credentials};
    use tutasdk::GeneratedId;

    pub const BASE: &str = "http://localhost:9000";
    const FILES: &str = "sdk/generated/tuta-sdk/rust/sdk/tests/download_mail_test";

    pub fn rest_client() -> TestRestClient {
        let mut client = TestRestClient::new(BASE);
        for (path, file) in [
            (
                "/rest/sys/Session/O1qC702-1J-0/3u3i8Lr9_7TnDDdAVw7w3TypTD2k1L00vIUTMF0SIPY",
                "session.json",
            ),
            ("/rest/sys/User/O1qC700----0", "user.json"),
            ("/rest/tutanota/Mail/O1qC705-17-0/O1qC7an--3-0", "mail.json"),
        ] {
            let body = std::fs::read(format!("{FILES}/{file}")).unwrap();
            client.insert_response(
                &format!("{BASE}{path}"),
                HttpMethod::GET,
                200,
                HashMap::default(),
                Some(&body),
            );
        }
        client
    }

    pub fn credentials() -> Credentials {
        Credentials {
            login: "bed-free@tutanota.de".to_owned(),
            user_id: GeneratedId("O1qC700----0".to_owned()),
            access_token: "ZC2NIBDACUABAdJhibIwclzaPU3fEu-NzQ".to_owned(),
            encrypted_passphrase_key: BASE64_STANDARD
                .decode("AZWEA/KTrHu0bW52CsctsBTTV4U3jrU51TadSxf6Nqs3xbEs3WfoOpPtxUDCNjHNppt6LHCfgTioejjGUJ2cCsXosZAysUiau5Nvyi8mtjLz")
                .unwrap(),
            credential_type: CredentialType::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use tutasdk::bindings::test_file_client::TestFileClient;
    use tutasdk::entities::generated::tutanota::Mail;
    use tutasdk::{GeneratedId, IdTupleGenerated};

    use super::*;

    #[tokio::test]
    async fn a_fixture_mail_is_decrypted_offline() {
        let sdk = new(
            fixture::BASE.to_owned(),
            Arc::new(fixture::rest_client()),
            Arc::new(TestFileClient::default()),
        );
        let logged_in = sdk.login(fixture::credentials()).await.unwrap();
        let decrypted = logged_in
            .mail_facade()
            .load_untyped_mail(&IdTupleGenerated {
                list_id: GeneratedId("O1qC705-17-0".to_owned()),
                element_id: GeneratedId("O1qC7an--3-0".to_owned()),
            })
            .await
            .unwrap();
        let mail: Mail = logged_in.instance_mapper.parse_entity(decrypted).unwrap();
        assert_eq!(mail.subject, "Html email features");
        assert_eq!(mail.sender.address, "map-free@tutanota.de");
        assert_eq!(mail.sender.name, "Matthias");
        assert_eq!(mail.receivedDate.as_millis(), 1_721_043_814_832);
    }

    #[tokio::test]
    async fn cached_files_stay_inside_their_directory() {
        let dir = tempfile::tempdir().unwrap();
        let files = StateFiles {
            dir: dir.path().to_owned(),
        };
        files
            .persist_content("model.json".to_owned(), vec![1, 2])
            .await
            .unwrap();
        assert_eq!(
            files.read_content("model.json".to_owned()).await.unwrap(),
            vec![1, 2]
        );
        assert!(files.read_content("../x".to_owned()).await.is_err());
        assert!(files
            .persist_content("a/b".to_owned(), vec![])
            .await
            .is_err());
        assert!(files.read_content("missing".to_owned()).await.is_err());
    }
}
