//! Folder navigation sidebar and storage gauge.
//!
//! The tree is a `GtkTreeListModel` over a single "My Files" root row, whose
//! children are built lazily from the in-memory [`FolderTree`]. Expanding a
//! row costs nothing on the network. The whole hierarchy is fetched once per
//! refresh by `GET /api/v1/folders`.
//!
//! Every row, "My Files" included, is a drop target for tiles dragged from
//! the file grid, so items can be moved by dropping them on a folder here.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use filester_app::api::FolderTree;
use filester_app::api::models::{Account, Folder};
use gtk::{gio, glib};

use super::Location;
use super::file_grid::{DraggedItems, GridItem};

/// Called when grid tiles are dropped onto a row.
type DropHandler = Rc<dyn Fn(Vec<GridItem>, Location)>;

/// Model item for one sidebar row.
#[derive(Debug, Clone)]
struct Row {
    location: Location,
    name: String,
}

/// The sidebar: folder tree plus storage usage footer.
pub struct Sidebar {
    list: gtk::ListView,
    scrolled: gtk::ScrolledWindow,
    selection: gtk::SingleSelection,
    model: RefCell<Option<gtk::TreeListModel>>,
    tree: RefCell<Rc<FolderTree>>,
    /// Set while the selection is changed programmatically, so the
    /// selection handler does not echo it back as navigation.
    syncing: Rc<Cell<bool>>,
    footer: gtk::Box,
    storage_bar: gtk::ProgressBar,
    storage_label: gtk::Label,
    on_drop: Rc<RefCell<Option<DropHandler>>>,
}

impl Sidebar {
    pub fn new() -> Self {
        let selection = gtk::SingleSelection::builder()
            .autoselect(false)
            .can_unselect(true)
            .build();
        let on_drop = Rc::new(RefCell::new(None));
        let list = gtk::ListView::new(
            Some(selection.clone()),
            Some(row_factory(Rc::clone(&on_drop))),
        );
        list.add_css_class("navigation-sidebar");
        list.update_property(&[gtk::accessible::Property::Label("Folders")]);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let storage_bar = gtk::ProgressBar::new();
        storage_bar.update_property(&[gtk::accessible::Property::Label("Storage used")]);
        let storage_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
        storage_label.add_css_class("caption");
        storage_label.add_css_class("dim-label");
        let heading = gtk::Label::builder().label("Storage").xalign(0.0).build();
        heading.add_css_class("caption-heading");
        let footer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .visible(false)
            .build();
        footer.append(&heading);
        footer.append(&storage_bar);
        footer.append(&storage_label);

        Self {
            list,
            scrolled,
            selection,
            model: RefCell::new(None),
            tree: RefCell::new(Rc::new(FolderTree::default())),
            syncing: Rc::new(Cell::new(false)),
            footer,
            storage_bar,
            storage_label,
            on_drop,
        }
    }

    /// Scrollable folder list (the sidebar's main content).
    pub fn content(&self) -> &gtk::ScrolledWindow {
        &self.scrolled
    }

    /// Storage gauge, intended as the sidebar's bottom bar.
    pub fn footer(&self) -> &gtk::Box {
        &self.footer
    }

    /// Call `f` when the user picks a folder in the sidebar.
    pub fn connect_location_selected(&self, f: impl Fn(Location) + 'static) {
        let syncing = Rc::clone(&self.syncing);
        self.selection.connect_selected_item_notify(move |sel| {
            if syncing.get() {
                return;
            }
            if let Some(row) = selected_row(sel) {
                f(row.location);
            }
        });
    }

    /// Call `f(items, destination)` when grid tiles are dropped on a row.
    pub fn connect_drop(&self, f: impl Fn(Vec<GridItem>, Location) + 'static) {
        *self.on_drop.borrow_mut() = Some(Rc::new(f));
    }

    /// Replace the folder hierarchy and select `current`.
    pub fn set_tree(&self, tree: Rc<FolderTree>, current: &Location) {
        let root_store = gio::ListStore::new::<glib::BoxedAnyObject>();
        root_store.append(&glib::BoxedAnyObject::new(Row {
            location: Location::Root,
            name: "My Files".to_owned(),
        }));
        let children_of = Rc::clone(&tree);
        let model = gtk::TreeListModel::new(root_store, false, false, move |obj| {
            let boxed = obj.downcast_ref::<glib::BoxedAnyObject>()?;
            let row = boxed.try_borrow::<Row>().ok()?;
            let children: Vec<&Folder> = match &row.location {
                Location::Root => children_of.roots().collect(),
                Location::Folder(id) => children_of.children(id).collect(),
            };
            if children.is_empty() {
                return None;
            }
            let store = gio::ListStore::new::<glib::BoxedAnyObject>();
            for folder in children {
                store.append(&glib::BoxedAnyObject::new(Row {
                    location: Location::Folder(folder.id.clone()),
                    name: folder.name.clone(),
                }));
            }
            Some(store.upcast())
        });

        *self.tree.borrow_mut() = tree;
        self.syncing.set(true);
        self.selection.set_model(Some(&model));
        self.syncing.set(false);
        *self.model.borrow_mut() = Some(model);
        self.select(current);
    }

