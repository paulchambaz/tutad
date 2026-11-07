//! The Message-IDs of mails Tuta accepted, so the client's own Sent copy can be dropped.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use mail_parser::MessageParser;

use crate::maildir::{self, Maildir};
use crate::state::State;

const FILE: &str = "sent.tsv";
const FOLDER: &str = "Sent";
const KEPT_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Notes that Tuta accepted the mail with this Message-ID.
///
/// # Errors
///
/// Fails when the ID is empty or holds a tab or a line break, or the file cannot be written.
pub fn record(dir: &Path, id: &str, now: u64) -> Result<()> {
    if id.is_empty() || id.contains(['\t', '\n', '\r']) {
        bail!("unusable Message-ID");
    }
    let path = dir.join(FILE);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(format!("{now}\t{id}\n").as_bytes()))
        .with_context(|| format!("cannot append to {}", path.display()))
}

/// The Message-IDs noted in the last week.
///
/// # Errors
///
/// Fails when the file exists and cannot be read.
pub fn load(dir: &Path, now: u64) -> Result<HashSet<String>> {
    Ok(kept(dir, now)?.into_iter().map(|(_, id)| id).collect())
}

/// Forgets what is older than a week or unreadable.
///
/// # Errors
///
/// Fails when the file cannot be read or replaced.
pub fn compact(dir: &Path, now: u64) -> Result<()> {
    let text: String = kept(dir, now)?
        .into_iter()
        .map(|(seconds, id)| format!("{seconds}\t{id}\n"))
        .collect();
    let path = dir.join(FILE);
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("cannot replace {}", path.display()))
}

