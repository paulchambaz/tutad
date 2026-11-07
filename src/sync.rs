//! Keeps the Maildir equal to the Tuta mailbox: a first fetch, then each change as it arrives.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use log::{info, warn};
use tokio::time::sleep;
use tutasdk::entities::generated::tutanota::Mail;
use tutasdk::folder_system::MailSetKind;
use tutasdk::{GeneratedId, IdTupleGenerated, LoggedInSdk};

use crate::events::{Batch, Operation, Update};
use crate::fetch::Fetcher;
use crate::folders::{self, Folder};
use crate::maildir::Maildir;
use crate::message::MailId;
use crate::render::render;
use crate::state::{Entry, State};
use crate::writeback::MOST_VANISHED;

const MAIL: i64 = 97;
const MAIL_SET: i64 = 429;
const MAIL_SET_ENTRY: i64 = 1450;
pub(crate) const PAUSE: Duration = Duration::from_millis(150);
const CHUNK: usize = 20;
const MOST_FAILURES: u32 = 3;
const ENTRY_ID_BYTES: usize = 13;
const TIMESTAMP_BYTES: usize = 4;
const ALPHABET: &[u8; 64] = b"-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz";

/// The Tuta folders that are mirrored, with the Maildir folder each one becomes.
pub struct Layout {
    folders: Vec<Place>,
}

struct Place {
    entries: String,
    name: String,
    id: IdTupleGenerated,
}

impl Layout {
    /// Picks the folders to mirror from the folder tree.
    ///
    /// The inbox, sent, drafts, archive and the user's own folders are kept.
    /// Trash, spam, the all-mail view and anything unknown are left out.
    pub fn new(tree: &[Folder]) -> Self {
        let mut path: Vec<Option<String>> = Vec::new();
        let mut folders = Vec::new();
        for folder in tree {
            path.truncate(folder.depth);
            path.push(match folder.kind {
                MailSetKind::Inbox => Some("INBOX".to_owned()),
                MailSetKind::Sent => Some("Sent".to_owned()),
                MailSetKind::Draft => Some("Drafts".to_owned()),
                MailSetKind::Archive => Some("Archive".to_owned()),
                MailSetKind::Custom => Some(clean(&folder.name)),
                _ => None,
            });
            if let Some(parts) = path
                .iter()
                .map(Option::as_deref)
                .collect::<Option<Vec<_>>>()
            {
                folders.push(Place {
                    entries: folder.entries.clone(),
                    name: parts.join("."),
                    id: IdTupleGenerated {
                        list_id: GeneratedId(folder.list.clone()),
                        element_id: GeneratedId(folder.id.clone()),
                    },
                });
            }
        }
        folders.sort_by_key(|place| match place.name.as_str() {
            "INBOX" => 0,
            "Sent" => 1,
            "Drafts" => 2,
            "Archive" => 3,
            _ => 4,
        });
        Self { folders }
    }

    /// Keeps only the folder with the given Maildir name.
    ///
    /// # Errors
    ///
    /// Fails when there is no such folder, naming the ones there are.
    pub fn only(mut self, name: &str) -> Result<Self> {
        if !self.folders.iter().any(|place| place.name == name) {
            let names: Vec<&str> = self
                .folders
                .iter()
                .map(|place| place.name.as_str())
                .collect();
            bail!("no such folder: {name}; there are {}", names.join(", "));
        }
        self.folders.retain(|place| place.name == name);
        Ok(self)
    }

    fn name(&self, entries: &str) -> Option<&str> {
        self.folders
            .iter()
            .find(|place| place.entries == entries)
            .map(|place| place.name.as_str())
    }

    fn renames(&self, new: &Layout) -> Vec<(String, String)> {
        self.folders
            .iter()
            .filter_map(|old| {
                let current = new.name(&old.entries)?;
                (current != old.name).then(|| (old.name.clone(), current.to_owned()))
            })
            .collect()
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.folders.iter().map(|place| place.name.as_str())
    }

    pub(crate) fn id(&self, name: &str) -> Option<&IdTupleGenerated> {
        self.folders
            .iter()
            .find(|place| place.name == name)
            .map(|place| &place.id)
    }
}

fn clean(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if matches!(c, '.' | '/') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "_".to_owned()
    } else if name.eq_ignore_ascii_case("INBOX") {
        format!("{name}_")
    } else {
        name.to_owned()
    }
}

