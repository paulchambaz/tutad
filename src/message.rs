//! Plain data for one Tuta mail, as the renderer and the Maildir need it.

/// The element id of a mail, unique in the mailbox and kept when the mail moves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MailId(pub String);

/// A name and an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub address: String,
}

/// A file attached to a mail, already decrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub cid: Option<String>,
    pub data: Vec<u8>,
}

/// A decrypted mail with everything needed to write it as a message file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: MailId,
    pub message_id: Option<String>,
    pub date_ms: u64,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub html: String,
    pub attachments: Vec<Attachment>,
    pub unread: bool,
}
