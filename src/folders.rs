//! The folders of the Tuta mailbox, read once and laid out as a tree.

use std::collections::HashSet;

use anyhow::{Context, Result};
use tutasdk::entities::generated::tutanota::MailSet;
use tutasdk::folder_system::MailSetKind;
use tutasdk::{GeneratedId, ListLoadDirection, LoggedInSdk};

const MOST_FOLDERS: usize = 100;

/// A folder and how deep it sits in the tree.
#[derive(Debug, PartialEq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub kind: MailSetKind,
    pub depth: usize,
}

/// Loads the folders of the user's mailbox with two requests.
///
/// # Errors
///
/// Fails when Tuta refuses or the folders cannot be decrypted.
pub async fn load(sdk: &LoggedInSdk) -> Result<Vec<Folder>> {
    let mail = sdk.mail_facade();
    let mailbox = mail
        .load_user_mailbox()
        .await
        .context("cannot load the mailbox")?;
    let sets: Vec<MailSet> = mail
        .get_crypto_entity_client()
        .load_range(
            &mailbox.mailSets.mailSets,
            &GeneratedId::min_id(),
            MOST_FOLDERS,
            ListLoadDirection::ASC,
        )
        .await
        .context("cannot load the folders")?;
    Ok(tree(&sets))
}

fn kind(set: &MailSet) -> MailSetKind {
    u64::try_from(set.folderType)
        .ok()
        .and_then(|number| MailSetKind::try_from(number).ok())
        .unwrap_or(MailSetKind::Unknown)
}

fn id(set: &MailSet) -> Option<&GeneratedId> {
    set._id.as_ref().map(|id| &id.element_id)
}

fn parent<'a>(set: &'a MailSet, known: &HashSet<&GeneratedId>) -> Option<&'a GeneratedId> {
    set.parentFolder
        .as_ref()
        .map(|id| &id.element_id)
        .filter(|parent| known.contains(parent))
}

fn walk(
    sets: &[MailSet],
    known: &HashSet<&GeneratedId>,
    under: Option<&GeneratedId>,
    depth: usize,
    out: &mut Vec<Folder>,
) {
    let mut children: Vec<&MailSet> = sets
        .iter()
        .filter(|set| id(set).is_some() && parent(set, known) == under)
        .collect();
    children.sort_by_key(|set| (kind(set) == MailSetKind::Custom, set.name.to_lowercase()));
    for set in children {
        let Some(own) = id(set) else { continue };
        out.push(Folder {
            id: own.0.clone(),
            name: set.name.clone(),
            kind: kind(set),
            depth,
        });
        walk(sets, known, Some(own), depth + 1, out);
    }
}

fn tree(sets: &[MailSet]) -> Vec<Folder> {
    let known: HashSet<&GeneratedId> = sets.iter().filter_map(id).collect();
    let mut out = Vec::new();
    walk(sets, &known, None, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use tutasdk::IdTupleGenerated;

    use super::*;

    fn set(id: &str, name: &str, folder_type: i64, parent: Option<&str>) -> MailSet {
        let tuple = |element: &str| IdTupleGenerated {
            list_id: GeneratedId("list".to_owned()),
            element_id: GeneratedId(element.to_owned()),
        };
        MailSet {
            _id: Some(tuple(id)),
            _permissions: GeneratedId(String::new()),
            _format: 0,
            _ownerEncSessionKey: None,
            name: name.to_owned(),
            folderType: folder_type,
            _ownerGroup: None,
            _ownerKeyVersion: None,
            color: None,
            _kdfNonce: None,
            parentFolder: parent.map(tuple),
            entries: GeneratedId(String::new()),
            _errors: Default::default(),
        }
    }

    fn outline(folders: &[Folder]) -> Vec<(String, usize)> {
        folders
            .iter()
            .map(|folder| (folder.id.clone(), folder.depth))
            .collect()
    }

    #[test]
    fn system_folders_come_first_and_children_follow_their_parent() {
        let sets = [
            set("work", "Work", 0, None),
            set("inbox", "", 1, None),
            set("clients", "Clients", 0, Some("work")),
            set("sent", "", 2, None),
            set("alpha", "Alpha", 0, None),
            set("acme", "Acme", 0, Some("clients")),
        ];
        let expected = [
            ("inbox", 0),
            ("sent", 0),
            ("alpha", 0),
            ("work", 0),
            ("clients", 1),
            ("acme", 2),
        ];
        assert_eq!(
            outline(&tree(&sets)),
            expected.map(|(id, depth)| (id.to_owned(), depth))
        );
    }

    #[test]
    fn kinds_are_read_from_the_folder_type() {
        let folders = tree(&[
            set("a", "", 3, None),
            set("b", "", 9999, None),
            set("c", "", 77, None),
        ]);
        let kinds: Vec<MailSetKind> = folders.iter().map(|folder| folder.kind).collect();
        assert!(kinds.contains(&MailSetKind::Trash));
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == MailSetKind::Unknown)
                .count(),
            2
        );
    }

    #[test]
    fn a_folder_whose_parent_is_missing_becomes_a_root() {
        let folders = tree(&[set("lost", "Lost", 0, Some("gone"))]);
        assert_eq!(outline(&folders), vec![("lost".to_owned(), 0)]);
    }

    #[test]
    fn a_folder_that_is_its_own_parent_does_not_loop() {
        let folders = tree(&[set("a", "A", 0, Some("a")), set("b", "B", 0, None)]);
        assert_eq!(outline(&folders), vec![("b".to_owned(), 0)]);
    }
}
