//! Sends a message through Tuta as a draft that is then sent.

use anyhow::{Context, Result};
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use crypto_primitives::aes::{Aes256Key, InitializationVector};
use crypto_primitives::key::GenericAesKey;
use crypto_primitives::randomizer_facade::RandomizerFacade;
use log::{info, warn};
use thiserror::Error;
use tutasdk::blobs::blob_facade::FileData;
use tutasdk::entities::generated::sys::BlobReferenceTokenWrapper;
use tutasdk::entities::generated::tutanota::{
    AttachmentKeyData, DraftAttachment, DraftCreateData, DraftData, DraftRecipient, Mail,
    NewDraftAttachment, SendDraftData, SendDraftParameters,
};
use tutasdk::rest_error::HttpError;
use tutasdk::services::generated::tutanota::{DraftService, SendDraftService};
use tutasdk::services::ExtraServiceParams;
use tutasdk::tutanota_constants::ArchiveDataType;
use tutasdk::{ApiCallError, CustomId, GeneratedId, IdTupleGenerated, LoggedInSdk};

use crate::message::Address;
use crate::outgoing::Outgoing;

const NEW_CONVERSATION: i64 = 0;

/// The account may not send as the address the message came from.
#[derive(Debug, Error)]
#[error("this account cannot send as the sender address")]
pub struct Refused;

fn random_id(randomizer: &RandomizerFacade) -> CustomId {
    CustomId(BASE64_URL_SAFE_NO_PAD.encode(randomizer.generate_random_array::<4>()))
}

fn recipients(list: &[Address]) -> Vec<DraftRecipient> {
    list.iter()
        .map(|recipient| DraftRecipient {
            _id: None,
            name: if recipient.name.is_empty() {
                recipient.address.clone()
            } else {
                recipient.name.clone()
            },
            mailAddress: recipient.address.clone(),
            _errors: Default::default(),
        })
        .collect()
}

fn plaintext_as_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

fn draft_data(outgoing: &Outgoing, added: Vec<DraftAttachment>) -> DraftData {
    let body = if outgoing.plaintext {
        plaintext_as_html(&outgoing.body)
    } else {
        outgoing.body.clone()
    };
    DraftData {
        _id: None,
        subject: outgoing.subject.clone(),
        bodyText: body.clone(),
        senderMailAddress: outgoing.from.address.clone(),
        senderName: if outgoing.from.name.is_empty() {
            outgoing.from.address.clone()
        } else {
            outgoing.from.name.clone()
        },
        confidential: false,
        method: 0,
        compressedBodyText: Some(body),
        toRecipients: recipients(&outgoing.to),
        ccRecipients: recipients(&outgoing.cc),
        bccRecipients: recipients(&outgoing.bcc),
        addedAttachments: added,
        removedAttachments: vec![],
        replyTos: vec![],
        _errors: Default::default(),
    }
}

fn send_data(
    session_key: Vec<u8>,
    draft: IdTupleGenerated,
    parameters_id: CustomId,
    attachment_keys: Vec<AttachmentKeyData>,
    plaintext: bool,
) -> SendDraftData {
    SendDraftData {
        _format: 0,
        language: "en".to_owned(),
        mailSessionKey: Some(session_key.clone()),
        bucketEncMailSessionKey: None,
        senderNameUnencrypted: None,
        plaintext,
        calendarMethod: false,
        sessionEncEncryptionAuthStatus: None,
        sendAt: None,
        allowUndo: false,
        internalRecipientKeyData: vec![],
        secureExternalRecipientKeyData: vec![],
        attachmentKeyData: attachment_keys.clone(),
        mail: draft.clone(),
        symEncInternalRecipientKeyData: vec![],
        parameters: Some(SendDraftParameters {
            _id: Some(parameters_id),
            language: "en".to_owned(),
            mailSessionKey: Some(session_key),
            bucketEncMailSessionKey: None,
            senderNameUnencrypted: None,
            plaintext,
            calendarMethod: false,
            sessionEncEncryptionAuthStatus: None,
            mail: draft,
            internalRecipientKeyData: vec![],
            secureExternalRecipientKeyData: vec![],
            symEncInternalRecipientKeyData: vec![],
            attachmentKeyData: attachment_keys,
        }),
    }
}