fn kept(dir: &Path, now: u64) -> Result<Vec<(u64, String)>> {
    let path = dir.join(FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    Ok(text
        .split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
        .filter_map(|line| {
            let (seconds, id) = line.split_once('\t')?;
            Some((seconds.parse::<u64>().ok()?, id.to_owned()))
        })
        .filter(|(seconds, _)| now.saturating_sub(*seconds) < KEPT_SECONDS)
        .collect())
}

fn message_id(head: &[u8]) -> Option<String> {
    MessageParser::default()
        .parse_headers(head)?
        .message_id()
        .map(str::to_owned)
}

/// Removes the client's copies from Sent and returns how many.
///
/// Only a file Tuta did not deliver is removed, and only when its Message-ID is
/// among `ids`. Tuta's own copy stays, whatever ID it carries. `heads` holds
/// the ID of each file already read, so a file is read once.
///
/// # Errors
///
/// Fails when the folder cannot be read or a copy cannot be removed.
pub fn drop_copies(
    maildir: &Maildir,
    state: &State,
    heads: &mut HashMap<String, Option<String>>,
    ids: &HashSet<String>,
) -> Result<usize> {
    let Some(names) = maildir.files(FOLDER)? else {
        heads.clear();
        return Ok(0);
    };
    let delivered: HashSet<&str> = state
        .all()
        .filter(|(_, entry)| entry.folder == FOLDER)
        .map(|(_, entry)| maildir::key(&entry.file))
        .collect();
    let present: HashSet<&str> = names.iter().map(|name| maildir::key(name)).collect();
    heads.retain(|key, _| present.contains(key.as_str()));
    let mut dropped = 0;
    for name in &names {
        let key = maildir::key(name);
        if delivered.contains(key) {
            continue;
        }
        if !heads.contains_key(key) {
            let Some(head) = maildir.head(FOLDER, name)? else {
                continue;
            };
            heads.insert(key.to_owned(), message_id(&head));
        }
        if heads
            .get(key)
            .and_then(Option::as_deref)
            .is_some_and(|id| ids.contains(id))
        {
            maildir.remove(FOLDER, name)?;
            heads.remove(key);
            dropped += 1;
        }
    }
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Address, MailId, Message};
    use crate::state::Entry;

    fn ids(words: &[&str]) -> HashSet<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    fn message(id: &str, message_id: &str) -> Message {
        Message {
            id: MailId(id.to_owned()),
            message_id: Some(message_id.to_owned()),
            date_ms: 1_767_225_600_000,
            subject: "s".to_owned(),
            from: Address {
                name: String::new(),
                address: "a@example.org".to_owned(),
            },
            to: Vec::new(),
            cc: Vec::new(),
            reply_to: Vec::new(),
            html: "x".to_owned(),
            attachments: Vec::new(),
            unread: false,
        }
    }

    fn tuta_copy(maildir: &Maildir, state: &mut State, id: &str, message_id: &str) {
        let mail = message(id, message_id);
        let text = format!("Message-ID: <{message_id}>\n\nx\n");
        let file = maildir.deliver(FOLDER, &mail, &text).unwrap();
        let entry = Entry {
            folder: FOLDER.to_owned(),
            file,
            list: "l".to_owned(),
        };
        state.set(&mail.id, entry).unwrap();
    }

    fn client_copy(dir: &Path, file: &str, message_id: &str) {
        let cur = dir.join("mail/.Sent/cur");
        fs::create_dir_all(&cur).unwrap();
        fs::write(
            cur.join(file),
            format!("Subject: s\nMessage-ID: <{message_id}>\n\nbody\n"),
        )
        .unwrap();
    }

    fn names(maildir: &Maildir) -> Vec<String> {
        let mut names = maildir.files(FOLDER).unwrap().unwrap();
        names.sort();
        names
    }

    #[test]
    fn ids_are_noted_and_forgotten_after_a_week() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), "old@x", 1_000).unwrap();
        record(dir.path(), "new@x", 900_000).unwrap();
        let now = 1_000 + KEPT_SECONDS;
        assert_eq!(load(dir.path(), now).unwrap(), ids(&["new@x"]));
        compact(dir.path(), now).unwrap();
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "900000\tnew@x\n");
    }

    #[test]
    fn nothing_noted_is_an_empty_set_and_a_torn_line_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), 5).unwrap().is_empty());
        fs::write(dir.path().join(FILE), "5\ta@x\n6\tb@x").unwrap();
        assert_eq!(load(dir.path(), 7).unwrap(), ids(&["a@x"]));
        assert!(record(dir.path(), "a\tb", 1).is_err());
        assert!(record(dir.path(), "", 1).is_err());
    }

    #[test]
    fn the_client_copy_goes_and_tuta_copy_stays() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(&dir.path().join("mail"));
        let mut state = State::open(&dir.path().join("state")).unwrap();
        tuta_copy(&maildir, &mut state, "A1", "same@x");
        client_copy(dir.path(), "1767225601.M1.host:2,S", "same@x");
        client_copy(dir.path(), "1767225602.M2.host:2,S", "other@x");
        let mut heads = HashMap::new();
        let dropped = drop_copies(&maildir, &state, &mut heads, &ids(&["same@x"])).unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(
            names(&maildir),
            ["1767225600.A1.tutad:2,S", "1767225602.M2.host:2,S"]
        );
    }

    #[test]
    fn a_copy_that_comes_before_its_id_goes_once_the_id_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(&dir.path().join("mail"));
        let state = State::open(&dir.path().join("state")).unwrap();
        client_copy(dir.path(), "1767225601.M1.host:2,S", "late@x");
        let mut heads = HashMap::new();
        assert_eq!(
            drop_copies(&maildir, &state, &mut heads, &ids(&[])).unwrap(),
            0
        );
        assert_eq!(heads.len(), 1);
        assert_eq!(
            drop_copies(&maildir, &state, &mut heads, &ids(&["late@x"])).unwrap(),
            1
        );
        assert!(names(&maildir).is_empty());
        assert!(heads.is_empty());
    }

    #[test]
    fn a_renamed_copy_is_still_found_and_other_folders_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(&dir.path().join("mail"));
        let state = State::open(&dir.path().join("state")).unwrap();
        client_copy(dir.path(), "1767225601.M1.host,S=120:2,S", "r@x");
        let inbox = dir.path().join("mail/cur");
        fs::create_dir_all(&inbox).unwrap();
        fs::write(inbox.join("1.M9.host:2,S"), "Message-ID: <r@x>\n\nx\n").unwrap();
        let mut heads = HashMap::new();
        let dropped = drop_copies(&maildir, &state, &mut heads, &ids(&["r@x"])).unwrap();
        assert_eq!(dropped, 1);
        assert!(inbox.join("1.M9.host:2,S").exists());
    }

    #[test]
    fn a_missing_sent_folder_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let maildir = Maildir::open(&dir.path().join("mail"));
        let state = State::open(&dir.path().join("state")).unwrap();
        let mut heads = HashMap::from([("k".to_owned(), None)]);
        assert_eq!(
            drop_copies(&maildir, &state, &mut heads, &ids(&["x"])).unwrap(),
            0
        );
        assert!(heads.is_empty());
    }
}
