//! The last event batch applied for each Tuta group, so a restart resumes where it stopped.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const FILE: &str = "events.tsv";
const KEPT_SECONDS: u64 = 44 * 24 * 60 * 60;

/// The last batch applied for each group, kept in an append-only file.
pub struct Cursors {
    batches: BTreeMap<String, (String, u64)>,
    log: File,
}

impl Cursors {
    /// Loads the cursors from a directory, creating the file when absent.
    ///
    /// Tuta keeps batches for 45 days. When the oldest cursor is 44 days old
    /// or more, all are dropped and the caller starts from a full sync.
    ///
    /// # Errors
    ///
    /// Fails when the directory or the file cannot be read or written.
    pub fn open(dir: &Path, now: u64) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let path = dir.join(FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let mut batches = BTreeMap::new();
        let text = String::from_utf8_lossy(&bytes);
        for line in text
            .split_inclusive('\n')
            .filter_map(|l| l.strip_suffix('\n'))
        {
            if let [group, batch, seconds] = line.split('\t').collect::<Vec<_>>()[..] {
                if let Ok(seconds) = seconds.parse::<u64>() {
                    batches.insert(group.to_owned(), (batch.to_owned(), seconds));
                }
            }
        }
        let oldest = batches.values().map(|(_, seconds)| *seconds).min();
        if oldest.is_some_and(|oldest| now.saturating_sub(oldest) >= KEPT_SECONDS) {
            batches.clear();
        }
        let compact: String = batches
            .iter()
            .map(|(group, (batch, seconds))| line(group, batch, *seconds))
            .collect();
        replace(&path, &compact)?;
        let log = OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        Ok(Self { batches, log })
    }

    /// The groups with a cursor and the last batch applied for each.
    pub fn all(&self) -> impl Iterator<Item = (&str, &str)> {
        self.batches
            .iter()
            .map(|(group, (batch, _))| (group.as_str(), batch.as_str()))
    }

    /// Whether no group has a cursor yet.
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Records the last batch applied for a group, replacing an earlier one.
    ///
    /// # Errors
    ///
    /// Fails when a field holds a tab or a line break, or the file cannot be written.
    pub fn set(&mut self, group: &str, batch: &str, now: u64) -> Result<()> {
        let text = line(group, batch, now);
        if group.is_empty() || batch.is_empty() || text.matches(['\t', '\n']).count() != 3 {
            bail!("unusable cursor {text:?}");
        }
        self.log
            .write_all(text.as_bytes())
            .context("cannot append to the event cursors")?;
        self.batches
            .insert(group.to_owned(), (batch.to_owned(), now));
        Ok(())
    }
}

fn line(group: &str, batch: &str, seconds: u64) -> String {
    format!("{group}\t{batch}\t{seconds}\n")
}

fn replace(path: &Path, text: &str) -> Result<()> {
    let temporary: PathBuf = path.with_extension("tmp");
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(cursors: &Cursors) -> Vec<(String, String)> {
        cursors
            .all()
            .map(|(group, batch)| (group.to_owned(), batch.to_owned()))
            .collect()
    }

    #[test]
    fn cursors_survive_a_reopen_and_the_last_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mut cursors = Cursors::open(dir.path(), 1000).unwrap();
        assert!(cursors.is_empty());
        cursors.set("g1", "b1", 1000).unwrap();
        cursors.set("g2", "b2", 1001).unwrap();
        cursors.set("g1", "b3", 1002).unwrap();
        drop(cursors);
        let cursors = Cursors::open(dir.path(), 2000).unwrap();
        assert_eq!(
            pairs(&cursors),
            [("g1".into(), "b3".into()), ("g2".into(), "b2".into())]
        );
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "g1\tb3\t1002\ng2\tb2\t1001\n");
    }

    #[test]
    fn a_torn_last_line_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), "g1\tb1\t10\ng2\tb2\t1").unwrap();
        let mut cursors = Cursors::open(dir.path(), 20).unwrap();
        assert_eq!(pairs(&cursors), [("g1".into(), "b1".into())]);
        cursors.set("g3", "b3", 21).unwrap();
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text, "g1\tb1\t10\ng3\tb3\t21\n");
    }

    #[test]
    fn cursors_are_dropped_after_forty_four_days() {
        let dir = tempfile::tempdir().unwrap();
        let day = 24 * 60 * 60;
        let mut cursors = Cursors::open(dir.path(), 0).unwrap();
        cursors.set("g1", "b1", 0).unwrap();
        cursors.set("g2", "b2", 10 * day).unwrap();
        drop(cursors);
        assert!(!Cursors::open(dir.path(), 43 * day).unwrap().is_empty());
        assert!(Cursors::open(dir.path(), 44 * day).unwrap().is_empty());
        assert!(Cursors::open(dir.path(), 44 * day).unwrap().is_empty());
        assert_eq!(fs::read_to_string(dir.path().join(FILE)).unwrap(), "");
    }

    #[test]
    fn fields_cannot_break_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let mut cursors = Cursors::open(dir.path(), 0).unwrap();
        assert!(cursors.set("a\tb", "x", 0).is_err());
        assert!(cursors.set("a", "x\ny", 0).is_err());
        assert!(cursors.set("", "x", 0).is_err());
        assert!(cursors.is_empty());
    }
}