async fn mail_group(sdk: &LoggedInSdk, address: &str) -> Result<GeneratedId> {
    match sdk
        .mail_facade()
        .get_group_id_for_mail_address(address)
        .await
    {
        Ok(group) => Ok(group),
        Err(ApiCallError::ServerResponseError {
            source: HttpError::NotFoundError,
        }) => Err(Refused.into()),
        Err(error) => Err(error).context("cannot look up the sender address"),
    }
}

async fn upload(
    sdk: &LoggedInSdk,
    outgoing: &Outgoing,
    group: &GeneratedId,
    group_key: &GenericAesKey,
    group_key_version: i64,
    randomizer: &RandomizerFacade,
) -> Result<(Vec<DraftAttachment>, Vec<GenericAesKey>)> {
    let keys: Vec<GenericAesKey> = outgoing
        .attachments
        .iter()
        .map(|_| Aes256Key::generate(randomizer).into())
        .collect();
    if keys.is_empty() {
        return Ok((Vec::new(), keys));
    }
    let files: Vec<FileData> = outgoing
        .attachments
        .iter()
        .zip(&keys)
        .map(|(attachment, key)| FileData {
            session_key: key.clone(),
            data: attachment.data.as_slice(),
        })
        .collect();
    let tokens: Vec<Vec<BlobReferenceTokenWrapper>> = sdk
        .blob_facade()
        .encrypt_and_upload_multiple(ArchiveDataType::Attachments, group, files.iter())
        .await
        .context("cannot upload an attachment")?;
    let mut added = Vec::with_capacity(keys.len());
    for ((attachment, key), tokens) in outgoing.attachments.iter().zip(&keys).zip(tokens) {
        let seal = |text: &str| {
            key.encrypt_data(text.as_bytes(), InitializationVector::generate(randomizer))
                .context("cannot encrypt an attachment field")
        };
        added.push(DraftAttachment {
            _id: Some(random_id(randomizer)),
            ownerEncFileSessionKey: group_key
                .encrypt_key(key, InitializationVector::generate(randomizer)),
            ownerKeyVersion: group_key_version,
            newFile: Some(NewDraftAttachment {
                _id: Some(random_id(randomizer)),
                encFileName: seal(&attachment.name)?,
                encMimeType: seal(&attachment.mime)?,
                encCid: None,
                referenceTokens: tokens,
            }),
            existingFile: None,
        });
    }
    Ok((added, keys))
}

async fn attachment_keys(
    sdk: &LoggedInSdk,
    draft: &IdTupleGenerated,
    keys: &[GenericAesKey],
    randomizer: &RandomizerFacade,
) -> Result<Vec<AttachmentKeyData>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mail: Mail = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .load(draft)
        .await
        .context("cannot read back the draft")?;
    anyhow::ensure!(
        mail.attachments.len() == keys.len(),
        "the draft holds {} attachments, {} were uploaded",
        mail.attachments.len(),
        keys.len()
    );
    Ok(mail
        .attachments
        .into_iter()
        .zip(keys)
        .map(|(file, key)| AttachmentKeyData {
            _id: Some(random_id(randomizer)),
            bucketEncFileSessionKey: None,
            fileSessionKey: Some(key.as_bytes().to_vec()),
            file,
        })
        .collect())
}

