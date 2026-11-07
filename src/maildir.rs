//! Writes messages into a Maildir++ tree the way a mail delivery agent does.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::message::Message;

/// A Maildir++ tree, with the inbox at the root and other folders as `.Name` below it.
pub struct Maildir {
    root: PathBuf,
}

impl Maildir {
    /// A Maildir at a path, which is created on first delivery.
    pub fn open(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Writes a message to a folder and returns its file name.
    ///
    /// The name depends only on the mail, so delivering the same mail twice
    /// leaves one file. A mail already read goes straight to `cur`. The caller
    /// must not deliver a mail again after its file was renamed or removed.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder or mail id, or when the files cannot be written.
    pub fn deliver(&self, folder: &str, message: &Message, text: &str) -> Result<String> {
        let id = &message.id.0;
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("unusable mail id {id:?}");
        }
        let directory = self.folder(folder)?;
        let stem = format!("{}.{id}.tutad", message.date_ms / 1000);
        let (subdirectory, name) = if message.unread {
            ("new", stem.clone())
        } else {
            ("cur", format!("{stem}:2,S"))
        };
        for part in ["tmp", "new", "cur"] {
            let path = directory.join(part);
            fs::create_dir_all(&path)
                .with_context(|| format!("cannot create {}", path.display()))?;
        }
        let temporary = directory.join("tmp").join(&stem);
        let mut file = File::create(&temporary)
            .with_context(|| format!("cannot create {}", temporary.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        let target = directory.join(subdirectory).join(&name);
        match fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot create {}", target.display()));
            }
        }
        fs::remove_file(&temporary)
            .with_context(|| format!("cannot remove {}", temporary.display()))?;
        Ok(name)
    }

    fn folder(&self, name: &str) -> Result<PathBuf> {
        if name == "INBOX" {
            return Ok(self.root.clone());
        }
        let usable = name.split('.').all(|part| !part.is_empty())
            && !name.contains(|c: char| c == '/' || c.is_control());
        if !usable {
            bail!("unusable folder name {name:?}");
        }
        Ok(self.root.join(format!(".{name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Address, Attachment, MailId};
    use crate::render::render;
    use crate::state::State;

    fn message(id: &str, unread: bool) -> Message {
        Message {
            id: MailId(id.to_owned()),
            message_id: None,
            date_ms: 1_767_225_600_000,
            subject: "s".to_owned(),
            from: Address {
                name: String::new(),
                address: "a@example.org".to_owned(),
            },
            to: Vec::new(),
            cc: Vec::new(),
            reply_to: Vec::new(),
            html: "<p>x</p>".to_owned(),
            attachments: Vec::new(),
            unread,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn unread_mail_lands_in_new_and_read_mail_in_cur() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let unread = message("A1", true);
        let text = render(&unread);
        let name = maildir.deliver("INBOX", &unread, &text).unwrap();
        assert_eq!(name, "1767225600.A1.tutad");
        assert_eq!(
            fs::read_to_string(dir.path().join("new").join(&name)).unwrap(),
            text
        );
        let read = message("A2", false);
        let name = maildir.deliver("INBOX", &read, &render(&read)).unwrap();
        assert_eq!(name, "1767225600.A2.tutad:2,S");
        assert!(dir.path().join("cur").join(&name).exists());
        assert!(names(&dir.path().join("tmp")).is_empty());
    }

    #[test]
    fn delivering_twice_leaves_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let mail = message("A1", true);
        let first = maildir.deliver("INBOX", &mail, "one\n").unwrap();
        let second = maildir.deliver("INBOX", &mail, "two\n").unwrap();
        assert_eq!(first, second);
        assert_eq!(names(&dir.path().join("new")), std::slice::from_ref(&first));
        assert_eq!(
            fs::read_to_string(dir.path().join("new").join(first)).unwrap(),
            "one\n"
        );
        assert!(names(&dir.path().join("tmp")).is_empty());
    }

    #[test]
    fn other_folders_are_dot_directories() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let mail = message("A1", true);
        let name = maildir.deliver("Work.Clients", &mail, "x\n").unwrap();
        assert!(dir.path().join(".Work.Clients/new").join(name).exists());
    }

    #[test]
    fn hostile_names_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        for folder in ["", ".", "..", "a/b", "../x", "a..b", ".a", "a.", "a\nb"] {
            assert!(
                maildir.deliver(folder, &message("A1", true), "x").is_err(),
                "{folder:?}"
            );
        }
        for id in ["", "../x", "a b", "a/b", "é"] {
            assert!(
                maildir.deliver("INBOX", &message(id, true), "x").is_err(),
                "{id:?}"
            );
        }
        assert!(names(dir.path()).is_empty());
    }

    fn fixtures() -> Vec<(&'static str, Message)> {
        let mut plain = message("P1", true);
        plain.subject = "Plain".to_owned();
        plain.html = "line one<br>line two".to_owned();
        let mut attached = message("P2", false);
        attached.subject = "Café, déjà vu".to_owned();
        attached.from.name = "Zoë O'Neil".to_owned();
        attached.message_id = Some("<real@example.org>".to_owned());
        attached.attachments = vec![Attachment {
            name: "ré.bin".to_owned(),
            mime: "application/octet-stream".to_owned(),
            cid: None,
            data: (0..=255).collect(),
        }];
        let mut nested = message("P3", true);
        nested.html = "<b>nested</b>".to_owned();
        vec![
            ("INBOX", plain),
            ("INBOX", attached),
            ("Work.Clients", nested),
        ]
    }

    #[test]
    fn fixtures_are_rendered_delivered_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(&dir.path().join("mail"));
        let mut state = State::open(&dir.path().join("state")).unwrap();
        for (folder, mail) in fixtures() {
            let name = maildir.deliver(folder, &mail, &render(&mail)).unwrap();
            state.set(&mail.id, folder, &name).unwrap();
        }
        let mail = dir.path().join("mail");
        assert_eq!(names(&mail.join("new")), ["1767225600.P1.tutad"]);
        assert_eq!(names(&mail.join("cur")), ["1767225600.P2.tutad:2,S"]);
        assert_eq!(
            names(&mail.join(".Work.Clients/new")),
            ["1767225600.P3.tutad"]
        );
        let entry = state.get(&MailId("P3".to_owned())).unwrap();
        assert_eq!(entry.folder, "Work.Clients");
        let text = fs::read_to_string(mail.join("cur/1767225600.P2.tutad:2,S")).unwrap();
        assert!(text.contains("Message-ID: <real@example.org>\n"));
        assert!(text.contains("multipart/mixed"));
    }
}
