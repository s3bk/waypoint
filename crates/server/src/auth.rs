//! Access keys. A graph has three keys, each derived from the one above by a
//! one-way hash, so holding a key lets you work out the keys below it but never
//! the ones above:
//!
//! ```text
//! owner --hash--> editor --hash--> viewer
//! ```
//!
//! - **viewer** reads the graph;
//! - **editor** reads and changes it, and can hand out the viewer key;
//! - **owner** can do the same, and can hand out the editor key.
//!
//! A key is a bearer credential: whoever holds it has its role, and sharing a
//! key shares the role. The graph is identified by its viewer key, which every
//! key can derive, so a request names the graph by that key and proves its role
//! with its own. Nothing is stored: the server derives downward from the key it
//! is given and compares. The one consequence is that a key cannot be revoked;
//! to cut someone off, move the content to a new graph with new keys.
//!
//! On disk a graph is filed under the hash of its viewer key, so a copy of the
//! database does not hand out access.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }
}

/// The three keys of a graph.
pub struct Keys {
    pub owner: String,
    pub editor: String,
    pub viewer: String,
}

fn digest(domain: &str, key: &str) -> String {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update([0]);
    h.update(key.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn editor_of(owner: &str) -> String {
    format!("wp_editor_{}", digest("waypoint/editor/v1", owner))
}

pub fn viewer_of(editor: &str) -> String {
    format!("wp_viewer_{}", digest("waypoint/viewer/v1", editor))
}

impl Keys {
    pub fn from_owner(owner: String) -> Keys {
        let editor = editor_of(&owner);
        let viewer = viewer_of(&editor);
        Keys { owner, editor, viewer }
    }

    /// A new graph's keys, from 256 random bits.
    pub fn generate() -> Keys {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("operating system randomness");
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Keys::from_owner(format!("wp_owner_{hex}"))
    }

    pub fn to_json(&self) -> Value {
        json!({ "owner": self.owner, "editor": self.editor, "viewer": self.viewer })
    }
}

/// The role `key` has on the graph identified by `viewer_key`, if any.
pub fn role_for(key: &str, viewer_key: &str) -> Option<Role> {
    if key.len() < 16 {
        return None;
    }
    if key == viewer_key {
        Some(Role::Viewer)
    } else if viewer_of(key) == viewer_key {
        Some(Role::Editor)
    } else if viewer_of(&editor_of(key)) == viewer_key {
        Some(Role::Owner)
    } else {
        None
    }
}

/// The role a key claims by its prefix, and the viewer key of the graph it would
/// open if the claim is true. A key with the wrong prefix gets a graph it will
/// not open: [`role_for`] is what decides.
pub fn claim(key: &str) -> Option<(Role, String)> {
    if key.starts_with("wp_owner_") {
        Some((Role::Owner, viewer_of(&editor_of(key))))
    } else if key.starts_with("wp_editor_") {
        Some((Role::Editor, viewer_of(key)))
    } else if key.starts_with("wp_viewer_") {
        Some((Role::Viewer, key.to_owned()))
    } else {
        None
    }
}

/// What `key` can hand out, given its role: the keys at its own level and below.
/// An owner key shares as editor or viewer, an editor key as viewer.
pub fn shareable(key: &str, role: Role) -> Value {
    match role {
        Role::Owner => {
            let k = Keys::from_owner(key.to_owned());
            json!({ "owner": k.owner, "editor": k.editor, "viewer": k.viewer })
        }
        Role::Editor => json!({ "editor": key, "viewer": viewer_of(key) }),
        Role::Viewer => json!({ "viewer": key }),
    }
}

/// The name a graph is stored and held in memory under.
pub fn storage_id(viewer_key: &str) -> String {
    digest("waypoint/graph/v1", viewer_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_key_works_for_its_role_and_derives_those_below() {
        let k = Keys::generate();
        assert!(k.owner.starts_with("wp_owner_") && k.editor.starts_with("wp_editor_") && k.viewer.starts_with("wp_viewer_"));
        assert_eq!(role_for(&k.owner, &k.viewer), Some(Role::Owner));
        assert_eq!(role_for(&k.editor, &k.viewer), Some(Role::Editor));
        assert_eq!(role_for(&k.viewer, &k.viewer), Some(Role::Viewer));
    }

    #[test]
    fn a_key_names_its_graph_by_its_prefix() {
        let k = Keys::generate();
        for (key, role) in [(&k.owner, Role::Owner), (&k.editor, Role::Editor), (&k.viewer, Role::Viewer)] {
            assert_eq!(claim(key), Some((role, k.viewer.clone())));
        }
        assert_eq!(claim("whatever"), None);
    }

    #[test]
    fn keys_do_not_work_on_other_graphs_and_nothing_else_works() {
        let (a, b) = (Keys::generate(), Keys::generate());
        for key in [&a.owner, &a.editor, &a.viewer] {
            assert_eq!(role_for(key, &b.viewer), None);
        }
        assert_eq!(role_for("", &a.viewer), None);
        assert_eq!(role_for("short", "short"), None);
        assert_eq!(role_for("wp_viewer_not-a-real-key-at-all", &a.viewer), None);
    }

    #[test]
    fn a_key_only_yields_keys_at_or_below_its_role() {
        let k = Keys::generate();
        assert_eq!(shareable(&k.owner, Role::Owner)["editor"], k.editor);
        assert_eq!(shareable(&k.editor, Role::Editor)["viewer"], k.viewer);
        assert!(shareable(&k.editor, Role::Editor).get("owner").is_none());
        assert_eq!(shareable(&k.viewer, Role::Viewer), json!({ "viewer": k.viewer }));
    }

    #[test]
    fn the_stored_name_does_not_reveal_the_viewer_key() {
        let k = Keys::generate();
        let id = storage_id(&k.viewer);
        assert_eq!(id.len(), 64);
        assert!(crate::rooms::valid_graph_id(&id));
        assert_ne!(id, storage_id(&Keys::generate().viewer));
        assert_eq!(role_for(&id, &k.viewer), None, "the stored name is not a credential");
    }
}
