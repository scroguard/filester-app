//! Icon grid of the current folder's subfolders and files.

use adw::prelude::*;
use filester_app::api::models::{FileEntry, Folder};
use gtk::{gdk, gio, glib, graphene};

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

/// The grid view plus its model and selection.
pub struct FileGrid {
    scrolled: gtk::ScrolledWindow,
    grid: gtk::GridView,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
}

impl FileGrid {
    /// Build the grid. `context_menu` is shown on right-click of a tile,
    /// after that tile is selected.
    pub fn new(context_menu: &gio::MenuModel) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let grid = gtk::GridView::builder()
            .model(&selection)
            .min_columns(2)
            .max_columns(16)
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

        grid.set_factory(Some(&tile_factory(&grid, &selection, &popover)));

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

    /// Replace all tiles.
    pub fn set_items(&self, items: Vec<GridItem>) {
        let objects: Vec<glib::BoxedAnyObject> =
            items.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
    }

    /// The selected tile's item.
    pub fn selected(&self) -> Option<GridItem> {
        let boxed = self
            .selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()?;
        let item = boxed.try_borrow::<GridItem>().ok()?.clone();
        Some(item)
    }

    /// Call `f` when a tile is activated (double-click or Enter).
    pub fn connect_activate(&self, f: impl Fn(GridItem) + 'static) {
        let store = self.store.clone();
        self.grid.connect_activate(move |_, pos| {
            let item = store
                .item(pos)
                .and_downcast::<glib::BoxedAnyObject>()
                .and_then(|b| b.try_borrow::<GridItem>().ok().map(|i| i.clone()));
            if let Some(item) = item {
                f(item);
            }
        });
    }

    /// Call `f` whenever the selection changes.
    pub fn connect_selection_changed(&self, f: impl Fn() + 'static) {
        self.selection.connect_selected_item_notify(move |_| f());
    }
}

/// Factory for `Box(Image, name Label, detail Label)` tiles, each with a
/// right-click gesture that selects the tile and opens the context menu.
fn tile_factory(
    grid: &gtk::GridView,
    selection: &gtk::SingleSelection,
    popover: &gtk::PopoverMenu,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let grid_weak = grid.downgrade();
    let selection = selection.clone();
    let popover = popover.clone();
    factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
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

        let gesture = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let item_weak = item.downgrade();
        let (selection, popover, grid_weak) =
            (selection.clone(), popover.clone(), grid_weak.clone());
        gesture.connect_pressed(move |gesture, _, x, y| {
            let (Some(item), Some(grid)) = (item_weak.upgrade(), grid_weak.upgrade()) else {
                return;
            };
            selection.set_selected(item.position());
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
        tile.add_controller(gesture);
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
