//! Icon grid of the current folder's subfolders and files.
//!
//! Selection is multiple: click selects one tile, Ctrl+click toggles,
//! Shift+click extends, dragging on empty space draws a selection box,
//! Ctrl+A selects all and Escape clears the selection.
//!
//! Tiles can be dragged onto a folder tile, or onto a folder in the
//! sidebar, to move them. The drag carries [`DraggedItems`] as an
//! in-process `GObject` value, so nothing leaves the application and drops
//! from other programs are never mistaken for it.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use filester_app::api::models::{FileEntry, Folder};
use gtk::{gdk, gio, glib, graphene};

use super::Location;

/// One tile in the grid.
#[derive(Debug, Clone)]
pub enum GridItem {
    /// A subfolder; activating it navigates into it.
    Folder(Folder),
    /// A file.
    File(FileEntry),
}

impl GridItem {
    /// Display name.
    pub fn name(&self) -> &str {
        match self {
            Self::Folder(f) => &f.name,
            Self::File(f) => &f.name,
        }
    }
}

/// Payload of an internal drag: the tiles being moved.
#[derive(Debug, Clone)]
pub struct DraggedItems(pub Vec<GridItem>);

impl DraggedItems {
    /// The drag payload carried by a drop's value, if it is one of ours.
    pub fn from_value(value: &glib::Value) -> Option<Vec<GridItem>> {
        let boxed = value.get::<glib::BoxedAnyObject>().ok()?;
        let items = boxed.try_borrow::<Self>().ok()?.0.clone();
        Some(items)
    }

    /// A drop target for these drags, for widgets outside the grid.
    pub fn drop_target() -> gtk::DropTarget {
        gtk::DropTarget::new(glib::BoxedAnyObject::static_type(), gdk::DragAction::MOVE)
    }
}

/// Called when tiles are dropped onto a folder tile.
type MoveHandler = Rc<dyn Fn(Vec<GridItem>, Location)>;

/// The grid view plus its model and selection.
pub struct FileGrid {
    scrolled: gtk::ScrolledWindow,
    grid: gtk::GridView,
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    on_move: Rc<RefCell<Option<MoveHandler>>>,
}

impl FileGrid {
    /// Build the grid. `context_menu` is shown on right-click of a tile.
    pub fn new(context_menu: &gio::MenuModel) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let grid = gtk::GridView::builder()
            .model(&selection)
            .min_columns(2)
            .max_columns(16)
            .enable_rubberband(true)
            .build();
        grid.update_property(&[gtk::accessible::Property::Label("Files and folders")]);

        // One popover shared by all tiles, parented to the grid.
        // A non-container parent must unparent its popover before disposal.
        let popover = gtk::PopoverMenu::from_model(Some(context_menu));
        popover.set_parent(&grid);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        let popover_for_destroy = popover.clone();
        grid.connect_destroy(move |_| popover_for_destroy.unparent());

        let on_move = Rc::new(RefCell::new(None));
        grid.set_factory(Some(&tile_factory(
            &grid,
            &selection,
            &popover,
            Rc::clone(&on_move),
        )));

        // Escape clears the selection (Ctrl+A / Ctrl+Shift+A are built in).
        let shortcuts = gtk::ShortcutController::new();
        let selection_weak = selection.downgrade();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::CallbackAction::new(move |_, _| {
                selection_weak
                    .upgrade()
                    .is_some_and(|s| s.unselect_all())
                    .into()
            })),
        ));
        grid.add_controller(shortcuts);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&grid)
            .build();
        Self {
            scrolled,
            grid,
            store,
            selection,
            on_move,
        }
    }

    /// The scrollable widget to place in the window.
    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scrolled
    }

    /// The grid itself, for attaching keyboard shortcuts.
    pub fn grid(&self) -> &gtk::GridView {
        &self.grid
    }

    /// Replace all tiles and clear the selection.
    pub fn set_items(&self, items: Vec<GridItem>) {
        let objects: Vec<glib::BoxedAnyObject> =
            items.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.selection.unselect_all();
    }

    /// The selected tiles' items, in grid order.
    pub fn selected(&self) -> Vec<GridItem> {
        selected_items(&self.selection)
    }

    /// Call `f` when a tile is activated (double-click or Enter).
    pub fn connect_activate(&self, f: impl Fn(GridItem) + 'static) {
        let store = self.store.clone();
        self.grid.connect_activate(move |_, pos| {
            if let Some(item) = item_at(&store, pos) {
                f(item);
            }
        });
    }

    /// Call `f` whenever the selection changes.
    pub fn connect_selection_changed(&self, f: impl Fn() + 'static) {
        self.selection.connect_selection_changed(move |_, _, _| f());
    }

    /// Call `f(items, destination)` when tiles are dropped onto a folder
    /// tile. Dropping a folder onto itself is ignored before `f` is called.
    pub fn connect_move(&self, f: impl Fn(Vec<GridItem>, Location) + 'static) {
        *self.on_move.borrow_mut() = Some(Rc::new(f));
    }
}