/// Sends the message and returns the id Tuta gave it.
///
/// # Errors
///
/// Fails with [`Refused`] when the account cannot send as the sender, and otherwise
/// on the first problem from Tuta or the network. Nothing is retried.
pub async fn send(sdk: &LoggedInSdk, outgoing: &Outgoing) -> Result<String> {
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let session_key: GenericAesKey = Aes256Key::generate(&randomizer).into();
    let group = mail_group(sdk, &outgoing.from.address).await?;
    let group_key = sdk
        .get_current_sym_group_key(&group)
        .await
        .context("cannot load the mail group key")?;
    let owner_key_version = i64::try_from(group_key.version).context("bad group key version")?;
    let (added, keys) = upload(
        sdk,
        outgoing,
        &group,
        &group_key.object,
        owner_key_version,
        &randomizer,
    )
    .await?;
    if !outgoing.reply_to.is_empty() {
        warn!("dropping Reply-To: Tuta does not support it on outgoing mail");
    }
    let create = DraftCreateData {
        _format: 0,
        previousMessageId: None,
        conversationType: NEW_CONVERSATION,
        ownerEncSessionKey: group_key
            .object
            .encrypt_key(&session_key, InitializationVector::generate(&randomizer)),
        ownerKeyVersion: owner_key_version,
        draftData: draft_data(outgoing, added),
        _errors: Default::default(),
    };
    let executor = sdk.get_service_executor();
    let draft = executor
        .post::<DraftService>(
            create,
            ExtraServiceParams {
                session_key: Some(session_key.clone()),
                ..Default::default()
            },
        )
        .await
        .context("cannot create the draft")?
        .draft;
    let keys = attachment_keys(sdk, &draft, &keys, &randomizer).await?;
    let sent = executor
        .post::<SendDraftService>(
            send_data(
                session_key.as_bytes().to_vec(),
                draft,
                random_id(&randomizer),
                keys,
                outgoing.plaintext,
            ),
            ExtraServiceParams::default(),
        )
        .await
        .context("cannot send the draft")?;
    info!(
        "sent a mail with {} attachments",
        outgoing.attachments.len()
    );
    Ok(sent.messageId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Attachment;

    fn outgoing(plaintext: bool, body: &str) -> Outgoing {
        Outgoing {
            from: Address {
                name: String::new(),
                address: "ann@tutanota.com".to_owned(),
            },
            to: vec![Address {
                name: String::new(),
                address: "bob@example.org".to_owned(),
            }],
            cc: vec![],
            bcc: vec![Address {
                name: "Dana".to_owned(),
                address: "dana@example.org".to_owned(),
            }],
            reply_to: vec![],
            subject: "Hi".to_owned(),
            body: body.to_owned(),
            plaintext,
            message_id: None,
            attachments: Vec::<Attachment>::new(),
        }
    }

    #[test]
    fn plain_text_is_escaped_and_broken_into_lines() {
        assert_eq!(
            plaintext_as_html("a <b> & c\r\nd\ne\rf"),
            "a &lt;b&gt; &amp; c<br>d<br>e<br>f"
        );
    }

    #[test]
    fn empty_names_fall_back_to_the_address() {
        let data = draft_data(&outgoing(true, "x"), vec![]);
        assert_eq!(data.senderName, "ann@tutanota.com");
        assert_eq!(data.toRecipients[0].name, "bob@example.org");
        assert_eq!(data.bccRecipients[0].name, "Dana");
        assert_eq!(data.bccRecipients[0].mailAddress, "dana@example.org");
    }

    #[test]
    fn the_body_is_escaped_only_when_it_is_plain_text() {
        assert_eq!(
            draft_data(&outgoing(true, "<i>"), vec![]).bodyText,
            "&lt;i&gt;"
        );
        assert_eq!(draft_data(&outgoing(false, "<i>"), vec![]).bodyText, "<i>");
    }

    #[test]
    fn the_session_key_rides_in_both_places() {
        let draft = IdTupleGenerated::new(GeneratedId("l".to_owned()), GeneratedId("e".to_owned()));
        let data = send_data(vec![1, 2], draft, CustomId("p".to_owned()), vec![], true);
        assert!(data.plaintext);
        assert_eq!(data.mailSessionKey, Some(vec![1, 2]));
        assert_eq!(
            data.parameters
                .and_then(|parameters| parameters.mailSessionKey),
            Some(vec![1, 2])
        );
    }
}