    /// Reveal and select `location` without emitting navigation.
    pub fn select(&self, location: &Location) {
        let Some(model) = self.model.borrow().clone() else {
            return;
        };
        let tree = Rc::clone(&self.tree.borrow());
        self.syncing.set(true);

        if let Some(root) = model.row(0) {
            root.set_expanded(true);
        }
        if let Location::Folder(id) = location {
            let path = tree.path_to(id);
            for ancestor in path.iter().take(path.len().saturating_sub(1)) {
                let loc = Location::Folder(ancestor.id.clone());
                if let Some(row) = find_position(&model, &loc).and_then(|pos| model.row(pos)) {
                    row.set_expanded(true);
                }
            }
        }
        match find_position(&model, location) {
            Some(pos) => {
                self.selection.set_selected(pos);
                self.list.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
            }
            None => self.selection.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.syncing.set(false);
    }

    /// Show account usage, or hide the gauge with `None`.
    pub fn set_storage(&self, account: Option<&Account>) {
        let Some(account) = account.filter(|a| a.storage_limit > 0) else {
            self.footer.set_visible(false);
            return;
        };
        #[allow(clippy::cast_precision_loss)] // display-only ratio
        let fraction = account.storage_used as f64 / account.storage_limit as f64;
        self.storage_bar.set_fraction(fraction.clamp(0.0, 1.0));
        self.storage_label.set_text(&format!(
            "{} of {} used",
            glib::format_size(account.storage_used),
            glib::format_size(account.storage_limit)
        ));
        self.footer.set_visible(true);
    }

    /// Forget everything (sign-out).
    pub fn clear(&self) {
        self.syncing.set(true);
        self.selection.set_model(None::<&gio::ListModel>);
        self.syncing.set(false);
        *self.model.borrow_mut() = None;
        *self.tree.borrow_mut() = Rc::new(FolderTree::default());
        self.set_storage(None);
    }
}

/// The `Row` behind the current selection, if any.
fn selected_row(selection: &gtk::SingleSelection) -> Option<Row> {
    let tree_row = selection
        .selected_item()
        .and_downcast::<gtk::TreeListRow>()?;
    let boxed = tree_row.item().and_downcast::<glib::BoxedAnyObject>()?;
    let row = boxed.try_borrow::<Row>().ok()?.clone();
    Some(row)
}

/// Position of the currently visible row for `location`.
fn find_position(model: &gtk::TreeListModel, location: &Location) -> Option<u32> {
    (0..model.n_items()).find(|&pos| {
        model
            .row(pos)
            .and_then(|r| r.item())
            .and_downcast::<glib::BoxedAnyObject>()
            .is_some_and(|b| {
                b.try_borrow::<Row>()
                    .is_ok_and(|row| &row.location == location)
            })
    })
}

/// The `Row` a list item currently shows.
fn bound_row(item: &gtk::ListItem) -> Option<Row> {
    let tree_row = item.item().and_downcast::<gtk::TreeListRow>()?;
    let boxed = tree_row.item().and_downcast::<glib::BoxedAnyObject>()?;
    let row = boxed.try_borrow::<Row>().ok()?.clone();
    Some(row)
}

/// Drop target that hands dragged grid tiles to the sidebar's handler,
/// with the row's location as the destination.
fn row_drop_target(
    item: &gtk::ListItem,
    on_drop: Rc<RefCell<Option<DropHandler>>>,
) -> gtk::DropTarget {
    let target = DraggedItems::drop_target();
    let item_weak = item.downgrade();
    target.connect_drop(move |_, value, _, _| {
        let row = item_weak.upgrade().as_ref().and_then(bound_row);
        let (Some(row), Some(items)) = (row, DraggedItems::from_value(value)) else {
            return false;
        };
        let handler = on_drop.borrow().clone();
        if let Some(handler) = handler {
            handler(items, row.location);
        }
        true
    });
    target
}

/// Factory for `TreeExpander > Box(Image, Label)` rows.
fn row_factory(on_drop: Rc<RefCell<Option<DropHandler>>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let icon = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&icon);
        content.append(&label);
        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&content));
        expander.add_controller(row_drop_target(item, Rc::clone(&on_drop)));
        item.set_child(Some(&expander));
    });
    factory.connect_bind(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(tree_row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };
        expander.set_list_row(Some(&tree_row));
        let Some(boxed) = tree_row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let Ok(row) = boxed.try_borrow::<Row>() else {
            return;
        };
        let Some(content) = expander.child() else {
            return;
        };
        if let Some(icon) = content.first_child().and_downcast::<gtk::Image>() {
            icon.set_icon_name(Some(match row.location {
                Location::Root => "user-home-symbolic",
                Location::Folder(_) => "folder-symbolic",
            }));
        }
        if let Some(label) = content.last_child().and_downcast::<gtk::Label>() {
            label.set_text(&row.name);
            label.set_tooltip_text(Some(&row.name));
        }
    });
    factory.connect_unbind(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() {
            expander.set_list_row(None);
        }
    });
    factory
}