fn item_at(model: &impl IsA<gio::ListModel>, pos: u32) -> Option<GridItem> {
    let boxed = model.item(pos).and_downcast::<glib::BoxedAnyObject>()?;
    let item = boxed.try_borrow::<GridItem>().ok()?.clone();
    Some(item)
}

fn selected_items(selection: &gtk::MultiSelection) -> Vec<GridItem> {
    let bitset = selection.selection();
    let Ok(count) = u32::try_from(bitset.size()) else {
        return Vec::new();
    };
    (0..count)
        .filter_map(|i| item_at(selection, bitset.nth(i)))
        .collect()
}

/// Factory for `Box(Image, name Label, detail Label)` tiles. Each tile
/// has a right-click gesture for the context menu, a drag source, and a
/// drop target that accepts drags only while it shows a folder.
fn tile_factory(
    grid: &gtk::GridView,
    selection: &gtk::MultiSelection,
    popover: &gtk::PopoverMenu,
    on_move: Rc<RefCell<Option<MoveHandler>>>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let grid_weak = grid.downgrade();
    let selection = selection.clone();
    let popover = popover.clone();
    factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let tile = build_tile();
        tile.add_controller(context_menu_gesture(item, &selection, &popover, &grid_weak));
        tile.add_controller(drag_source(item, &selection));
        tile.add_controller(folder_drop_target(item, Rc::clone(&on_move)));
        item.set_child(Some(&tile));
    });
    factory.connect_bind(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let Ok(data) = boxed.try_borrow::<GridItem>() else {
            return;
        };
        let Some(tile) = item.child() else { return };
        let icon = tile.first_child().and_downcast::<gtk::Image>();
        let name = icon
            .as_ref()
            .and_then(WidgetExt::next_sibling)
            .and_downcast::<gtk::Label>();
        let detail = tile.last_child().and_downcast::<gtk::Label>();
        let (Some(icon), Some(name), Some(detail)) = (icon, name, detail) else {
            return;
        };

        match &*data {
            GridItem::Folder(folder) => {
                icon.set_icon_name(Some("folder"));
                detail.set_text(&folder_detail(folder));
            }
            GridItem::File(file) => {
                icon.set_from_gicon(&file_icon(file));
                detail.set_text(&glib::format_size(file.size));
            }
        }
        name.set_text(data.name());
        tile.set_tooltip_text(Some(data.name()));
    });
    factory
}

fn build_tile() -> gtk::Box {
    let tile = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(6)
        .margin_end(6)
        .width_request(112)
        .build();
    let icon = gtk::Image::builder().pixel_size(48).build();
    let name = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .justify(gtk::Justification::Center)
        .max_width_chars(14)
        .build();
    let detail = gtk::Label::new(None);
    detail.add_css_class("caption");
    detail.add_css_class("dim-label");
    tile.append(&icon);
    tile.append(&name);
    tile.append(&detail);
    tile
}