fn mail_id(entry_id: &str) -> Option<MailId> {
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(entry_id).ok()?;
    if bytes.len() != ENTRY_ID_BYTES {
        return None;
    }
    let id = bytes[TIMESTAMP_BYTES..]
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|chunk| {
            let n = chunk
                .iter()
                .fold(0_u32, |n, byte| n << 8 | u32::from(*byte));
            [18, 12, 6, 0].map(|shift| char::from(ALPHABET[(n >> shift & 63) as usize]))
        })
        .collect();
    Some(MailId(id))
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Deliver {
        entries: String,
        entry: String,
        folder: String,
    },
    Move {
        id: MailId,
        to: String,
    },
    Remove {
        id: MailId,
    },
    Refresh {
        list: String,
        id: MailId,
    },
}

fn is(update: &Update, type_id: i64, operation: Operation) -> bool {
    update.application == "tutanota" && update.type_id == type_id && update.operation == operation
}

fn plan(batch: &Batch, layout: &Layout, state: &State) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut at: HashMap<MailId, Option<String>> = HashMap::new();
    let place = |at: &HashMap<MailId, Option<String>>, id: &MailId| match at.get(id) {
        Some(folder) => folder.clone(),
        None => state.get(id).map(|entry| entry.folder.clone()),
    };
    for update in &batch.updates {
        if !is(update, MAIL_SET_ENTRY, Operation::Create) {
            continue;
        }
        let (Some(folder), Some(id)) = (layout.name(&update.list_id), mail_id(&update.id)) else {
            continue;
        };
        match place(&at, &id) {
            Some(current) if current == folder => {}
            Some(_) => {
                actions.push(Action::Move {
                    id: id.clone(),
                    to: folder.to_owned(),
                });
                at.insert(id, Some(folder.to_owned()));
            }
            None => {
                actions.push(Action::Deliver {
                    entries: update.list_id.clone(),
                    entry: update.id.clone(),
                    folder: folder.to_owned(),
                });
                at.insert(id, Some(folder.to_owned()));
            }
        }
    }
    for update in &batch.updates {
        if !is(update, MAIL_SET_ENTRY, Operation::Delete) {
            continue;
        }
        let (Some(folder), Some(id)) = (layout.name(&update.list_id), mail_id(&update.id)) else {
            continue;
        };
        if place(&at, &id).as_deref() == Some(folder) {
            actions.push(Action::Remove { id: id.clone() });
            at.insert(id, None);
        }
    }
    for update in &batch.updates {
        let id = MailId(update.id.clone());
        if is(update, MAIL, Operation::Delete) && place(&at, &id).is_some() {
            actions.push(Action::Remove { id: id.clone() });
            at.insert(id, None);
        } else if is(update, MAIL, Operation::Update)
            && !at.contains_key(&id)
            && state.get(&id).is_some()
        {
            actions.push(Action::Refresh {
                list: update.list_id.clone(),
                id: id.clone(),
            });
            let current = place(&at, &id);
            at.insert(id, current);
        }
    }
    actions
}

fn stale(state: &State, layout: &Layout, listing: &HashMap<MailId, String>) -> Result<Vec<Action>> {
    let actions: Vec<Action> = state
        .all()
        .filter(|(_, entry)| layout.names().any(|name| name == entry.folder))
        .filter_map(|(id, entry)| match listing.get(id) {
            None => Some(Action::Remove { id: id.clone() }),
            Some(folder) if *folder != entry.folder => Some(Action::Move {
                id: id.clone(),
                to: folder.clone(),
            }),
            Some(_) => None,
        })
        .collect();
    let gone = actions
        .iter()
        .filter(|action| matches!(action, Action::Remove { .. }))
        .count();
    if gone > MOST_VANISHED {
        bail!("Tuta no longer lists {gone} mails; not removing them");
    }
    Ok(actions)
}

fn relocate(maildir: &Maildir, state: &mut State, id: &MailId, to: &str) -> Result<()> {
    let Some(entry) = state.get(id) else {
        return Ok(());
    };
    match maildir.move_to(&entry.folder, &entry.file, to)? {
        Some(file) => state.set(
            id,
            Entry {
                folder: to.to_owned(),
                file,
                list: entry.list.clone(),
            },
        ),
        None => {
            warn!("the file of mail {} is gone; forgetting it", id.0);
            state.remove(id)
        }
    }
}

fn discard(maildir: &Maildir, state: &mut State, id: &MailId) -> Result<()> {
    if let Some(entry) = state.get(id) {
        maildir.remove(&entry.folder, &entry.file)?;
        state.remove(id)?;
    }
    Ok(())
}

