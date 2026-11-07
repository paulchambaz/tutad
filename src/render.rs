//! Turns a Tuta mail into the text of an RFC 2822 message.

use std::fmt::Write;

use base64::prelude::BASE64_STANDARD;
use base64::Engine;

use crate::message::{Address, Attachment, Message};

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The message as it is written to a Maildir, with LF line ends.
pub fn render(message: &Message) -> String {
    let html = message.html.replace("\r\n", "\n");
    let plain = html_to_text(&html);
    let mut out = String::new();
    push_header(&mut out, "Date", &date(message.date_ms));
    push_header(&mut out, "From", &mailbox(&message.from));
    push_header(&mut out, "Subject", &encode_word(&message.subject));
    for (name, list) in [
        ("To", &message.to),
        ("Cc", &message.cc),
        ("Reply-To", &message.reply_to),
    ] {
        if !list.is_empty() {
            let list: Vec<String> = list.iter().map(mailbox).collect();
            push_header(&mut out, name, &list.join(", "));
        }
    }
    push_header(&mut out, "Message-ID", &message_id(message));
    push_header(&mut out, "MIME-Version", "1.0");
    let alternative = format!("=_tutad_alt_{}", message.id.0);
    if message.attachments.is_empty() {
        push_alternative(&mut out, &alternative, &plain, &html);
        return out;
    }
    let mixed = format!("=_tutad_{}", message.id.0);
    push_header(
        &mut out,
        "Content-Type",
        &format!("multipart/mixed; boundary=\"{mixed}\""),
    );
    let _ = write!(out, "\n--{mixed}\n");
    push_alternative(&mut out, &alternative, &plain, &html);
    for attachment in &message.attachments {
        let _ = writeln!(out, "--{mixed}");
        push_attachment(&mut out, attachment);
    }
    let _ = writeln!(out, "--{mixed}--");
    out
}

fn push_header(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "{name}: {value}");
}

fn push_alternative(out: &mut String, boundary: &str, plain: &str, html: &str) {
    push_header(
        out,
        "Content-Type",
        &format!("multipart/alternative; boundary=\"{boundary}\""),
    );
    for (kind, body) in [("plain", plain), ("html", html)] {
        let _ = write!(
            out,
            "\n--{boundary}\nContent-Type: text/{kind}; charset=utf-8\nContent-Transfer-Encoding: 8bit\n\n{body}"
        );
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
    let _ = write!(out, "\n--{boundary}--\n");
}

fn push_attachment(out: &mut String, attachment: &Attachment) {
    let mime: String = attachment
        .mime
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .collect();
    let mime = if mime.is_empty() {
        "application/octet-stream"
    } else {
        &mime
    };
    let name = parameter(&attachment.name);
    let disposition = if attachment.cid.is_some() {
        "inline"
    } else {
        "attachment"
    };
    push_header(out, "Content-Type", &format!("{mime}; name=\"{name}\""));
    push_header(out, "Content-Transfer-Encoding", "base64");
    push_header(
        out,
        "Content-Disposition",
        &format!("{disposition}; filename=\"{name}\""),
    );
    if let Some(cid) = &attachment.cid {
        push_header(out, "Content-ID", &format!("<{}>", bare(cid)));
    }
    out.push('\n');
    let encoded = BASE64_STANDARD.encode(&attachment.data);
    for line in encoded.as_bytes().chunks(76) {
        out.extend(line.iter().map(|&byte| char::from(byte)));
        out.push('\n');
    }
}

fn message_id(message: &Message) -> String {
    let given = message.message_id.as_deref().map(bare).unwrap_or_default();
    if given.is_empty() {
        format!("<{}@tutad.invalid>", message.id.0)
    } else {
        format!("<{given}>")
    }
}

fn bare(id: &str) -> String {
    id.chars()
        .filter(|c| !c.is_control() && !c.is_whitespace() && !matches!(c, '<' | '>'))
        .collect()
}

fn mailbox(address: &Address) -> String {
    let email: String = address
        .address
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace() && !matches!(c, '<' | '>' | ','))
        .collect();
    let name = address.name.trim();
    if name.is_empty() {
        return email;
    }
    let quoted = name.is_ascii()
        && !name.contains(['\r', '\n'])
        && name.contains(|c| "()<>[]:;@\\,.\"".contains(c));
    if quoted {
        format!("\"{}\" <{email}>", escape(name))
    } else {
        format!("{} <{email}>", encode_word(name))
    }
}