/// Right-click: keep the selection if the tile is part of it (so the menu
/// acts on all selected tiles), otherwise select only this tile. Then show
/// the context menu at the pointer.
fn context_menu_gesture(
    item: &gtk::ListItem,
    selection: &gtk::MultiSelection,
    popover: &gtk::PopoverMenu,
    grid_weak: &glib::WeakRef<gtk::GridView>,
) -> gtk::GestureClick {
    let gesture = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let item_weak = item.downgrade();
    let (selection, popover, grid_weak) = (selection.clone(), popover.clone(), grid_weak.clone());
    gesture.connect_pressed(move |gesture, _, x, y| {
        let (Some(item), Some(grid)) = (item_weak.upgrade(), grid_weak.upgrade()) else {
            return;
        };
        let pos = item.position();
        if !selection.is_selected(pos) {
            selection.select_item(pos, true);
        }
        let Some(tile) = gesture.widget() else { return };
        #[allow(clippy::cast_possible_truncation)] // widget coordinates fit easily in f32/i32
        let point = graphene::Point::new(x as f32, y as f32);
        if let Some(p) = tile.compute_point(&grid, &point) {
            #[allow(clippy::cast_possible_truncation)]
            let rect = gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1);
            popover.set_pointing_to(Some(&rect));
            popover.popup();
        }
    });
    gesture
}

/// Dragging a selected tile drags the whole selection. Dragging an
/// unselected tile selects and drags just that tile.
fn drag_source(item: &gtk::ListItem, selection: &gtk::MultiSelection) -> gtk::DragSource {
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    let item_weak = item.downgrade();
    let selection = selection.clone();
    source.connect_prepare(move |_, _, _| {
        let item = item_weak.upgrade()?;
        let pos = item.position();
        if !selection.is_selected(pos) {
            selection.select_item(pos, true);
        }
        let items = selected_items(&selection);
        if items.is_empty() {
            return None;
        }
        let payload = glib::BoxedAnyObject::new(DraggedItems(items));
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    source.connect_drag_begin(|source, _| {
        if let Some(tile) = source.widget() {
            let icon = gtk::WidgetPaintable::new(Some(&tile));
            source.set_icon(Some(&icon), tile.width() / 2, tile.height() / 2);
        }
    });
    source
}

/// Accept internal drags only while this tile shows a folder.
fn folder_drop_target(
    item: &gtk::ListItem,
    on_move: Rc<RefCell<Option<MoveHandler>>>,
) -> gtk::DropTarget {
    let target = DraggedItems::drop_target();
    let item_weak = item.downgrade();
    target.connect_accept(move |_, drop| {
        drop.formats()
            .contains_type(glib::BoxedAnyObject::static_type())
            && bound_folder(&item_weak).is_some()
    });
    let item_weak = item.downgrade();
    target.connect_drop(move |_, value, _, _| {
        let (Some(folder), Some(items)) =
            (bound_folder(&item_weak), DraggedItems::from_value(value))
        else {
            return false;
        };
        // Releasing a dragged folder over itself is not a move.
        if items
            .iter()
            .any(|i| matches!(i, GridItem::Folder(f) if f.id == folder.id))
        {
            return false;
        }
        let handler = on_move.borrow().clone();
        if let Some(handler) = handler {
            handler(items, Location::Folder(folder.id));
        }
        true
    });
    target
}

fn bound_folder(item: &glib::WeakRef<gtk::ListItem>) -> Option<Folder> {
    let boxed = item
        .upgrade()?
        .item()
        .and_downcast::<glib::BoxedAnyObject>()?;
    match &*boxed.try_borrow::<GridItem>().ok()? {
        GridItem::Folder(folder) => Some(folder.clone()),
        GridItem::File(_) => None,
    }
}

fn folder_detail(folder: &Folder) -> String {
    match folder.file_count.saturating_add(folder.folder_count) {
        0 => "Empty".to_owned(),
        1 => "1 item".to_owned(),
        n => format!("{n} items"),
    }
}

/// Themed icon for the server-reported MIME type, with a generic fallback.
fn file_icon(file: &FileEntry) -> gio::Icon {
    file.mime_type
        .as_deref()
        .and_then(gio::content_type_from_mime_type)
        .map_or_else(
            || gio::content_type_get_icon("application/octet-stream"),
            |ct| gio::content_type_get_icon(&ct),
        )
}