fn mark(maildir: &Maildir, state: &mut State, id: &MailId, unread: bool) -> Result<()> {
    let Some(entry) = state.get(id) else {
        return Ok(());
    };
    if let Some(file) = maildir.set_seen(&entry.folder, &entry.file, !unread)? {
        if file != entry.file {
            let renamed = Entry {
                file,
                ..entry.clone()
            };
            state.set(id, renamed)?;
        }
    }
    Ok(())
}

/// Writes Tuta's mail into a Maildir and keeps it current.
pub struct Mirror {
    fetcher: Fetcher,
    pub(crate) sdk: Arc<LoggedInSdk>,
    pub(crate) maildir: Maildir,
    pub(crate) state: State,
    failures: u32,
    pub(crate) vanished: HashSet<MailId>,
}

impl Mirror {
    /// A mirror of the session's mailbox into the Maildir, remembering what it wrote in the state.
    pub fn new(sdk: Arc<LoggedInSdk>, maildir: Maildir, state: State) -> Self {
        Self {
            fetcher: Fetcher::new(Arc::clone(&sdk)),
            sdk,
            maildir,
            state,
            failures: 0,
            vanished: HashSet::new(),
        }
    }

    /// Fetches every mirrored folder's mails that are not in the Maildir yet, newest first.
    ///
    /// Returns the Maildir folder of every mail Tuta listed. A mail that cannot
    /// be read is skipped and tried again at the next start.
    ///
    /// # Errors
    ///
    /// Fails on the first Tuta refusal on a listing, on a disk error, or after
    /// three mails in a row could not be read.
    pub async fn initial(&mut self, layout: &Layout) -> Result<HashMap<MailId, String>> {
        let mut listing = HashMap::new();
        for Place {
            entries,
            name: folder,
            ..
        } in &layout.folders
        {
            let ids = self.fetcher.mail_ids(&GeneratedId(entries.clone())).await?;
            let total = ids.len();
            listing.extend(
                ids.iter()
                    .map(|id| (MailId(id.element_id.0.clone()), folder.clone())),
            );
            let fresh: Vec<IdTupleGenerated> = ids
                .into_iter()
                .filter(|id| self.state.get(&MailId(id.element_id.0.clone())).is_none())
                .collect();
            info!("{folder}: {} new of {total}", fresh.len());
            for chunk in fresh.chunks(CHUNK) {
                for mail in self.fetcher.mails(chunk).await? {
                    self.deliver(&mail, folder).await?;
                }
            }
        }
        Ok(listing)
    }

    /// Drops the mails Tuta no longer lists and moves the ones it filed elsewhere.
    ///
    /// For a start after the event stream's replay window has passed. Read
    /// flags are not compared.
    ///
    /// # Errors
    ///
    /// Fails when over 100 mails would be dropped, or on a disk error.
    pub fn reconcile(&mut self, layout: &Layout, listing: &HashMap<MailId, String>) -> Result<()> {
        let actions = stale(&self.state, layout, listing)?;
        if !actions.is_empty() {
            info!("{} changes made in Tuta while away", actions.len());
        }
        for action in actions {
            match action {
                Action::Move { id, to } => relocate(&self.maildir, &mut self.state, &id, &to)?,
                Action::Remove { id } => discard(&self.maildir, &mut self.state, &id)?,
                Action::Deliver { .. } | Action::Refresh { .. } => {}
            }
        }
        Ok(())
    }

    /// Applies one batch of changes from the event stream.
    ///
    /// # Errors
    ///
    /// Fails on a disk error, a Tuta refusal while moving or marking, or after
    /// three mails in a row could not be read. The batch is then not done and
    /// the caller must not record it.
    pub async fn apply(&mut self, layout: &mut Layout, batch: &Batch) -> Result<()> {
        if batch.updates.iter().any(|u| is(u, MAIL_SET, u.operation)) {
            let current = Layout::new(&folders::load(&self.sdk).await?);
            self.rename_folders(&layout.renames(&current))?;
            *layout = current;
        }
        for action in plan(batch, layout, &self.state) {
            match action {
                Action::Deliver {
                    entries,
                    entry,
                    folder,
                } => {
                    let Some(id) = self.fetcher.mail_of_entry(&entries, &entry).await? else {
                        continue;
                    };
                    for mail in self.fetcher.mails(&[id]).await? {
                        self.deliver(&mail, &folder).await?;
                    }
                }
                Action::Move { id, to } => relocate(&self.maildir, &mut self.state, &id, &to)?,
                Action::Remove { id } => discard(&self.maildir, &mut self.state, &id)?,
                Action::Refresh { list, id } => {
                    let ids = [IdTupleGenerated {
                        list_id: GeneratedId(list),
                        element_id: GeneratedId(id.0.clone()),
                    }];
                    for mail in self.fetcher.mails(&ids).await? {
                        mark(&self.maildir, &mut self.state, &id, mail.unread)?;
                    }
                    sleep(PAUSE).await;
                }
            }
        }
        Ok(())
    }

