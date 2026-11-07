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
        let entries: BTreeMap<MailId, Entry> = String::from_utf8_lossy(&bytes)
            .split_inclusive('\n')
            .filter_map(|line| line.strip_suffix('\n'))
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let (id, folder, file) = (fields.next()?, fields.next()?, fields.next()?);
                fields.next().is_none().then(|| {
                    (
                        MailId(id.to_owned()),
                        Entry {
                            folder: folder.to_owned(),
                            file: file.to_owned(),
                        },
                    )
                })
            })
            .collect();
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

    /// Records where a mail was written, replacing an earlier record.
    ///
    /// # Errors
    ///
    /// Fails when a field holds a tab or a line break, or the file cannot be written.
    pub fn set(&mut self, id: &MailId, folder: &str, file: &str) -> Result<()> {
        let entry = Entry {
            folder: folder.to_owned(),
            file: file.to_owned(),
        };
        let text = line(id, &entry);
        if text.matches(['\t', '\n']).count() != 3 {
            bail!("unusable record {text:?}");
        }
        self.log
            .write_all(text.as_bytes())
            .context("cannot append to the mail record")?;
        self.entries.insert(id.clone(), entry);
        Ok(())
    }
}

fn line(id: &MailId, entry: &Entry) -> String {
    format!("{}\t{}\t{}\n", id.0, entry.folder, entry.file)
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

    #[test]
    fn records_survive_a_reopen_and_the_last_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        assert_eq!(state.get(&id("A1")), None);
        state.set(&id("A1"), "INBOX", "one").unwrap();
        state.set(&id("A2"), "Sent", "two").unwrap();
        state.set(&id("A1"), "Archive", "three").unwrap();
        drop(state);
        let state = State::open(dir.path()).unwrap();
        let entry = state.get(&id("A1")).unwrap();
        assert_eq!(
            (entry.folder.as_str(), entry.file.as_str()),
            ("Archive", "three")
        );
        assert_eq!(state.get(&id("A2")).unwrap().file, "two");
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "A1\tArchive\tthree\nA2\tSent\ttwo\n");
    }

    #[test]
    fn a_torn_last_line_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), "A1\tINBOX\tone\nA2\tINBOX\ttw").unwrap();
        let mut state = State::open(dir.path()).unwrap();
        assert!(state.get(&id("A1")).is_some());
        assert_eq!(state.get(&id("A2")), None);
        state.set(&id("A3"), "INBOX", "three").unwrap();
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "A1\tINBOX\tone\nA3\tINBOX\tthree\n");
    }

    #[test]
    fn fields_cannot_break_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        assert!(state.set(&id("A1"), "a\tb", "x").is_err());
        assert!(state.set(&id("A1"), "a", "x\ny").is_err());
        assert_eq!(state.get(&id("A1")), None);
    }
}
