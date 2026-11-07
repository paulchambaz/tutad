//! Sends what was done to the Maildir by hand, such as reading, moving or deleting, to Tuta.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};
use log::info;
use tokio::time::sleep;
use tutasdk::entities::generated::tutanota::MoveMailData;
use tutasdk::services::generated::tutanota::MoveMailService;
use tutasdk::services::ExtraServiceParams;
use tutasdk::{GeneratedId, IdTupleGenerated};

use crate::maildir::{self, Maildir};
use crate::message::MailId;
use crate::state::{Entry, State};
use crate::sync::{Layout, Mirror, PAUSE};

pub(crate) const MOST_VANISHED: usize = 100;
const CHUNK: usize = 50;

#[derive(Debug, PartialEq, Eq)]
enum Change {
    Read {
        id: MailId,
        seen: bool,
        file: String,
    },
    Moved {
        id: MailId,
        to: String,
        file: String,
    },
    Vanished {
        id: MailId,
    },
}

struct Disk {
    files: HashMap<String, (String, String)>,
    scanned: HashSet<String>,
}

fn scan(maildir: &Maildir, layout: &Layout) -> Result<Disk> {
    let mut files = HashMap::new();
    let mut scanned = HashSet::new();
    for folder in layout.names() {
        let Some(names) = maildir.files(folder)? else {
            continue;
        };
        scanned.insert(folder.to_owned());
        for name in names {
            files.insert(maildir::key(&name).to_owned(), (folder.to_owned(), name));
        }
    }
    Ok(Disk { files, scanned })
}

fn diff(state: &State, disk: &Disk) -> Vec<Change> {
    let mut changes = Vec::new();
    for (id, entry) in state.all() {
        match disk.files.get(maildir::key(&entry.file)) {
            Some((folder, file)) => {
                let seen = maildir::is_seen(file);
                if seen != maildir::is_seen(&entry.file) {
                    changes.push(Change::Read {
                        id: id.clone(),
                        seen,
                        file: file.clone(),
                    });
                }
                if *folder != entry.folder {
                    changes.push(Change::Moved {
                        id: id.clone(),
                        to: folder.clone(),
                        file: file.clone(),
                    });
                }
            }
            None if disk.scanned.contains(&entry.folder) => {
                changes.push(Change::Vanished { id: id.clone() });
            }
            None => {}
        }
    }
    changes
}

