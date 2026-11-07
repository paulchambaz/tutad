//! The Tuta SDK, built so that all its traffic goes through the [`Governor`].

use std::sync::Arc;

use tutasdk::bindings::file_client::FileClient;
use tutasdk::bindings::rest_client::RestClient;
use tutasdk::Sdk;

use crate::governor::Governor;

/// An SDK that talks to Tuta only through the governor, with its own suspension off.
#[cfg_attr(not(test), expect(dead_code))]
pub fn new(
    base_url: String,
    rest_client: Arc<dyn RestClient>,
    file_client: Arc<dyn FileClient>,
) -> Sdk {
    Sdk::new_without_suspension(base_url, Arc::new(Governor::new(rest_client)), file_client)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::prelude::BASE64_STANDARD;
    use base64::Engine;
    use tutasdk::bindings::rest_client::HttpMethod;
    use tutasdk::bindings::test_file_client::TestFileClient;
    use tutasdk::bindings::test_rest_client::TestRestClient;
    use tutasdk::entities::generated::tutanota::Mail;
    use tutasdk::login::{CredentialType, Credentials};
    use tutasdk::{GeneratedId, IdTupleGenerated};

    use super::*;

    const BASE: &str = "http://localhost:9000";
    const FIXTURES: &str = "sdk/generated/tuta-sdk/rust/sdk/tests/download_mail_test";

    fn fixture_client() -> TestRestClient {
        let mut client = TestRestClient::new(BASE);
        for (path, file) in [
            (
                "/rest/sys/Session/O1qC702-1J-0/3u3i8Lr9_7TnDDdAVw7w3TypTD2k1L00vIUTMF0SIPY",
                "session.json",
            ),
            ("/rest/sys/User/O1qC700----0", "user.json"),
            ("/rest/tutanota/Mail/O1qC705-17-0/O1qC7an--3-0", "mail.json"),
        ] {
            let body = std::fs::read(format!("{FIXTURES}/{file}")).unwrap();
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

    #[tokio::test]
    async fn a_fixture_mail_is_decrypted_offline() {
        let credentials = Credentials {
            login: "bed-free@tutanota.de".to_owned(),
            user_id: GeneratedId("O1qC700----0".to_owned()),
            access_token: "ZC2NIBDACUABAdJhibIwclzaPU3fEu-NzQ".to_owned(),
            encrypted_passphrase_key: BASE64_STANDARD
                .decode("AZWEA/KTrHu0bW52CsctsBTTV4U3jrU51TadSxf6Nqs3xbEs3WfoOpPtxUDCNjHNppt6LHCfgTioejjGUJ2cCsXosZAysUiau5Nvyi8mtjLz")
                .unwrap(),
            credential_type: CredentialType::Internal,
        };
        let sdk = new(
            BASE.to_owned(),
            Arc::new(fixture_client()),
            Arc::new(TestFileClient::default()),
        );
        let logged_in = sdk.login(credentials).await.unwrap();
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
}
