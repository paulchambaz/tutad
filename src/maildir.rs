//! Writes messages into a Maildir++ tree the way a mail delivery agent does.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::message::Message;

const HEAD: u64 = 8192;

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

    /// Deletes a mail's file, wherever Dovecot has renamed it, and does nothing if it is gone.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder or when the file cannot be removed.
    pub fn remove(&self, folder: &str, file: &str) -> Result<()> {
        let Some((path, _)) = self.locate(folder, file)? else {
            return Ok(());
        };
        fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))
    }

    /// Sets or clears a mail's read flag and returns its new file name, or `None` if it is gone.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder or when the file cannot be renamed.
    pub fn set_seen(&self, folder: &str, file: &str, seen: bool) -> Result<Option<String>> {
        let Some((path, name)) = self.locate(folder, file)? else {
            return Ok(None);
        };
        let (stem, flags) = split(&name);
        if flags.contains('S') == seen {
            return Ok(Some(name));
        }
        let mut letters: Vec<char> = flags.chars().filter(|&letter| letter != 'S').collect();
        if seen {
            letters.push('S');
        }
        letters.sort_unstable();
        let renamed = format!("{stem}:2,{}", letters.into_iter().collect::<String>());
        let target = self.folder(folder)?.join("cur").join(&renamed);
        fs::rename(&path, &target).with_context(|| format!("cannot rename {}", path.display()))?;
        Ok(Some(renamed))
    }

    /// Moves a mail to another folder and returns its file name, or `None` if it is gone.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder or when the file cannot be renamed.
    pub fn move_to(&self, from: &str, file: &str, to: &str) -> Result<Option<String>> {
        let Some((path, name)) = self.locate(from, file)? else {
            return Ok(None);
        };
        let subdirectory = path
            .parent()
            .and_then(Path::file_name)
            .with_context(|| format!("cannot place {}", path.display()))?;
        let directory = self.folder(to)?;
        for part in ["tmp", "new", "cur"] {
            let made = directory.join(part);
            fs::create_dir_all(&made)
                .with_context(|| format!("cannot create {}", made.display()))?;
        }
        let target = directory.join(subdirectory).join(&name);
        fs::rename(&path, &target).with_context(|| format!("cannot rename {}", path.display()))?;
        Ok(Some(name))
    }

    /// Renames a folder and returns false when the new name is already taken.
    ///
    /// A folder that is not there counts as renamed, since it holds nothing.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder name or when the directory cannot be renamed.
    pub fn rename_folder(&self, from: &str, to: &str) -> Result<bool> {
        let (source, target) = (self.folder(from)?, self.folder(to)?);
        if !source.exists() {
            return Ok(true);
        }
        if target.exists() {
            return Ok(false);
        }
        fs::rename(&source, &target)
            .with_context(|| format!("cannot rename {}", source.display()))?;
        Ok(true)
    }

    /// Lists the file names of a folder, or `None` when the folder is not there.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder or when a directory cannot be read.
    pub fn files(&self, folder: &str) -> Result<Option<Vec<String>>> {
        let directory = self.folder(folder)?;
        let mut names = Vec::new();
        let mut found = false;
        for part in ["new", "cur"] {
            let entries = match fs::read_dir(directory.join(part)) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot read {}", directory.display()))
                }
            };
            found = true;
            for entry in entries {
                let entry =
                    entry.with_context(|| format!("cannot read {}", directory.display()))?;
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Ok(found.then_some(names))
    }

    /// Reads the first bytes of a mail, enough for its headers, or `None` if it is gone.
    ///
    /// # Errors
    ///
    /// Fails on an unusable folder or when the file cannot be read.
    pub fn head(&self, folder: &str, file: &str) -> Result<Option<Vec<u8>>> {
        let Some((path, _)) = self.locate(folder, file)? else {
            return Ok(None);
        };
        let opened = match File::open(&path) {
            Ok(opened) => opened,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let mut bytes = Vec::new();
        opened
            .take(HEAD)
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read {}", path.display()))?;
        Ok(Some(bytes))
    }

    fn locate(&self, folder: &str, file: &str) -> Result<Option<(PathBuf, String)>> {
        let directory = self.folder(folder)?;
        let (stem, _) = split(file);
        for part in ["new", "cur"] {
            let entries = match fs::read_dir(directory.join(part)) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot read {}", directory.display()))
                }
            };
            for entry in entries {
                let name = entry
                    .with_context(|| format!("cannot read {}", directory.display()))?
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                let rest = name.strip_prefix(stem);
                if rest.is_some_and(|rest| rest.is_empty() || rest.starts_with([':', ','])) {
                    return Ok(Some((directory.join(part).join(&name), name)));
                }
            }
        }
        Ok(None)
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

/// The part of a file name that never changes, whatever flags or sizes get added.
pub fn key(file: &str) -> &str {
    file.split([':', ',']).next().unwrap_or(file)
}