fn parameter(value: &str) -> String {
    if value.is_ascii() && !value.contains(['\r', '\n']) {
        escape(value)
    } else {
        encode_word(value)
    }
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn encode_word(value: &str) -> String {
    if value.is_ascii() && !value.contains(['\r', '\n']) {
        value.to_owned()
    } else {
        format!("=?UTF-8?B?{}?=", BASE64_STANDARD.encode(value))
    }
}

fn date(millis: u64) -> String {
    let seconds = millis / 1000;
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} +0000",
        DAYS[((days + 4) % 7) as usize],
        MONTHS[(month - 1) as usize],
        day_seconds / 3600,
        day_seconds % 3600 / 60,
        day_seconds % 60,
    )
}

fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            rest = "";
            break;
        };
        let inner = &after[..close];
        let name = inner
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &after[close + 1..];
        match name.as_str() {
            "script" | "style" if !inner.starts_with('/') => {
                let end = format!("</{name}");
                rest = rest
                    .to_ascii_lowercase()
                    .find(&end)
                    .map_or("", |index| &rest[index..]);
            }
            "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                text.push('\n');
            }
            _ => {}
        }
    }
    text.push_str(rest);
    tidy(&decode_entities(&text))
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest
            .find(';')
            .filter(|&semi| semi <= 10)
            .and_then(|semi| entity(&rest[1..semi]).map(|c| (c, semi)));
        if let Some((c, semi)) = decoded {
            out.push(c);
            rest = &rest[semi + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = true;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MailId;

    fn person(name: &str, address: &str) -> Address {
        Address {
            name: name.to_owned(),
            address: address.to_owned(),
        }
    }

    fn message() -> Message {
        Message {
            id: MailId("O1qC7an--3-0".to_owned()),
            message_id: Some("<abc@example.org>".to_owned()),
            date_ms: 1_767_225_600_000,
            subject: "Hello".to_owned(),
            from: person("Alice", "alice@example.org"),
            to: vec![person("", "paul@tutanota.com")],
            cc: Vec::new(),
            reply_to: Vec::new(),
            html: "<p>Hi &amp; bye</p>".to_owned(),
            attachments: Vec::new(),
            unread: true,
        }
    }

    #[test]
    fn dates_are_in_utc() {
        assert_eq!(date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(date(951_782_400_000), "Tue, 29 Feb 2000 00:00:00 +0000");
        assert_eq!(date(1_767_225_600_000), "Thu, 01 Jan 2026 00:00:00 +0000");
        assert_eq!(date(1_794_095_999_999), "Sat, 07 Nov 2026 23:59:59 +0000");
    }

    #[test]
    fn a_plain_mail_has_both_alternatives() {
        let text = render(&message());
        assert!(text.starts_with(
            "Date: Thu, 01 Jan 2026 00:00:00 +0000\nFrom: Alice <alice@example.org>\n"
        ));
        assert!(text.contains("To: paul@tutanota.com\n"));
        assert!(text.contains("Message-ID: <abc@example.org>\n"));
        assert!(text.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(text.contains("\nHi & bye\n"));
        assert!(text.contains("<p>Hi &amp; bye</p>\n"));
        assert!(text.ends_with("--=_tutad_alt_O1qC7an--3-0--\n"));
        assert!(!text.contains('\r'));
    }

    #[test]
    fn rendering_is_stable() {
        assert_eq!(render(&message()), render(&message()));
    }

    #[test]
    fn a_missing_message_id_falls_back_to_the_mail_id() {
        let mut mail = message();
        mail.message_id = None;
        assert!(render(&mail).contains("Message-ID: <O1qC7an--3-0@tutad.invalid>\n"));
        mail.message_id = Some("  <>  ".to_owned());
        assert!(render(&mail).contains("Message-ID: <O1qC7an--3-0@tutad.invalid>\n"));
    }

    #[test]
    fn non_ascii_text_becomes_encoded_words() {
        let mut mail = message();
        mail.subject = "Café".to_owned();
        mail.from = person("Zoë", "zoe@example.org");
        let text = render(&mail);
        assert!(text.contains("Subject: =?UTF-8?B?Q2Fmw6k=?=\n"));
        assert!(text.contains("From: =?UTF-8?B?Wm/Dqw==?= <zoe@example.org>\n"));
    }

    #[test]
    fn a_name_with_a_special_is_quoted() {
        assert_eq!(
            mailbox(&person("Doe, \"Jo\"", "jo@example.org")),
            "\"Doe, \\\"Jo\\\"\" <jo@example.org>"
        );
        assert_eq!(
            mailbox(&person("Jo Doe", "jo@example.org")),
            "Jo Doe <jo@example.org>"
        );
    }

    #[test]
    fn line_breaks_cannot_inject_headers() {
        let mut mail = message();
        mail.subject = "a\r\nBcc: x@example.org".to_owned();
        mail.from = person(
            "b\r\nBcc: y@example.org",
            "c@example.org\r\nBcc: z@example.org",
        );
        let text = render(&mail);
        assert!(!text.contains("\nBcc:"));
        assert!(!text.contains('\r'));
    }

    #[test]
    fn reply_to_and_cc_are_listed() {
        let mut mail = message();
        mail.cc = vec![person("B", "b@example.org"), person("C", "c@example.org")];
        mail.reply_to = vec![person("", "reply@example.org")];
        let text = render(&mail);
        assert!(text.contains("Cc: B <b@example.org>, C <c@example.org>\n"));
        assert!(text.contains("Reply-To: reply@example.org\n"));
    }

    #[test]
    fn attachments_are_wrapped_in_mixed() {
        let mut mail = message();
        mail.attachments = vec![
            Attachment {
                name: "a \"b\".bin".to_owned(),
                mime: "application/pdf".to_owned(),
                cid: None,
                data: vec![7; 100],
            },
            Attachment {
                name: "ï.png".to_owned(),
                mime: String::new(),
                cid: Some("<img1>".to_owned()),
                data: Vec::new(),
            },
        ];
        let text = render(&mail);
        assert!(text.contains("Content-Type: multipart/mixed; boundary=\"=_tutad_O1qC7an--3-0\"\n"));
        assert!(text.contains("Content-Type: application/pdf; name=\"a \\\"b\\\".bin\"\n"));
        assert!(text.contains("Content-Disposition: attachment; filename=\"a \\\"b\\\".bin\"\n"));
        assert!(text
            .contains("Content-Type: application/octet-stream; name=\"=?UTF-8?B?w68ucG5n?=\"\n"));
        assert!(text.contains("Content-Disposition: inline;"));
        assert!(text.contains("Content-ID: <img1>\n"));
        assert!(text.contains(&format!("\n{}\n", "BwcH".repeat(19))));
        assert!(text.ends_with("--=_tutad_O1qC7an--3-0--\n"));
    }

    #[test]
    fn html_becomes_text() {
        assert_eq!(
            html_to_text("<style>p{}</style><div>a&nbsp;b</div><p>&lt;c&gt; &#233;&#x41; &bogus; &</p><script>x</script>"),
            "a b\n\n<c> éA &bogus; &\n"
        );
        assert_eq!(html_to_text(""), "\n");
    }
}