    fn rename_folders(&mut self, renames: &[(String, String)]) -> Result<()> {
        for (from, to) in renames {
            if self.maildir.rename_folder(from, to)? {
                self.state.rename_folder(from, to)?;
                info!("folder {from} is now {to}");
            } else {
                warn!("cannot rename folder {from} to {to}, which exists; mail stays in {from}");
            }
        }
        Ok(())
    }

    async fn deliver(&mut self, mail: &Mail, folder: &str) -> Result<()> {
        let written = async {
            let message = self.fetcher.message(mail).await?;
            let file = self.maildir.deliver(folder, &message, &render(&message))?;
            let list = mail._id.as_ref().map(|id| id.list_id.0.clone());
            let entry = Entry {
                folder: folder.to_owned(),
                file,
                list: list.context("the mail has no id")?,
            };
            self.state.set(&message.id, entry)
        }
        .await;
        sleep(PAUSE).await;
        match written {
            Ok(()) => self.failures = 0,
            Err(error) => {
                self.failures += 1;
                warn!("skipping a mail: {error:#}");
                if self.failures >= MOST_FAILURES {
                    bail!("{MOST_FAILURES} mails in a row could not be read; stopping");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::message::{Address, Message};

    fn folder(entries: &str, name: &str, kind: MailSetKind, depth: usize) -> Folder {
        Folder {
            list: "sets".to_owned(),
            id: format!("id-{entries}"),
            entries: entries.to_owned(),
            name: name.to_owned(),
            kind,
            depth,
        }
    }

    fn layout() -> Layout {
        Layout::new(&[
            folder("e-sent", "", MailSetKind::Sent, 0),
            folder("e-inbox", "", MailSetKind::Inbox, 0),
            folder("e-trash", "", MailSetKind::Trash, 0),
            folder("e-spam", "", MailSetKind::Spam, 0),
            folder("e-all", "", MailSetKind::All, 0),
            folder("e-work", "Work", MailSetKind::Custom, 0),
            folder("e-clients", "Clients", MailSetKind::Custom, 1),
            folder("e-odd", "a.b/c", MailSetKind::Custom, 1),
            folder("e-inbox2", "inbox", MailSetKind::Custom, 0),
            folder("e-archive", "", MailSetKind::Archive, 0),
            folder("e-draft", "", MailSetKind::Draft, 0),
        ])
    }

    fn entry_id(mail: &str) -> String {
        let mut bytes = vec![0_u8, 0, 0, 1];
        let mut group = 0_u32;
        let mut bits = 0;
        for c in mail.bytes() {
            let value = ALPHABET.iter().position(|a| *a == c).unwrap() as u32;
            group = group << 6 | value;
            bits += 6;
            if bits == 24 {
                bytes.extend([(group >> 16) as u8, (group >> 8) as u8, group as u8]);
                group = 0;
                bits = 0;
            }
        }
        BASE64_URL_SAFE_NO_PAD.encode(bytes)
    }

    fn update(type_id: i64, list: &str, id: &str, operation: Operation) -> Update {
        Update {
            application: "tutanota".to_owned(),
            type_id,
            list_id: list.to_owned(),
            id: id.to_owned(),
            operation,
        }
    }

    fn batch(updates: Vec<Update>) -> Batch {
        Batch {
            group: "g".to_owned(),
            id: "b".to_owned(),
            updates,
        }
    }

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

    fn written(dir: &std::path::Path, ids: &[(&str, &str)]) -> (Maildir, State) {
        let maildir = Maildir::open(&dir.join("mail"));
        let mut state = State::open(&dir.join("state")).unwrap();
        for (id, folder) in ids {
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

    #[test]
    fn folders_are_named_ordered_and_cleaned() {
        let layout = layout();
        let names: Vec<(&str, &str)> = layout
            .folders
            .iter()
            .map(|place| (place.entries.as_str(), place.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("e-inbox", "INBOX"),
                ("e-sent", "Sent"),
                ("e-draft", "Drafts"),
                ("e-archive", "Archive"),
                ("e-work", "Work"),
                ("e-clients", "Work.Clients"),
                ("e-odd", "Work.a_b_c"),
                ("e-inbox2", "inbox_"),
            ]
        );
    }

    #[test]
    fn a_mail_id_is_read_out_of_a_folder_entry_id() {
        assert_eq!(
            mail_id("V7ifKQAAAAAAAAAAAQ"),
            Some(MailId("-----------0".to_owned()))
        );
        assert_eq!(
            mail_id(&entry_id("O1qC7an--3-0")),
            Some(MailId("O1qC7an--3-0".to_owned()))
        );
        assert_eq!(mail_id("AAAA"), None);
        assert_eq!(mail_id("not base64!"), None);
    }

    #[test]
    fn a_new_entry_in_a_mirrored_folder_is_fetched_and_one_elsewhere_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(dir.path(), &[]);
        let id = entry_id("AAAAAAAAAAA0");
        let create = |list| update(MAIL_SET_ENTRY, list, &id, Operation::Create);
        let actions = plan(
            &batch(vec![create("e-inbox"), create("e-trash")]),
            &layout(),
            &state,
        );
        assert_eq!(
            actions,
            [Action::Deliver {
                entries: "e-inbox".to_owned(),
                entry: id,
                folder: "INBOX".to_owned(),
            }]
        );
    }

    #[test]
    fn a_move_is_a_move_whichever_half_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(dir.path(), &[("AAAAAAAAAAA0", "INBOX")]);
        let id = entry_id("AAAAAAAAAAA0");
        let delete = update(MAIL_SET_ENTRY, "e-inbox", &id, Operation::Delete);
        let create = update(MAIL_SET_ENTRY, "e-work", &id, Operation::Create);
        let moved = Action::Move {
            id: MailId("AAAAAAAAAAA0".to_owned()),
            to: "Work".to_owned(),
        };
        let one = plan(
            &batch(vec![delete.clone(), create.clone()]),
            &layout(),
            &state,
        );
        let two = plan(&batch(vec![create, delete]), &layout(), &state);
        assert_eq!(one, [moved]);
        assert_eq!(one, two);
    }

    #[test]
    fn a_move_to_the_trash_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(dir.path(), &[("AAAAAAAAAAA0", "INBOX")]);
        let id = entry_id("AAAAAAAAAAA0");
        let actions = plan(
            &batch(vec![
                update(MAIL_SET_ENTRY, "e-inbox", &id, Operation::Delete),
                update(MAIL_SET_ENTRY, "e-trash", &id, Operation::Create),
            ]),
            &layout(),
            &state,
        );
        assert_eq!(
            actions,
            [Action::Remove {
                id: MailId("AAAAAAAAAAA0".to_owned())
            }]
        );
    }

    #[test]
    fn a_repeated_batch_does_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(dir.path(), &[("AAAAAAAAAAA0", "INBOX")]);
        let id = entry_id("AAAAAAAAAAA0");
        let create = update(MAIL_SET_ENTRY, "e-inbox", &id, Operation::Create);
        assert!(plan(&batch(vec![create]), &layout(), &state).is_empty());
        let stale = update(MAIL_SET_ENTRY, "e-work", &id, Operation::Delete);
        assert!(plan(&batch(vec![stale]), &layout(), &state).is_empty());
    }

    #[test]
    fn mail_updates_refresh_known_mails_once_and_deletes_remove_them() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(dir.path(), &[("AAAAAAAAAAA0", "INBOX")]);
        let change = |id: &str, operation| update(MAIL, "list", id, operation);
        let actions = plan(
            &batch(vec![
                change("AAAAAAAAAAA0", Operation::Update),
                change("AAAAAAAAAAA0", Operation::Update),
                change("AAAAAAAAAAA1", Operation::Update),
            ]),
            &layout(),
            &state,
        );
        assert_eq!(
            actions,
            [Action::Refresh {
                list: "list".to_owned(),
                id: MailId("AAAAAAAAAAA0".to_owned())
            }]
        );
        let actions = plan(
            &batch(vec![
                change("AAAAAAAAAAA0", Operation::Delete),
                change("AAAAAAAAAAA1", Operation::Delete),
            ]),
            &layout(),
            &state,
        );
        assert_eq!(
            actions,
            [Action::Remove {
                id: MailId("AAAAAAAAAAA0".to_owned())
            }]
        );
    }

    #[test]
    fn other_applications_and_types_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(dir.path(), &[]);
        let id = entry_id("AAAAAAAAAAA0");
        let mut other = update(MAIL_SET_ENTRY, "e-inbox", &id, Operation::Create);
        other.application = "sys".to_owned();
        let unrelated = update(5, "e-inbox", &id, Operation::Create);
        assert!(plan(&batch(vec![other, unrelated]), &layout(), &state).is_empty());
    }