/// Whether a file name carries the read flag.
pub fn is_seen(file: &str) -> bool {
    split(file).1.contains('S')
}

fn split(file: &str) -> (&str, &str) {
    match file.split_once(':') {
        Some((stem, info)) => (stem, info.strip_prefix("2,").unwrap_or("")),
        None => (file, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Address, Attachment, MailId};
    use crate::render::render;
    use crate::state::{Entry, State};

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

    #[test]
    fn a_mail_is_found_after_dovecot_renamed_it() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let name = maildir
            .deliver("INBOX", &message("A1", true), "x\n")
            .unwrap();
        fs::rename(
            dir.path().join("new").join(&name),
            dir.path().join("cur/1767225600.A1.tutad,S=2:2,F"),
        )
        .unwrap();
        let moved = maildir.set_seen("INBOX", &name, true).unwrap().unwrap();
        assert_eq!(moved, "1767225600.A1.tutad,S=2:2,FS");
        assert!(dir.path().join("cur").join(&moved).exists());
        assert_eq!(
            maildir.set_seen("INBOX", &moved, true).unwrap(),
            Some(moved)
        );
        maildir.remove("INBOX", &name).unwrap();
        assert!(names(&dir.path().join("cur")).is_empty());
        maildir.remove("INBOX", &name).unwrap();
        assert_eq!(maildir.set_seen("INBOX", &name, true).unwrap(), None);
    }

    #[test]
    fn marking_seen_moves_a_new_mail_to_cur() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let name = maildir
            .deliver("INBOX", &message("A1", true), "x\n")
            .unwrap();
        let seen = maildir.set_seen("INBOX", &name, true).unwrap().unwrap();
        assert_eq!(seen, "1767225600.A1.tutad:2,S");
        assert!(names(&dir.path().join("new")).is_empty());
        assert!(dir.path().join("cur").join(seen).exists());
    }

    #[test]
    fn the_read_flag_can_be_cleared_and_other_flags_stay() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let name = maildir
            .deliver("INBOX", &message("A1", false), "x\n")
            .unwrap();
        assert_eq!(name, "1767225600.A1.tutad:2,S");
        let flagged = "1767225600.A1.tutad:2,FS";
        fs::rename(
            dir.path().join("cur").join(&name),
            dir.path().join("cur").join(flagged),
        )
        .unwrap();
        let unseen = maildir.set_seen("INBOX", &name, false).unwrap().unwrap();
        assert_eq!(unseen, "1767225600.A1.tutad:2,F");
        assert_eq!(
            maildir.set_seen("INBOX", &unseen, false).unwrap(),
            Some(unseen.clone())
        );
        let fresh = maildir
            .deliver("INBOX", &message("A2", true), "y\n")
            .unwrap();
        assert_eq!(
            maildir.set_seen("INBOX", &fresh, false).unwrap(),
            Some(fresh)
        );
        assert!(dir.path().join("new/1767225600.A2.tutad").exists());
    }

    #[test]
    fn moving_keeps_the_file_and_its_state() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let read = maildir
            .deliver("INBOX", &message("A1", false), "x\n")
            .unwrap();
        let unread = maildir
            .deliver("INBOX", &message("A2", true), "y\n")
            .unwrap();
        assert_eq!(
            maildir.move_to("INBOX", &read, "Work.Clients").unwrap(),
            Some(read.clone())
        );
        maildir.move_to("INBOX", &unread, "Archive").unwrap();
        assert!(dir.path().join(".Work.Clients/cur").join(&read).exists());
        assert!(dir.path().join(".Archive/new").join(&unread).exists());
        assert!(names(&dir.path().join("cur")).is_empty());
        assert_eq!(maildir.move_to("INBOX", &read, "Archive").unwrap(), None);
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
            let entry = Entry {
                folder: folder.to_owned(),
                file: name,
                list: "l".to_owned(),
            };
            state.set(&mail.id, entry).unwrap();
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

    #[test]
    fn a_folder_is_renamed_unless_the_new_name_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(dir.path());
        let one = message("A1", false);
        let two = message("A2", false);
        maildir.deliver("Work", &one, "x\n").unwrap();
        maildir.deliver("Taken", &two, "x\n").unwrap();
        assert!(maildir.rename_folder("Work", "Jobs").unwrap());
        assert!(!dir.path().join(".Work").exists());
        assert_eq!(
            names(&dir.path().join(".Jobs/cur")),
            ["1767225600.A1.tutad:2,S"]
        );
        assert!(!maildir.rename_folder("Jobs", "Taken").unwrap());
        assert!(dir.path().join(".Jobs").exists());
        assert!(maildir.rename_folder("Nothing", "Else").unwrap());
        assert!(maildir.rename_folder("Jobs", "a/b").is_err());
    }
}
