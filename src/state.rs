//! The record tying each Tuta mail to the Maildir file that holds it.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::message::MailId;

const FILE: &str = "mail.tsv";

/// Where a mail lives in the Maildir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub folder: String,
    pub file: String,
    pub list: String,
}

/// The mails written so far, kept in an append-only file that survives restarts.
pub struct State {
    entries: BTreeMap<MailId, Entry>,
    log: File,
}

impl State {
    /// Loads the record from a directory, creating it when absent.
    ///
    /// A final line cut short by a crash is dropped. The file is rewritten
    /// without the lines that later ones replaced.
    ///
    /// # Errors
    ///
    /// Fails when the directory or the file cannot be read or written.
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let path = dir.join(FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let mut entries = BTreeMap::new();
        let text = String::from_utf8_lossy(&bytes);
        for line in text
            .split_inclusive('\n')
            .filter_map(|l| l.strip_suffix('\n'))
        {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields[..] {
                [id, folder, file, list] => {
                    let entry = Entry {
                        folder: folder.to_owned(),
                        file: file.to_owned(),
                        list: list.to_owned(),
                    };
                    entries.insert(MailId(id.to_owned()), entry);
                }
                [id] if !id.is_empty() => {
                    entries.remove(&MailId(id.to_owned()));
                }
                _ => {}
            }
        }
        let compact: String = entries.iter().map(|(id, entry)| line(id, entry)).collect();
        replace(&path, &compact)?;
        let log = OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        Ok(Self { entries, log })
    }

    /// Where a mail was written, if it was.
    pub fn get(&self, id: &MailId) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// Every recorded mail, in id order.
    pub fn all(&self) -> impl Iterator<Item = (&MailId, &Entry)> {
        self.entries.iter()
    }

    /// Records where a mail was written, replacing an earlier record.
    ///
    /// # Errors
    ///
    /// Fails when a field holds a tab or a line break, or the file cannot be written.
    pub fn set(&mut self, id: &MailId, entry: Entry) -> Result<()> {
        let text = line(id, &entry);
        if text.matches(['\t', '\n']).count() != 4 {
            bail!("unusable record {text:?}");
        }
        self.log
            .write_all(text.as_bytes())
            .context("cannot append to the mail record")?;
        self.entries.insert(id.clone(), entry);
        Ok(())
    }

    /// Forgets a mail, which is a no-op when it was never recorded.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn remove(&mut self, id: &MailId) -> Result<()> {
        if !self.entries.contains_key(id) {
            return Ok(());
        }
        self.log
            .write_all(format!("{}\n", id.0).as_bytes())
            .context("cannot append to the mail record")?;
        self.entries.remove(id);
        Ok(())
    }
}

fn line(id: &MailId, entry: &Entry) -> String {
    format!(
        "{}\t{}\t{}\t{}\n",
        id.0, entry.folder, entry.file, entry.list
    )
}

fn replace(path: &Path, text: &str) -> Result<()> {
    let temporary: PathBuf = path.with_extension("tmp");
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> MailId {
        MailId(text.to_owned())
    }

    fn entry(folder: &str, file: &str) -> Entry {
        Entry {
            folder: folder.to_owned(),
            file: file.to_owned(),
            list: "l".to_owned(),
        }
    }

    #[test]
    fn records_survive_a_reopen_and_the_last_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        assert_eq!(state.get(&id("A1")), None);
        state.set(&id("A1"), entry("INBOX", "one")).unwrap();
        state.set(&id("A2"), entry("Sent", "two")).unwrap();
        state.set(&id("A1"), entry("Archive", "three")).unwrap();
        drop(state);
        let state = State::open(dir.path()).unwrap();
        let entry = state.get(&id("A1")).unwrap();
        assert_eq!(
            (entry.folder.as_str(), entry.file.as_str()),
            ("Archive", "three")
        );
        assert_eq!(state.get(&id("A2")).unwrap().file, "two");
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "A1\tArchive\tthree\tl\nA2\tSent\ttwo\tl\n");
    }

    #[test]
    fn a_torn_last_line_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), "A1\tINBOX\tone\tl\nA2\tINBOX\ttw").unwrap();
        let mut state = State::open(dir.path()).unwrap();
        assert!(state.get(&id("A1")).is_some());
        assert_eq!(state.get(&id("A2")), None);
        state.set(&id("A3"), entry("INBOX", "three")).unwrap();
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "A1\tINBOX\tone\tl\nA3\tINBOX\tthree\tl\n");
    }

    #[test]
    fn a_removed_mail_stays_removed_after_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        state.set(&id("A1"), entry("INBOX", "one")).unwrap();
        state.set(&id("A2"), entry("INBOX", "two")).unwrap();
        state.remove(&id("A1")).unwrap();
        state.remove(&id("nope")).unwrap();
        assert_eq!(state.get(&id("A1")), None);
        drop(state);
        let state = State::open(dir.path()).unwrap();
        assert_eq!(state.get(&id("A1")), None);
        assert!(state.get(&id("A2")).is_some());
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "A2\tINBOX\ttwo\tl\n");
    }

    #[test]
    fn fields_cannot_break_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        assert!(state.set(&id("A1"), entry("a\tb", "x")).is_err());
        assert!(state.set(&id("A1"), entry("a", "x\ny")).is_err());
        assert_eq!(state.get(&id("A1")), None);
    }
}
