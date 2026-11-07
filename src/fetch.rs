//! Reads mail from Tuta: which mails a folder holds, and the text and files of each.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use tutasdk::crypto::crypto_facade::ResolvedSessionKey;
use tutasdk::entities::entity_facade::{
    has_owner_enc_session_key, list_element_id, owner_group, EntityDecryptionRequirements,
};
use tutasdk::entities::generated::sys::EntityEventBatch;
use tutasdk::entities::generated::tutanota::{
    ConversationEntry, Mail, MailAddress, MailDetails, MailDetailsBlob, MailDetailsDraft,
    MailSetEntry, TutanotaFile,
};
use tutasdk::entities::Entity;
use tutasdk::rest_error::HttpError;
use tutasdk::tutanota_constants::ArchiveDataType;
use tutasdk::{
    ApiCallError, CustomId, GeneratedId, IdTupleCustom, IdTupleGenerated, ListLoadDirection,
    LoggedInSdk,
};

use crate::message::{Address, Attachment, MailId, Message};

enum Source<'a> {
    Blob(&'a IdTupleGenerated),
    Draft(&'a IdTupleGenerated),
}

/// Reads mail from a logged-in session. Nothing is written to the account.
pub struct Fetcher {
    sdk: Arc<LoggedInSdk>,
}

impl Fetcher {
    /// A reader for the given session.
    pub fn new(sdk: Arc<LoggedInSdk>) -> Self {
        Self { sdk }
    }

    /// The mails in a folder, newest first, given the folder's entry list.
    ///
    /// # Errors
    ///
    /// Fails when Tuta refuses or the entries cannot be decrypted.
    pub async fn mail_ids(&self, entries: &GeneratedId) -> Result<Vec<IdTupleGenerated>> {
        let entries: Vec<MailSetEntry> = self
            .sdk
            .mail_facade()
            .get_crypto_entity_client()
            .load_all(entries, ListLoadDirection::DESC)
            .await
            .context("cannot list the folder")?;
        Ok(entries.into_iter().map(|entry| entry.mail).collect())
    }

    /// The mail a folder entry points at, or `None` if the entry is already gone.
    ///
    /// # Errors
    ///
    /// Fails when Tuta refuses or the entry cannot be decrypted.
    pub async fn mail_of_entry(
        &self,
        entries: &str,
        entry: &str,
    ) -> Result<Option<IdTupleGenerated>> {
        let id = IdTupleCustom {
            list_id: GeneratedId(entries.to_owned()),
            element_id: CustomId(entry.to_owned()),
        };
        let loaded = self
            .sdk
            .mail_facade()
            .get_crypto_entity_client()
            .load::<MailSetEntry, _>(&id)
            .await;
        match loaded {
            Ok(entry) => Ok(Some(entry.mail)),
            Err(ApiCallError::ServerResponseError {
                source: HttpError::NotFoundError,
            }) => Ok(None),
            Err(error) => Err(error).context("cannot load the folder entry"),
        }
    }

    /// The mailbox's group and its newest event batch, where the event stream should resume from.
    ///
    /// # Errors
    ///
    /// Fails when Tuta refuses.
    pub async fn latest_batch(&self) -> Result<Option<(String, String)>> {
        let mail = self.sdk.mail_facade();
        let mailbox = mail
            .load_user_mailbox()
            .await
            .context("cannot load the mailbox")?;
        let group = mailbox
            ._ownerGroup
            .context("the mailbox has no owner group")?;
        let batches: Vec<EntityEventBatch> = mail
            .get_crypto_entity_client()
            .load_range(&group, &GeneratedId::max_id(), 1, ListLoadDirection::DESC)
            .await
            .context("cannot read the newest event batch")?;
        Ok(batches
            .into_iter()
            .next()
            .and_then(|batch| batch._id)
            .map(|id| (group.0, id.element_id.0)))
    }

    /// Loads the given mails in the order asked, leaving out any that are gone.
    ///
    /// # Errors
    ///
    /// Fails when Tuta refuses or a mail cannot be decrypted.
    pub async fn mails(&self, ids: &[IdTupleGenerated]) -> Result<Vec<Mail>> {
        let client = self.sdk.mail_facade().get_crypto_entity_client();
        let mut lists: Vec<(&GeneratedId, Vec<GeneratedId>)> = Vec::new();
        for id in ids {
            match lists.iter_mut().find(|(list, _)| **list == id.list_id) {
                Some((_, elements)) => elements.push(id.element_id.clone()),
                None => lists.push((&id.list_id, vec![id.element_id.clone()])),
            }
        }
        let mut loaded = HashMap::new();
        for (list, elements) in &lists {
            let batch: Vec<Mail> = client
                .load_multiple(list, elements)
                .await
                .context("cannot load mails")?;
            for mail in batch {
                if let Some(id) = &mail._id {
                    loaded.insert(id.element_id.clone(), mail);
                }
            }
        }
        Ok(ids
            .iter()
            .filter_map(|id| loaded.remove(&id.element_id))
            .collect())
    }

    /// Loads the text, the files and the Message-ID of a mail.
    ///
    /// # Errors
    ///
    /// Fails when Tuta refuses or anything cannot be decrypted.
    pub async fn message(&self, mail: &Mail) -> Result<Message> {
        let details = self.details(mail).await?;
        let files = self.attachments(mail).await?;
        let message_id = self.message_id(mail).await?;
        Ok(to_message(mail, details.as_ref(), files, message_id))
    }

    async fn message_id(&self, mail: &Mail) -> Result<Option<String>> {
        let entry = self
            .sdk
            .mail_facade()
            .get_crypto_entity_client()
            .load::<ConversationEntry, _>(&mail.conversationEntry)
            .await;
        match entry {
            Ok(entry) => Ok(Some(entry.messageId)),
            Err(ApiCallError::ServerResponseError {
                source: HttpError::NotFoundError,
            }) => Ok(None),
            Err(error) => Err(error).context("cannot load the conversation entry"),
        }
    }

    async fn details(&self, mail: &Mail) -> Result<Option<MailDetails>> {
        match (&mail.mailDetails, &mail.mailDetailsDraft) {
            (Some(id), None) => {
                let blob: MailDetailsBlob = self.body(mail, Source::Blob(id)).await?;
                Ok(Some(blob.details))
            }
            (None, Some(id)) => {
                let draft: MailDetailsDraft = self.body(mail, Source::Draft(id)).await?;
                Ok(Some(draft.details))
            }
            _ => Ok(None),
        }
    }

    async fn body<T: Entity + DeserializeOwned>(
        &self,
        mail: &Mail,
        source: Source<'_>,
    ) -> Result<T> {
        let type_ref = T::type_ref();
        let client = self.sdk.get_entity_client();
        let parsed = match source {
            Source::Blob(id) => {
                let bytes = self
                    .sdk
                    .blob_facade()
                    .load_blob_element(&type_ref, id)
                    .await
                    .context("cannot download the mail text")?;
                let raw = serde_json::from_slice::<Vec<_>>(&bytes)
                    .context("invalid blob response")?
                    .into_iter()
                    .next()
                    .context("empty blob response")?;
                client.parse_raw(&type_ref, raw)?
            }
            Source::Draft(id) => client
                .load(&type_ref, id)
                .await
                .context("cannot load the draft text")?,
        };
        let model = client.resolve_server_type_ref(&type_ref)?;
        let needs = EntityDecryptionRequirements::for_entity(
            &model,
            &parsed,
            &self.sdk.type_model_provider,
        )?;
        let key = if needs.session_key {
            Some(self.owner_key(mail).await?)
        } else {
            None
        };
        let decrypted = self
            .sdk
            .mail_facade()
            .get_crypto_entity_client()
            .decrypt_parsed(&type_ref, parsed, key)
            .await
            .context("cannot decrypt the mail text")?;
        self.sdk
            .instance_mapper
            .parse_entity(decrypted)
            .map_err(|error| anyhow!("cannot read the mail text: {error}"))
    }

    async fn owner_key(&self, mail: &Mail) -> Result<ResolvedSessionKey> {
        let group = mail
            ._ownerGroup
            .as_ref()
            .context("mail has no owner group")?;
        let encrypted = mail
            ._ownerEncSessionKey
            .as_ref()
            .context("mail has no session key")?;
        let version = mail._ownerKeyVersion.unwrap_or(0).unsigned_abs();
        let group_key = self
            .sdk
            .mail_facade()
            .get_crypto_entity_client()
            .get_crypto_facade()
            .get_key_loader_facade()
            .load_sym_group_key(group, version, None)
            .await
            .map_err(|error| anyhow!("cannot load the group key: {error}"))?;
        let session_key = group_key
            .decrypt_aes_key(encrypted)
            .map_err(|error| anyhow!("cannot decrypt the session key: {error}"))?;
        Ok(ResolvedSessionKey {
            session_key,
            owner_enc_session_key: encrypted.clone(),
            owner_key_version: version,
            sender_identity_pub_key: None,
        })
    }

    async fn attachments(&self, mail: &Mail) -> Result<Vec<(TutanotaFile, Vec<u8>)>> {
        if mail.attachments.is_empty() {
            return Ok(Vec::new());
        }
        let type_ref = TutanotaFile::type_ref();
        let client = self.sdk.get_entity_client();
        let model = client.resolve_server_type_ref(&type_ref)?;
        let mut lists: Vec<(&GeneratedId, Vec<GeneratedId>)> = Vec::new();
        for id in &mail.attachments {
            match lists.iter_mut().find(|(list, _)| **list == id.list_id) {
                Some((_, elements)) => elements.push(id.element_id.clone()),
                None => lists.push((&id.list_id, vec![id.element_id.clone()])),
            }
        }
        let mut parsed_files = HashMap::new();
        for (list, elements) in &lists {
            let loaded = client
                .load_multiple(&type_ref, list, elements)
                .await
                .context("cannot load the attachments")?;
            for parsed in loaded {
                let id = list_element_id(&model, &parsed).context("an attachment has no id")?;
                if id.list_id != **list || !elements.contains(&id.element_id) {
                    bail!("Tuta returned an attachment the mail does not list");
                }
                parsed_files.insert(id.element_id, parsed);
            }
        }
        if parsed_files.len() != mail.attachments.len() {
            bail!("Tuta did not return every attachment");
        }

        let crypto_client = self.sdk.mail_facade().get_crypto_entity_client();
        let crypto = crypto_client.get_crypto_facade();
        let mut keys = HashMap::new();
        let mut from_bucket: Vec<IdTupleGenerated> = Vec::new();
        for id in &mail.attachments {
            let parsed = &parsed_files[&id.element_id];
            if has_owner_enc_session_key(&model, parsed) {
                let key = crypto
                    .resolve_session_key(parsed, &model)
                    .await
                    .map_err(|error| anyhow!("cannot resolve an attachment key: {error}"))?
                    .context("an attachment is encrypted but has no key")?;
                keys.insert(id.element_id.clone(), key);
            } else {
                from_bucket.push(id.clone());
            }
        }
        if !from_bucket.is_empty() {
            let bucket = mail
                .bucketKey
                .as_ref()
                .context("an attachment has no key and the mail has no bucket")?;
            let mail_group = mail
                ._ownerGroup
                .as_ref()
                .context("mail has no owner group")?;
            if from_bucket.iter().any(|id| {
                owner_group(&model, &parsed_files[&id.element_id]).as_ref() != Some(mail_group)
            }) {
                bail!("an attachment belongs to another group than its mail");
            }
            let resolved = crypto
                .resolve_bucket_instance_keys(bucket, mail_group, &from_bucket)
                .await
                .map_err(|error| anyhow!("cannot resolve the bucket keys: {error}"))?;
            keys.extend(
                from_bucket
                    .iter()
                    .map(|id| id.element_id.clone())
                    .zip(resolved),
            );
        }

        let mut out = Vec::with_capacity(mail.attachments.len());
        for id in &mail.attachments {
            let parsed = parsed_files
                .remove(&id.element_id)
                .context("an attachment is listed twice")?;
            let key = keys
                .remove(&id.element_id)
                .context("an attachment has no key")?;
            let decrypted = crypto_client
                .decrypt_parsed(&type_ref, parsed, Some(key.clone()))
                .await
                .context("cannot decrypt an attachment")?;
            let file: TutanotaFile = self
                .sdk
                .instance_mapper
                .parse_entity(decrypted)
                .map_err(|error| anyhow!("cannot read an attachment: {error}"))?;
            if !file._errors.is_empty() {
                bail!("an attachment's name did not decrypt");
            }
            let data = self.file_data(&file, &key).await?;
            out.push((file, data));
        }
        Ok(out)
    }

    async fn file_data(&self, file: &TutanotaFile, key: &ResolvedSessionKey) -> Result<Vec<u8>> {
        let instance = file._id.as_ref().context("an attachment has no id")?;
        let mut archives: HashMap<&GeneratedId, Vec<GeneratedId>> = HashMap::new();
        for blob in &file.blobs {
            archives
                .entry(&blob.archiveId)
                .or_default()
                .push(blob.blobId.clone());
        }
        let mut chunks = HashMap::new();
        for (archive, ids) in archives {
            let encrypted = self
                .sdk
                .blob_facade()
                .download_blobs(archive, instance, ArchiveDataType::Attachments, &ids)
                .await
                .context("cannot download an attachment")?;
            for (id, bytes) in encrypted {
                let data = key
                    .session_key
                    .decrypt_data(&bytes)
                    .map_err(|error| anyhow!("cannot decrypt an attachment blob: {error}"))?;
                chunks.insert((archive, id), data);
            }
        }
        let mut data = Vec::new();
        for blob in &file.blobs {
            let chunk = chunks
                .get(&(&blob.archiveId, blob.blobId.clone()))
                .context("Tuta did not return every attachment blob")?;
            data.extend_from_slice(chunk);
        }
        Ok(data)
    }
}

fn address(address: &MailAddress) -> Address {
    Address {
        name: address.name.clone(),
        address: address.address.clone(),
    }
}

fn to_message(
    mail: &Mail,
    details: Option<&MailDetails>,
    files: Vec<(TutanotaFile, Vec<u8>)>,
    message_id: Option<String>,
) -> Message {
    let id = mail
        ._id
        .as_ref()
        .map(|id| id.element_id.0.clone())
        .unwrap_or_default();
    let recipients = |pick: fn(&MailDetails) -> &Vec<MailAddress>| -> Vec<Address> {
        details.map_or_else(Vec::new, |details| {
            pick(details).iter().map(address).collect()
        })
    };
    Message {
        id: MailId(id),
        message_id: message_id.filter(|id| !id.is_empty()),
        date_ms: mail.receivedDate.as_millis(),
        subject: mail.subject.clone(),
        from: address(&mail.sender),
        to: recipients(|details| &details.recipients.toRecipients),
        cc: recipients(|details| &details.recipients.ccRecipients),
        reply_to: details.map_or_else(Vec::new, |details| {
            details
                .replyTos
                .iter()
                .map(|reply| Address {
                    name: reply.name.clone(),
                    address: reply.address.clone(),
                })
                .collect()
        }),
        html: details
            .and_then(|details| {
                details
                    .body
                    .compressedText
                    .as_ref()
                    .or(details.body.text.as_ref())
            })
            .cloned()
            .unwrap_or_default(),
        attachments: files
            .into_iter()
            .map(|(file, data)| Attachment {
                name: file.name,
                mime: file
                    .mimeType
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                cid: file.cid,
                data,
            })
            .collect(),
        unread: mail.unread,
    }
}

#[cfg(test)]
mod tests {
    use tutasdk::bindings::test_file_client::TestFileClient;
    use tutasdk::date::DateTime;
    use tutasdk::entities::generated::tutanota::Mail;
    use tutasdk::entities::generated::tutanota::{Body, EncryptedMailAddress, Recipients};

    use super::*;
    use crate::sdk::{self, fixture};

    async fn fixture_mail() -> Mail {
        let sdk = sdk::new(
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
        logged_in.instance_mapper.parse_entity(decrypted).unwrap()
    }

    fn mail_address(name: &str, address: &str) -> MailAddress {
        MailAddress {
            _id: None,
            name: name.to_owned(),
            address: address.to_owned(),
            contact: None,
            _errors: Default::default(),
        }
    }

    fn details(text: Option<&str>, compressed: Option<&str>) -> MailDetails {
        MailDetails {
            _id: None,
            sentDate: DateTime::from_system_time(std::time::UNIX_EPOCH),
            authStatus: 0,
            replyTos: vec![EncryptedMailAddress {
                _id: None,
                name: "Reply".to_owned(),
                address: "reply@example.org".to_owned(),
                _errors: Default::default(),
            }],
            recipients: Recipients {
                _id: None,
                toRecipients: vec![mail_address("To", "to@example.org")],
                ccRecipients: vec![mail_address("Cc", "cc@example.org")],
                bccRecipients: vec![mail_address("Bcc", "bcc@example.org")],
            },
            headers: None,
            body: Body {
                _id: None,
                text: text.map(str::to_owned),
                compressedText: compressed.map(str::to_owned),
                _errors: Default::default(),
            },
        }
    }

    #[tokio::test]
    async fn a_mail_is_mapped_to_a_message() {
        let mail = fixture_mail().await;
        let details = details(Some("<p>plain</p>"), None);
        let message = to_message(&mail, Some(&details), Vec::new(), Some("<m@x>".to_owned()));
        assert_eq!(message.id, MailId("O1qC7an--3-0".to_owned()));
        assert_eq!(message.message_id.as_deref(), Some("<m@x>"));
        assert_eq!(message.date_ms, 1_721_043_814_832);
        assert_eq!(message.subject, "Html email features");
        assert_eq!(message.from.address, "map-free@tutanota.de");
        assert_eq!(message.from.name, "Matthias");
        assert_eq!(message.to[0].address, "to@example.org");
        assert_eq!(message.cc[0].address, "cc@example.org");
        assert_eq!(message.reply_to[0].address, "reply@example.org");
        assert_eq!(message.html, "<p>plain</p>");
        assert_eq!(message.unread, mail.unread);
    }

    #[tokio::test]
    async fn the_bcc_is_never_written_and_compressed_text_wins() {
        let mail = fixture_mail().await;
        let details = details(Some("old"), Some("new"));
        let message = to_message(&mail, Some(&details), Vec::new(), None);
        assert_eq!(message.html, "new");
        assert!(message.message_id.is_none());
        let everyone: Vec<&str> = message
            .to
            .iter()
            .chain(&message.cc)
            .map(|a| a.address.as_str())
            .collect();
        assert!(!everyone.contains(&"bcc@example.org"));
    }

    #[tokio::test]
    async fn a_mail_without_details_has_an_empty_body_and_an_empty_message_id_is_none() {
        let mail = fixture_mail().await;
        let message = to_message(&mail, None, Vec::new(), Some(String::new()));
        assert_eq!(message.html, "");
        assert!(message.to.is_empty() && message.reply_to.is_empty());
        assert!(message.message_id.is_none());
    }
}
