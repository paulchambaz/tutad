//! A message submitted by Postfix, read into what Tuta needs to send it.

use mail_parser::{MessageParser, PartKind};
use thiserror::Error;

use crate::message::{Address, Attachment};

/// A submitted message that cannot be sent, whatever is tried later.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// The input is not a mail message.
    #[error("the message cannot be read")]
    Unreadable,
    /// The message has no usable `From` address.
    #[error("the message has no sender")]
    NoSender,
    /// The envelope names nobody to send to.
    #[error("the message has no recipient")]
    NoRecipient,
}

/// A message ready to become a Tuta draft.
#[derive(Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub body: String,
    pub plaintext: bool,
    pub message_id: Option<String>,
    pub attachments: Vec<Attachment>,
}

fn addresses(list: Option<mail_parser::AddressList<'_>>) -> Vec<Address> {
    list.into_iter()
        .flat_map(|list| list.mailboxes())
        .filter_map(|mailbox| {
            Some(Address {
                name: mailbox.name().unwrap_or_default().to_owned(),
                address: mailbox.address()?.to_owned(),
            })
        })
        .collect()
}

fn listed(list: &[Address], address: &str) -> bool {
    list.iter()
        .any(|listed| listed.address.eq_ignore_ascii_case(address))
}

/// Reads a submitted message, sent to exactly the envelope recipients.
///
/// A recipient named in `To` or `Cc` keeps that place and every other one is a blind copy.
///
/// # Errors
///
/// Fails when the input is no message, has no sender, or the envelope is empty.
pub fn parse(raw: &[u8], envelope: &[String]) -> Result<Outgoing, Error> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(Error::Unreadable)?;
    let from = addresses(message.from())
        .into_iter()
        .next()
        .ok_or(Error::NoSender)?;
    if envelope.is_empty() {
        return Err(Error::NoRecipient);
    }
    let headers_to = addresses(message.to());
    let headers_cc = addresses(message.cc());
    let mut to = Vec::new();
    let mut cc = Vec::new();
    let mut bcc = Vec::new();
    for recipient in envelope {
        let found = |list: &[Address]| {
            list.iter()
                .find(|listed| listed.address.eq_ignore_ascii_case(recipient))
                .cloned()
        };
        if let Some(address) = found(&headers_to) {
            to.push(address);
        } else if let Some(address) = found(&headers_cc) {
            cc.push(address);
        } else if !listed(&bcc, recipient) {
            bcc.push(Address {
                name: String::new(),
                address: recipient.clone(),
            });
        }
    }
    let plaintext = !message
        .parts()
        .any(|part| matches!(part.kind(), PartKind::Html));
    let body = if plaintext {
        message.body_text(0)
    } else {
        message.body_html(0)
    }
    .map(|body| body.into_owned())
    .unwrap_or_default();
    let attachments = message
        .attachments()
        .map(|part| Attachment {
            name: part.attachment_name().unwrap_or("attachment").to_owned(),
            mime: part.content_type().map_or_else(
                || "application/octet-stream".to_owned(),
                |kind| match kind.subtype() {
                    Some(subtype) => format!("{}/{subtype}", kind.ctype()),
                    None => kind.ctype().to_owned(),
                },
            ),
            cid: None,
            data: part.decoded().into_owned(),
        })
        .collect();
    Ok(Outgoing {
        from,
        to,
        cc,
        bcc,
        reply_to: addresses(message.reply_to()),
        subject: message.subject().unwrap_or_default().to_owned(),
        body,
        plaintext,
        message_id: message.message_id().map(str::to_owned),
        attachments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const PLAIN: &[u8] = b"From: Ann <ann@tutanota.com>\r\n\
To: Bob <bob@example.org>\r\n\
Cc: carl@example.org\r\n\
Subject: =?UTF-8?Q?Caf=C3=A9?=\r\n\
Message-ID: <one@example.org>\r\n\
\r\n\
Hello <b>there</b> & bye\r\n";

    #[test]
    fn a_plain_message_keeps_its_text_and_headers() {
        let sent = parse(
            PLAIN,
            &envelope(&["bob@example.org", "carl@example.org", "dana@example.org"]),
        )
        .unwrap();
        assert_eq!(sent.from.address, "ann@tutanota.com");
        assert_eq!(sent.from.name, "Ann");
        assert_eq!(sent.subject, "Café");
        assert!(sent.plaintext);
        assert_eq!(sent.body.trim_end(), "Hello <b>there</b> & bye");
        assert_eq!(sent.message_id.as_deref(), Some("one@example.org"));
        assert_eq!(sent.to[0].name, "Bob");
        assert_eq!(sent.cc[0].address, "carl@example.org");
        assert_eq!(sent.bcc[0].address, "dana@example.org");
    }

    #[test]
    fn only_the_envelope_is_written_to() {
        let sent = parse(PLAIN, &envelope(&["BOB@example.org"])).unwrap();
        assert_eq!(sent.to.len(), 1);
        assert!(sent.cc.is_empty() && sent.bcc.is_empty());
    }

    #[test]
    fn an_html_alternative_wins_over_the_text() {
        let raw = b"From: ann@tutanota.com\r\n\
To: bob@example.org\r\n\
Subject: Hi\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
plain\r\n\
--b\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>html</p>\r\n\
--b--\r\n";
        let sent = parse(raw, &envelope(&["bob@example.org"])).unwrap();
        assert!(!sent.plaintext);
        assert_eq!(sent.body.trim_end(), "<p>html</p>");
    }

    #[test]
    fn an_attachment_is_decoded() {
        let raw = b"From: ann@tutanota.com\r\n\
To: bob@example.org\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
see file\r\n\
--b\r\n\
Content-Type: application/pdf; name=\"a.pdf\"\r\n\
Content-Disposition: attachment; filename=\"a.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
aGVsbG8=\r\n\
--b--\r\n";
        let sent = parse(raw, &envelope(&["bob@example.org"])).unwrap();
        assert_eq!(sent.attachments.len(), 1);
        assert_eq!(sent.attachments[0].name, "a.pdf");
        assert_eq!(sent.attachments[0].mime, "application/pdf");
        assert_eq!(sent.attachments[0].data, b"hello");
        assert_eq!(sent.body.trim_end(), "see file");
    }

    #[test]
    fn a_message_without_sender_or_recipient_is_refused() {
        assert_eq!(
            parse(
                b"To: bob@example.org\r\n\r\nhi",
                &envelope(&["bob@example.org"])
            ),
            Err(Error::NoSender)
        );
        assert_eq!(parse(PLAIN, &[]), Err(Error::NoRecipient));
    }
}