    #[test]
    fn moving_removing_and_marking_keep_the_files_and_the_state_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let (maildir, mut state) = written(dir.path(), &[("A1", "INBOX"), ("A2", "INBOX")]);
        let (one, two) = (MailId("A1".to_owned()), MailId("A2".to_owned()));
        mark(&maildir, &mut state, &one, false).unwrap();
        assert_eq!(state.get(&one).unwrap().file, "1767225600.A1.tutad:2,S");
        mark(&maildir, &mut state, &one, true).unwrap();
        assert_eq!(state.get(&one).unwrap().file, "1767225600.A1.tutad:2,");
        relocate(&maildir, &mut state, &one, "Work").unwrap();
        assert_eq!(state.get(&one).unwrap().folder, "Work");
        assert!(dir
            .path()
            .join("mail/.Work/cur/1767225600.A1.tutad:2,")
            .exists());
        discard(&maildir, &mut state, &two).unwrap();
        assert!(state.get(&two).is_none());
        assert!(fs::read_dir(dir.path().join("mail/new"))
            .unwrap()
            .next()
            .is_none());
        discard(&maildir, &mut state, &two).unwrap();
        relocate(&maildir, &mut state, &two, "Work").unwrap();
    }

    #[test]
    fn a_file_deleted_behind_our_back_is_forgotten_on_a_move() {
        let dir = tempfile::tempdir().unwrap();
        let (maildir, mut state) = written(dir.path(), &[("A1", "INBOX")]);
        let id = MailId("A1".to_owned());
        let file = state.get(&id).unwrap().file.clone();
        maildir.remove("INBOX", &file).unwrap();
        relocate(&maildir, &mut state, &id, "Work").unwrap();
        assert!(state.get(&id).is_none());
    }

    fn listing(places: &[(&str, &str)]) -> HashMap<MailId, String> {
        places
            .iter()
            .map(|(id, folder)| (MailId((*id).to_owned()), (*folder).to_owned()))
            .collect()
    }

    #[test]
    fn what_tuta_dropped_or_moved_while_away_is_caught_up() {
        let dir = tempfile::tempdir().unwrap();
        let (_, state) = written(
            dir.path(),
            &[
                ("A1", "INBOX"),
                ("A2", "INBOX"),
                ("A3", "INBOX"),
                ("A4", "Gone"),
            ],
        );
        let listing = listing(&[("A1", "INBOX"), ("A3", "Work")]);
        let actions = stale(&state, &layout(), &listing).unwrap();
        assert_eq!(
            actions,
            [
                Action::Remove {
                    id: MailId("A2".to_owned())
                },
                Action::Move {
                    id: MailId("A3".to_owned()),
                    to: "Work".to_owned()
                },
            ]
        );
    }

    #[test]
    fn too_many_mails_gone_at_once_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<String> = (0..=MOST_VANISHED).map(|n| format!("A{n}")).collect();
        let placed: Vec<(&str, &str)> = ids.iter().map(|id| (id.as_str(), "INBOX")).collect();
        let (_, state) = written(dir.path(), &placed);
        assert!(stale(&state, &layout(), &listing(&[])).is_err());
        let kept: Vec<(&str, &str)> = placed.iter().skip(1).copied().collect();
        assert!(stale(&state, &layout(), &listing(&kept)).is_ok());
    }

    #[test]
    fn a_renamed_folder_and_its_children_are_paired_by_their_tuta_id() {
        let tree = |parent: &str| {
            Layout::new(&[
                folder("e-inbox", "", MailSetKind::Inbox, 0),
                folder("e-work", parent, MailSetKind::Custom, 0),
                folder("e-clients", "Clients", MailSetKind::Custom, 1),
            ])
        };
        let renames = tree("Work").renames(&tree("Jobs"));
        assert_eq!(
            renames,
            [
                ("Work".to_owned(), "Jobs".to_owned()),
                ("Work.Clients".to_owned(), "Jobs.Clients".to_owned()),
            ]
        );
        assert!(tree("Work").renames(&tree("Work")).is_empty());
        assert!(tree("Work")
            .renames(&layout())
            .iter()
            .all(|(from, _)| from != "INBOX"));
    }
}
