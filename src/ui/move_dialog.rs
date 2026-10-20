//! "Move to…" folder picker and the rules for valid move destinations.
//!
//! The same rules apply to the dialog and to drag-and-drop: items cannot
//! be moved to the folder they are already in, and a folder cannot be moved
//! into itself or anywhere beneath it. The server enforces the second rule
//! too; checking first avoids a pointless request and gives a clear reason.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use filester_app::api::FolderTree;
use filester_app::api::models::FolderId;

use super::Location;
use super::file_grid::GridItem;

/// Why a destination cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// The items are already in this folder.
    AlreadyHere,
    /// The destination is one of the folders being moved, or inside one.
    IntoItself,
}

impl Blocked {
    /// Short explanation for the user.
    pub fn reason(self) -> &'static str {
        match self {
            Self::AlreadyHere => "Already here",
            Self::IntoItself => "Can't move a folder into itself",
        }
    }
}

/// One destination row in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub location: Location,
    pub name: String,
    /// Indentation level; "My Files" is 0.
    pub depth: usize,
    pub blocked: Option<Blocked>,
    /// Whether the destination and every folder above it are public, so
    /// anyone with its link can see what is moved there.
    pub public: bool,
}

/// The folders among `items`.
pub fn folder_ids(items: &[GridItem]) -> Vec<FolderId> {
    items
        .iter()
        .filter_map(|i| match i {
            GridItem::Folder(f) => Some(f.id.clone()),
            GridItem::File(_) => None,
        })
        .collect()
}

/// Whether items in `from`, including the folders `moving`, may be moved to
/// `dest`.
pub fn check(
    tree: &FolderTree,
    moving: &[FolderId],
    from: &Location,
    dest: &Location,
) -> Result<(), Blocked> {
    if dest == from {
        return Err(Blocked::AlreadyHere);
    }
    if let Location::Folder(dest) = dest
        && moving.iter().any(|m| tree.is_within(dest, m))
    {
        return Err(Blocked::IntoItself);
    }
    Ok(())
}

/// Every destination, "My Files" first and then the folder tree in order.
pub fn targets(tree: &FolderTree, moving: &[FolderId], from: &Location) -> Vec<Target> {
    let mut out = vec![Target {
        location: Location::Root,
        name: "My Files".to_owned(),
        depth: 0,
        blocked: check(tree, moving, from, &Location::Root).err(),
        public: false,
    }];
    for (depth, folder) in tree.depth_first() {
        let location = Location::Folder(folder.id.clone());
        out.push(Target {
            blocked: check(tree, moving, from, &location).err(),
            public: tree.path_to(&folder.id).iter().all(|f| f.public),
            location,
            name: folder.name.clone(),
            depth: depth + 1,
        });
    }
    out
}