fn settle(
    changes: Vec<Change>,
    before: &HashSet<MailId>,
) -> Result<(Vec<Change>, HashSet<MailId>)> {
    let vanished: HashSet<MailId> = changes
        .iter()
        .filter_map(|change| match change {
            Change::Vanished { id } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let confirmed = vanished.intersection(before).count();
    if confirmed > MOST_VANISHED {
        bail!("{confirmed} mails vanished at once; not deleting them in Tuta");
    }
    let due = changes
        .into_iter()
        .filter(|change| match change {
            Change::Vanished { id } => before.contains(id),
            _ => true,
        })
        .collect();
    Ok((due, vanished))
}

fn tuple(entry: &Entry, id: &MailId) -> IdTupleGenerated {
    IdTupleGenerated {
        list_id: GeneratedId(entry.list.clone()),
        element_id: GeneratedId(id.0.clone()),
    }
}

impl Mirror {
    /// Sends the reads, moves and deletes made in the Maildir to Tuta.
    ///
    /// A deleted file goes to Tuta's Trash and only once it has been missing
    /// in two scans in a row. More than a hundred at once is refused.
    ///
    /// # Errors
    ///
    /// Fails on a disk error, on too many deletes at once, or on the first
    /// Tuta refusal. What was already sent stays recorded and nothing is retried.
    pub async fn write_back(&mut self, layout: &Layout) -> Result<()> {
        let disk = scan(&self.maildir, layout)?;
        let (due, vanished) = settle(diff(&self.state, &disk), &self.vanished)?;
        self.vanished = vanished;
        if due.is_empty() {
            return Ok(());
        }
        let mut reads: [Vec<(MailId, String)>; 2] = [Vec::new(), Vec::new()];
        let mut moves: HashMap<String, Vec<(MailId, String)>> = HashMap::new();
        let mut trash = Vec::new();
        for change in due {
            match change {
                Change::Read { id, seen, file } => reads[usize::from(seen)].push((id, file)),
                Change::Moved { id, to, file } => moves.entry(to).or_default().push((id, file)),
                Change::Vanished { id } => trash.push(id),
            }
        }
        let total = reads.iter().map(Vec::len).sum::<usize>()
            + moves.values().map(Vec::len).sum::<usize>()
            + trash.len();
        for (seen, group) in reads.iter().enumerate() {
            for chunk in group.chunks(CHUNK) {
                self.mark_read(chunk, seen == 1).await?;
            }
        }
        for (to, group) in &moves {
            let target = layout
                .id(to)
                .with_context(|| format!("no Tuta folder for {to}"))?;
            for chunk in group.chunks(CHUNK) {
                self.move_mails(chunk, to, target).await?;
            }
        }
        for chunk in trash.chunks(CHUNK) {
            self.trash_mails(chunk).await?;
        }
        info!("sent {total} local changes to Tuta");
        Ok(())
    }

    fn entries(&self, chunk: &[(MailId, String)]) -> Vec<(MailId, String, Entry)> {
        chunk
            .iter()
            .filter_map(|(id, file)| Some((id.clone(), file.clone(), self.state.get(id)?.clone())))
            .collect()
    }

    async fn mark_read(&mut self, chunk: &[(MailId, String)], seen: bool) -> Result<()> {
        let known = self.entries(chunk);
        let ids = known
            .iter()
            .map(|(id, _, entry)| tuple(entry, id))
            .collect();
        self.sdk
            .mail_facade()
            .set_unread_status_for_mails(ids, !seen)
            .await
            .context("cannot send a read flag")?;
        for (id, file, entry) in known {
            self.state.set(&id, Entry { file, ..entry })?;
        }
        sleep(PAUSE).await;
        Ok(())
    }

    async fn move_mails(
        &mut self,
        chunk: &[(MailId, String)],
        to: &str,
        target: &IdTupleGenerated,
    ) -> Result<()> {
        let known = self.entries(chunk);
        let data = MoveMailData {
            _format: 0,
            moveReason: None,
            targetFolder: target.clone(),
            mails: known
                .iter()
                .map(|(id, _, entry)| tuple(entry, id))
                .collect(),
            excludeMailSet: None,
        };
        self.sdk
            .get_service_executor()
            .post::<MoveMailService>(data, ExtraServiceParams::default())
            .await
            .context("cannot move a mail")?;
        for (id, file, entry) in known {
            let moved = Entry {
                folder: to.to_owned(),
                file,
                ..entry
            };
            self.state.set(&id, moved)?;
        }
        sleep(PAUSE).await;
        Ok(())
    }

    async fn trash_mails(&mut self, chunk: &[MailId]) -> Result<()> {
        let ids: Vec<IdTupleGenerated> = chunk
            .iter()
            .filter_map(|id| Some(tuple(self.state.get(id)?, id)))
            .collect();
        self.sdk
            .mail_facade()
            .trash_mails(ids)
            .await
            .context("cannot trash a mail")?;
        for id in chunk {
            self.state.remove(id)?;
        }
        sleep(PAUSE).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::message::{Address, Message};

    fn message(id: &str) -> Message {
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
            html: "x".to_owned(),
            attachments: Vec::new(),
            unread: true,
        }
    }

    fn setup(dir: &Path, folders: &[(&str, &str)]) -> (Maildir, State) {
        let maildir = Maildir::open(&dir.join("mail"));
        let mut state = State::open(&dir.join("state")).unwrap();
        for (id, folder) in folders {
            let mail = message(id);
            let file = maildir.deliver(folder, &mail, "x\n").unwrap();
            let entry = Entry {
                folder: (*folder).to_owned(),
                file,
                list: "l".to_owned(),
            };
            state.set(&mail.id, entry).unwrap();
        }
        (maildir, state)
    }

    fn disk(files: &[(&str, &str)], scanned: &[&str]) -> Disk {
        Disk {
            files: files
                .iter()
                .map(|(folder, file)| {
                    (
                        maildir::key(file).to_owned(),
                        ((*folder).to_owned(), (*file).to_owned()),
                    )
                })
                .collect(),
            scanned: scanned.iter().map(|folder| (*folder).to_owned()).collect(),
        }
    }

    fn id(text: &str) -> MailId {
        MailId(text.to_owned())
    }

    #[test]
    fn nothing_changed_means_nothing_to_send() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = setup(dir.path(), &[("A1", "INBOX"), ("A2", "Sent")]);
        let on_disk = disk(
            &[
                ("INBOX", "1767225600.A1.tutad"),
                ("Sent", "1767225600.A2.tutad"),
            ],
            &["INBOX", "Sent"],
        );
        assert!(diff(&state, &on_disk).is_empty());
    }

    #[test]
    fn a_flag_a_move_and_a_missing_file_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = setup(
            dir.path(),
            &[("A1", "INBOX"), ("A2", "INBOX"), ("A3", "INBOX")],
        );
        let on_disk = disk(
            &[
                ("INBOX", "1767225600.A1.tutad:2,S"),
                ("Archive", "1767225600.A2.tutad"),
            ],
            &["INBOX", "Archive"],
        );
        assert_eq!(
            diff(&state, &on_disk),
            [
                Change::Read {
                    id: id("A1"),
                    seen: true,
                    file: "1767225600.A1.tutad:2,S".to_owned()
                },
                Change::Moved {
                    id: id("A2"),
                    to: "Archive".to_owned(),
                    file: "1767225600.A2.tutad".to_owned()
                },
                Change::Vanished { id: id("A3") },
            ]
        );
    }

    #[test]
    fn a_file_renamed_by_dovecot_is_still_the_same_mail() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = setup(dir.path(), &[("A1", "INBOX")]);
        let on_disk = disk(&[("INBOX", "1767225600.A1.tutad,S=1:2,")], &["INBOX"]);
        assert!(diff(&state, &on_disk).is_empty());
    }

    #[test]
    fn a_folder_that_was_not_read_never_loses_its_mails() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = setup(dir.path(), &[("A1", "Work"), ("A2", "INBOX")]);
        let on_disk = disk(&[], &["INBOX"]);
        assert_eq!(diff(&state, &on_disk), [Change::Vanished { id: id("A2") }]);
    }

    #[test]
    fn a_delete_waits_for_a_second_scan_and_a_flood_is_refused() {
        let gone = |name: &str| Change::Vanished { id: id(name) };
        let (due, vanished) = settle(vec![gone("A1")], &HashSet::new()).unwrap();
        assert!(due.is_empty());
        assert_eq!(vanished, HashSet::from([id("A1")]));
        let (due, _) = settle(vec![gone("A1"), gone("A2")], &vanished).unwrap();
        assert_eq!(due, [gone("A1")]);
        let flood: HashSet<MailId> = (0..=MOST_VANISHED).map(|n| id(&n.to_string())).collect();
        let changes = flood.iter().map(|name| gone(&name.0)).collect();
        assert!(settle(changes, &flood).is_err());
    }

    #[test]
    fn the_maildir_is_scanned_by_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (maildir, _) = setup(dir.path(), &[("A1", "INBOX"), ("A2", "Work")]);
        assert_eq!(
            maildir.files("Work").unwrap(),
            Some(vec!["1767225600.A2.tutad".to_owned()])
        );
        assert_eq!(maildir.files("Gone").unwrap(), None);
        fs::remove_dir_all(dir.path().join("mail/.Work")).unwrap();
        assert_eq!(maildir.files("Work").unwrap(), None);
    }
}