/// Ask where to move `items`, which are currently in `from`. Resolves to
/// the chosen destination, or `None` if the user cancels.
pub async fn choose(
    parent: &impl IsA<gtk::Widget>,
    tree: &FolderTree,
    items: &[GridItem],
    from: &Location,
) -> Option<Location> {
    let targets = Rc::new(targets(tree, &folder_ids(items), from));
    let heading = match items {
        [one] => format!("Move “{}”", one.name()),
        many => format!("Move {} Items", many.len()),
    };

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();
    list.update_property(&[gtk::accessible::Property::Label("Destination folder")]);
    for target in targets.iter() {
        list.append(&target_row(target));
    }
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(240)
        .max_content_height(360)
        .propagate_natural_height(true)
        .child(&list)
        .build();
    scrolled.add_css_class("card");

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search folders")
        .build();
    let note = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    note.add_css_class("caption");
    note.add_css_class("dim-label");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(&search);
    body.append(&scrolled);
    body.append(&note);

    let dialog = adw::AlertDialog::new(Some(&heading), None);
    dialog.set_extra_child(Some(&body));
    dialog.add_response("cancel", "_Cancel");
    dialog.add_response("move", "_Move");
    dialog.set_response_appearance("move", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("move"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("move", false);

    let query = Rc::new(RefCell::new(String::new()));
    let (filter_targets, filter_query) = (Rc::clone(&targets), Rc::clone(&query));
    list.set_filter_func(move |row| {
        let q = filter_query.borrow();
        q.is_empty()
            || usize::try_from(row.index())
                .ok()
                .and_then(|i| filter_targets.get(i))
                .is_some_and(|t| t.name.to_lowercase().contains(q.as_str()))
    });
    let list_weak = list.downgrade();
    search.connect_search_changed(move |entry| {
        *query.borrow_mut() = entry.text().trim().to_lowercase();
        if let Some(list) = list_weak.upgrade() {
            list.invalidate_filter();
        }
    });

    let (dialog_weak, row_targets) = (dialog.downgrade(), Rc::clone(&targets));
    list.connect_row_selected(move |_, row| {
        let target = row
            .and_then(|r| usize::try_from(r.index()).ok())
            .and_then(|i| row_targets.get(i));
        if let Some(dialog) = dialog_weak.upgrade() {
            dialog.set_response_enabled("move", target.is_some_and(|t| t.blocked.is_none()));
        }
        match target {
            Some(t) if t.public && t.blocked.is_none() => {
                note.set_text(&format!(
                    "“{}” is public: anyone with its link will be able to see what you move there.",
                    t.name
                ));
                note.set_visible(true);
            }
            _ => note.set_visible(false),
        }
    });
    let response = dialog.choose_future(Some(parent)).await;
    if response != "move" {
        return None;
    }
    let index = usize::try_from(list.selected_row()?.index()).ok()?;
    let target = targets.get(index)?;
    target.blocked.is_none().then(|| target.location.clone())
}

fn target_row(target: &Target) -> gtk::ListBoxRow {
    let icon = gtk::Image::from_icon_name(match target.location {
        Location::Root => "user-home-symbolic",
        Location::Folder(_) => "folder-symbolic",
    });
    let name = gtk::Label::builder()
        .label(target.name.as_str())
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let content = gtk::Box::builder()
        .spacing(8)
        .margin_start(i32::try_from(target.depth.min(12) * 18).unwrap_or(0))
        .build();
    // The reason sits under the name so it never squeezes the name.
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    text.append(&name);
    if let Some(blocked) = target.blocked {
        let reason = gtk::Label::builder()
            .label(blocked.reason())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        reason.add_css_class("caption");
        reason.add_css_class("dim-label");
        text.append(&reason);
    }
    content.append(&icon);
    content.append(&text);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .sensitive(target.blocked.is_none())
        .tooltip_text(target.name.as_str())
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&target.name)]);
    row
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use filester_app::api::models::Folder;

    fn folder(id: &str, parent: Option<&str>, public: bool) -> Folder {
        Folder {
            id: FolderId::new(id),
            name: id.to_uppercase(),
            public,
            file_count: 0,
            folder_count: 0,
            created_at: None,
            parent: parent.map(FolderId::new),
        }
    }

    /// a ─ b ─ c, and d at the top level. a, b and c are public.
    fn tree() -> FolderTree {
        FolderTree::build(vec![
            folder("a", None, true),
            folder("b", Some("a"), true),
            folder("c", Some("b"), true),
            folder("d", None, false),
        ])
    }

    fn loc(id: &str) -> Location {
        Location::Folder(FolderId::new(id))
    }

    #[test]
    fn cannot_move_to_the_current_folder() {
        let t = tree();
        assert_eq!(
            check(&t, &[], &loc("a"), &loc("a")),
            Err(Blocked::AlreadyHere)
        );
        assert_eq!(
            check(&t, &[], &Location::Root, &Location::Root),
            Err(Blocked::AlreadyHere)
        );
        assert_eq!(check(&t, &[], &loc("a"), &Location::Root), Ok(()));
    }

    #[test]
    fn cannot_move_a_folder_into_its_own_subtree() {
        let t = tree();
        let moving = [FolderId::new("b")];
        assert_eq!(
            check(&t, &moving, &loc("a"), &loc("b")),
            Err(Blocked::IntoItself)
        );
        assert_eq!(
            check(&t, &moving, &loc("a"), &loc("c")),
            Err(Blocked::IntoItself)
        );
        assert_eq!(check(&t, &moving, &loc("a"), &loc("d")), Ok(()));
        assert_eq!(check(&t, &moving, &loc("a"), &Location::Root), Ok(()));
    }

    #[test]
    fn targets_list_root_then_tree_with_reasons_and_visibility() {
        let t = tree();
        let list = targets(&t, &[FolderId::new("b")], &loc("a"));
        let summary: Vec<(&str, usize, Option<Blocked>, bool)> = list
            .iter()
            .map(|t| (t.name.as_str(), t.depth, t.blocked, t.public))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("My Files", 0, None, false),
                ("A", 1, Some(Blocked::AlreadyHere), true),
                ("B", 2, Some(Blocked::IntoItself), true),
                ("C", 3, Some(Blocked::IntoItself), true),
                ("D", 1, None, false),
            ]
        );
    }

    #[test]
    fn a_public_folder_under_a_private_one_is_not_public() {
        let t = FolderTree::build(vec![folder("p", None, false), folder("q", Some("p"), true)]);
        let list = targets(&t, &[], &Location::Root);
        assert!(!list.iter().find(|t| t.name == "Q").unwrap().public);
    }
}
