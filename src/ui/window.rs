//! The application window: sign-in, folder navigation, file grid,
//! transfers, preferences.
//!
//! # Structure
//!
//! ```text
//! AdwApplicationWindow
//! └─ AdwToastOverlay
//!    └─ GtkStack  "loading" | "signin" | "main"
//!       └─ AdwNavigationSplitView                      ("main")
//!          ├─ sidebar: HeaderBar, folder tree, storage gauge
//!          └─ content: HeaderBar (Up, title, Upload, New Folder, menu)
//!                      GtkStack "loading" | "grid" | "empty" | "error"
//!                      transfers panel (bottom bar)
//! ```
//!
//! # Threading and state
//!
//! [`MainWindow`] lives on the GTK thread behind an `Rc`. Mutable state sits
//! in a `RefCell` that is never borrowed across a GTK call that can emit
//! signals: values are copied out, the borrow ends, and then widgets are
//! touched. Each folder load carries a generation number, and results for a
//! location the user has since left are discarded.
//!
//! # Opening files
//!
//! With the default [`OpenMode::Browser`], opening a file shows its share
//! page in the browser. With [`OpenMode::LocalApp`], a public file is
//! downloaded into the [`FileCache`] (or taken from it) and handed to
//! `GtkFileLauncher`, which picks the default app through the desktop or
//! the `OpenURI` portal. Private files, and anything the download route
//! refuses, fall back to the browser. The cache is pruned at startup
//! according to the retention setting and emptied on sign-out.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use filester_app::api::models::{FileEntry, FileQuery, FolderFilter, FolderVisibility};
use filester_app::api::{
    ApiError, ApiKey, ClientConfig, DownloadConfig, FilesterClient, FolderTree, PublicDownloader,
};
use filester_app::cache::FileCache;
use filester_app::config;
use filester_app::secrets::{SecretStoreError, TokenStore};
use filester_app::worker::Worker;
use gtk::{gdk, gio, glib};
use secrecy::SecretString;
use tracing::{debug, info, warn};

use super::file_grid::{FileGrid, GridItem};
use super::format::{self, describe_error, is_safe_share_url, plain_toast};
use super::preferences::PreferencesDialog;
use super::settings::{self, OpenMode, Settings};
use super::sidebar::Sidebar;
use super::transfers::TransfersPanel;
use super::{Location, spawn_ui};

/// Longest wait for any keyring operation. Generous enough to type a
/// wallet password if the keyring prompts. It bounds the hang when no Secret
/// Service is running and D-Bus activation stalls.
const KEYRING_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on pages fetched for the top-level file list (100 files
/// each), so one view can never consume the whole hourly request budget.
const MAX_ROOT_PAGES: u32 = 50;
/// Uploads finishing within this window trigger a single refresh.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Window actions whose enabled state changes at runtime.
struct Actions {
    upload: gio::SimpleAction,
    new_folder: gio::SimpleAction,
    refresh: gio::SimpleAction,
    go_up: gio::SimpleAction,
    delete: gio::SimpleAction,
    copy_link: gio::SimpleAction,
    open: gio::SimpleAction,
    open_link: gio::SimpleAction,
    sign_out: gio::SimpleAction,
}

impl Actions {
    fn new() -> Self {
        let make = |name: &str| {
            let action = gio::SimpleAction::new(name, None);
            action.set_enabled(false);
            action
        };
        Self {
            upload: make("upload"),
            new_folder: make("new-folder"),
            refresh: make("refresh"),
            go_up: make("go-up"),
            delete: make("delete"),
            copy_link: make("copy-link"),
            open: make("open"),
            open_link: make("open-link"),
            sign_out: make("sign-out"),
        }
    }

    fn all(&self) -> [&gio::SimpleAction; 9] {
        [
            &self.upload,
            &self.new_folder,
            &self.refresh,
            &self.go_up,
            &self.delete,
            &self.copy_link,
            &self.open,
            &self.open_link,
            &self.sign_out,
        ]
    }
}

/// Widgets of the sign-in page.
struct SignInPage {
    root: adw::ToolbarView,
    entry: gtk::PasswordEntry,
    button: gtk::Button,
    spinner: gtk::Spinner,
    error: gtk::Label,
}

#[derive(Default)]
struct State {
    client: Option<FilesterClient>,
    tree: Rc<FolderTree>,
    location: Location,
    /// Incremented for every folder load. Never reset, so results from a
    /// previous session can never match.
    generation: u64,
}

/// The main window and its controllers.
pub struct MainWindow {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    pages: gtk::Stack,
    signin: SignInPage,
    split: adw::NavigationSplitView,
    sidebar: Sidebar,
    title: adw::WindowTitle,
    content: gtk::Stack,
    error_page: adw::StatusPage,
    grid: FileGrid,
    transfers: Rc<TransfersPanel>,
    actions: Actions,
    worker: Rc<Worker>,
    state: RefCell<State>,
    reload_pending: Cell<bool>,
    settings: Cell<Settings>,
    cache: FileCache,
    /// `None` only if the TLS backend failed to start; opening then falls
    /// back to the browser.
    downloader: Option<PublicDownloader>,
    /// The open Preferences dialog, so cache work can update it.
    preferences: RefCell<Option<Rc<PreferencesDialog>>>,
}

/// Run `f` if the window is still alive.
fn with(weak: &Weak<MainWindow>, f: impl FnOnce(&Rc<MainWindow>)) {
    if let Some(this) = weak.upgrade() {
        f(&this);
    }
}

impl MainWindow {
    /// Build the window. Call [`start`](Self::start) afterwards.
    pub fn new(app: &adw::Application, worker: Rc<Worker>) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(config::APP_NAME)
            .icon_name(app_icon_name())
            .default_width(1100)
            .default_height(720)
            .width_request(360)
            .height_request(294)
            .build();

        let signin = build_sign_in_page();
        let loading = build_loading_page();

        let sidebar = Sidebar::new();
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&adw::HeaderBar::new());
        sidebar_view.set_content(Some(sidebar.content()));
        sidebar_view.add_bottom_bar(sidebar.footer());
        let sidebar_page = adw::NavigationPage::new(&sidebar_view, config::APP_NAME);

        let (header, title) = build_content_header();

        let grid = FileGrid::new(&build_item_menu().upcast());
        let (content, error_page) = build_content_stack(&grid);

        let transfers = TransfersPanel::new();
        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&header);
        content_view.set_content(Some(&content));
        content_view.add_bottom_bar(transfers.widget());
        let content_page = adw::NavigationPage::new(&content_view, "My Files");

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content_page)
            .min_sidebar_width(220.0)
            .build();

        let pages = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        pages.add_named(&loading, Some("loading"));
        pages.add_named(&signin.root, Some("signin"));
        pages.add_named(&split, Some("main"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&pages));
        window.set_content(Some(&toasts));

        // Collapse to a single pane on narrow (phone-sized) windows.
        if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 640sp") {
            let breakpoint = adw::Breakpoint::new(condition);
            breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
            window.add_breakpoint(breakpoint);
        }

        let downloader = DownloadConfig::production()
            .and_then(PublicDownloader::new)
            .inspect_err(|err| warn!(error = %err, "downloads unavailable"))
            .ok();

        let this = Rc::new(Self {
            window,
            toasts,
            pages,
            signin,
            split,
            sidebar,
            title,
            content,
            error_page,
            grid,
            transfers,
            actions: Actions::new(),
            worker,
            state: RefCell::new(State::default()),
            reload_pending: Cell::new(false),
            settings: Cell::new(Settings::load(&settings::settings_path())),
            cache: FileCache::new(settings::cache_dir()),
            downloader,
            preferences: RefCell::new(None),
        });
        this.connect_actions();
        this.connect_signals();
        this
    }

    /// Raise the window.
    pub fn present(&self) {
        self.window.present();
    }

    /// Show the window, prune the download cache, and try the stored API key.
    pub fn start(self: &Rc<Self>) {
        self.pages.set_visible_child_name("loading");
        self.window.present();
        self.prune_cache();
        debug!("looking up stored API key");
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            with_keyring(|store| async move { store.load().await }),
            move |result| {
                debug!(
                    found = matches!(result, Ok(Ok(Some(_)))),
                    "stored API key lookup finished"
                );
                with(&weak, |this| match result {
                    Ok(Ok(Some(key))) => this.connect_with_key(key, false),
                    Ok(Ok(None)) => this.show_sign_in(None),
                    Ok(Err(err)) => {
                        warn!(error = %err, "secret store unavailable");
                        this.show_sign_in(Some(&format!(
                            "The system keyring is unavailable ({err}). You can still sign in, \
                             but your key won't be remembered."
                        )));
                    }
                    Err(_cancelled) => {}
                });
            },
        );
    }

    // --- Wiring -----------------------------------------------------------------

    fn connect_actions(self: &Rc<Self>) {
        let bind = |action: &gio::SimpleAction, f: fn(&Rc<Self>)| {
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| with(&weak, f));
            self.window.add_action(action);
        };
        bind(&self.actions.upload, Self::choose_upload);
        bind(&self.actions.new_folder, Self::prompt_new_folder);
        bind(&self.actions.refresh, Self::refresh);
        bind(&self.actions.go_up, Self::go_up);
        bind(&self.actions.delete, Self::confirm_delete);
        bind(&self.actions.copy_link, Self::copy_link);
        bind(&self.actions.open, Self::open_file);
        bind(&self.actions.open_link, Self::open_link);
        bind(&self.actions.sign_out, |this| this.sign_out(None));

        // Always available, signed in or not.
        for (name, f) in [
            ("about", Self::show_about as fn(&Rc<Self>)),
            ("preferences", Self::show_preferences),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| with(&weak, f));
            self.window.add_action(&action);
        }

        // Item shortcuts apply only while the grid has focus, so Delete in a
        // text entry can never trigger a deletion.
        let shortcuts = gtk::ShortcutController::new();
        for (trigger, action) in [("Delete", "win.delete"), ("<Control>c", "win.copy-link")] {
            if let Some(trigger) = gtk::ShortcutTrigger::parse_string(trigger) {
                shortcuts.add_shortcut(gtk::Shortcut::new(
                    Some(trigger),
                    Some(gtk::NamedAction::new(action)),
                ));
            }
        }
        self.grid.grid().add_controller(shortcuts);
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.signin
            .entry
            .connect_activate(move |_| with(&weak, Self::submit_sign_in));
        let weak = Rc::downgrade(self);
        self.signin
            .button
            .connect_clicked(move |_| with(&weak, Self::submit_sign_in));

        let weak = Rc::downgrade(self);
        self.sidebar.connect_location_selected(move |location| {
            with(&weak, |this| {
                this.split.set_show_content(true);
                this.navigate(location);
            });
        });

        let weak = Rc::downgrade(self);
        self.grid.connect_activate(move |item| {
            with(&weak, |this| match item {
                GridItem::Folder(folder) => this.navigate(Location::Folder(folder.id)),
                GridItem::File(_) => this.open_file(),
            });
        });
        let weak = Rc::downgrade(self);
        self.grid
            .connect_selection_changed(move || with(&weak, |this| this.update_item_actions()));

        // Drag and drop of local files onto the content area uploads them.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths: Vec<PathBuf> = files.files().iter().filter_map(gio::File::path).collect();
            if paths.is_empty() {
                return false;
            }
            with(&weak, |this| this.upload_paths(paths));
            true
        });
        self.content.add_controller(drop);

        // Closing with active transfers asks first.
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if this.transfers.active_count() == 0 {
                return glib::Propagation::Proceed;
            }
            this.confirm_close();
            glib::Propagation::Stop
        });
    }

    // --- Session ------------------------------------------------------------------

    fn submit_sign_in(self: &Rc<Self>) {
        let text = self.signin.entry.text();
        let key = match ApiKey::new(SecretString::from(text.as_str())) {
            Ok(key) => key,
            Err(err) => {
                self.show_sign_in(Some(&describe_error(&err)));
                return;
            }
        };
        // Drop the entry's copy as soon as possible. GTK's internal buffers
        // are outside our control; see README "Token handling".
        self.signin.entry.set_text("");
        self.connect_with_key(key, true);
    }

    /// Validate `key` against the API, optionally store it, and enter the
    /// main view.
    fn connect_with_key(self: &Rc<Self>, key: ApiKey, remember: bool) {
        self.set_sign_in_busy(true);
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                let client = FilesterClient::new(ClientConfig::production()?, Some(key.clone()))?;
                let account = client.account().await?;
                let saved = if remember {
                    with_keyring(|store| async move { store.save(&key).await }).await
                } else {
                    Ok(())
                };
                Ok::<_, ApiError>((client, account, saved))
            },
            move |result| {
                with(&weak, |this| {
                    this.set_sign_in_busy(false);
                    match result {
                        Ok(Ok((client, account, saved))) => {
                            if let Err(err) = saved {
                                warn!(error = %err, "could not store API key");
                                this.toast(
                                    "Signed in, but your key couldn't be saved to the system \
                                     keyring. You'll need to sign in again next time.",
                                );
                            }
                            info!("signed in");
                            this.sidebar.set_storage(Some(&account));
                            this.state.borrow_mut().client = Some(client);
                            this.set_session_actions(true);
                            this.pages.set_visible_child_name("main");
                            this.refresh();
                        }
                        Ok(Err(err)) => this.show_sign_in(Some(&describe_error(&err))),
                        Err(_cancelled) => {}
                    }
                });
            },
        );
    }

    /// Return to the sign-in page, cancel transfers, empty the download
    /// cache, and remove the stored key.
    fn sign_out(self: &Rc<Self>, message: Option<&str>) {
        self.transfers.abort_all();
        self.with_cache(FileCache::purge, |this, result| match result {
            Ok(report) => debug!(files = report.files_removed, "download cache emptied"),
            Err(err) => {
                warn!(error = %err, "could not empty the download cache");
                this.toast("Couldn't empty the download cache");
            }
        });
        {
            let mut state = self.state.borrow_mut();
            state.client = None;
            state.tree = Rc::new(FolderTree::default());
            state.location = Location::Root;
            state.generation += 1;
        }
        self.set_session_actions(false);
        self.grid.set_items(Vec::new());
        self.sidebar.clear();
        spawn_ui(
            &self.worker,
            with_keyring(|store| async move { store.clear().await }),
            |result| {
                if let Ok(Err(err)) = result {
                    warn!(error = %err, "could not remove stored API key");
                }
            },
        );
        info!("signed out");
        self.show_sign_in(message);
    }

    fn session_expired(self: &Rc<Self>) {
        self.sign_out(Some("Your API key was rejected. Please sign in again."));
    }

    // --- Navigation & loading -----------------------------------------------------

    /// Reload the folder tree and account, then the current folder.
    fn refresh(self: &Rc<Self>) {
        let Some(client) = self.client() else { return };
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                let (tree, account) = tokio::join!(client.folder_tree(), client.account());
                Ok::<_, ApiError>((tree?, account.ok()))
            },
            move |result| {
                with(&weak, |this| match result {
                    Ok(Ok((tree, account))) => this.apply_tree(tree, account.as_ref()),
                    Ok(Err(err)) => this.report_error(&err),
                    Err(_cancelled) => {}
                });
            },
        );
    }

    fn apply_tree(
        self: &Rc<Self>,
        tree: FolderTree,
        account: Option<&filester_app::api::models::Account>,
    ) {
        let tree = Rc::new(tree);
        let location = {
            let mut state = self.state.borrow_mut();
            if let Location::Folder(id) = &state.location
                && tree.get(id).is_none()
            {
                state.location = Location::Root;
            }
            state.tree = Rc::clone(&tree);
            state.location.clone()
        };
        self.sidebar.set_tree(tree, &location);
        if let Some(account) = account {
            self.sidebar.set_storage(Some(account));
        }
        self.load_location();
    }

    fn navigate(self: &Rc<Self>, location: Location) {
        if self.state.borrow().location == location {
            return;
        }
        self.state.borrow_mut().location = location.clone();
        self.sidebar.select(&location);
        self.load_location();
    }

    fn go_up(self: &Rc<Self>) {
        let parent = {
            let state = self.state.borrow();
            match &state.location {
                Location::Root => return,
                Location::Folder(id) => state
                    .tree
                    .parent(id)
                    .map_or(Location::Root, |p| Location::Folder(p.id.clone())),
            }
        };
        self.navigate(parent);
    }

    /// Fetch and show the contents of the current location.
    fn load_location(self: &Rc<Self>) {
        let Some(client) = self.client() else { return };
        let (location, tree, generation) = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            (
                state.location.clone(),
                Rc::clone(&state.tree),
                state.generation,
            )
        };
        self.update_header(&location, &tree);
        self.content.set_visible_child_name("loading");

        let subfolders: Vec<GridItem> = match &location {
            Location::Root => tree.roots().cloned().map(GridItem::Folder).collect(),
            Location::Folder(id) => tree.children(id).cloned().map(GridItem::Folder).collect(),
        };
        let fetch_location = location.clone();
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                match fetch_location {
                    Location::Root => fetch_root_files(&client).await,
                    Location::Folder(id) => client.folder_files(&id).await,
                }
            },
            move |result| {
                with(&weak, |this| {
                    if this.state.borrow().generation != generation {
                        return; // the user has navigated elsewhere
                    }
                    match result {
                        Ok(Ok(mut files)) => {
                            files.sort_by_cached_key(|f| f.name.to_lowercase());
                            let mut items = subfolders;
                            items.extend(files.into_iter().map(GridItem::File));
                            let empty = items.is_empty();
                            this.grid.set_items(items);
                            this.content.set_visible_child_name(if empty {
                                "empty"
                            } else {
                                "grid"
                            });
                            this.update_item_actions();
                        }
                        Ok(Err(ApiError::Unauthorized)) => this.session_expired(),
                        Ok(Err(err)) => {
                            format::set_status_description(&this.error_page, &describe_error(&err));
                            this.content.set_visible_child_name("error");
                        }
                        Err(_cancelled) => {}
                    }
                });
            },
        );
    }

    /// Coalesce refreshes after a burst of finished uploads.
    fn schedule_reload(self: &Rc<Self>) {
        if self.reload_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(RELOAD_DEBOUNCE, move || {
            with(&weak, |this| {
                this.reload_pending.set(false);
                this.refresh();
            });
        });
    }

    // --- Item operations ----------------------------------------------------------

    fn choose_upload(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Upload Files")
            .modal(true)
            .build();
        let window = self.window.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            match dialog.open_multiple_future(Some(&window)).await {
                Ok(files) => {
                    let paths: Vec<PathBuf> = files
                        .iter::<gio::File>()
                        .filter_map(|f| f.ok().and_then(|f| f.path()))
                        .collect();
                    with(&weak, |this| this.upload_paths(paths));
                }
                Err(err) if err.matches(gtk::DialogError::Dismissed) => {}
                Err(err) => warn!(error = %err, "file chooser failed"),
            }
        });
    }

    fn upload_paths(self: &Rc<Self>, paths: Vec<PathBuf>) {
        let Some(client) = self.client() else { return };
        let folder = self.state.borrow().location.folder_id().cloned();
        for path in paths {
            let weak = Rc::downgrade(self);
            self.transfers.enqueue_upload(
                &self.worker,
                client.clone(),
                path,
                folder.clone(),
                move |ok| {
                    if ok {
                        with(&weak, Self::schedule_reload);
                    }
                },
            );
        }
    }

    fn prompt_new_folder(self: &Rc<Self>) {
        let entry = gtk::Entry::builder()
            .placeholder_text("Folder name")
            .activates_default(true)
            .max_length(100)
            .build();
        let public = gtk::CheckButton::with_label("Anyone with the link can view");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.append(&entry);
        body.append(&public);

        let dialog = adw::AlertDialog::new(Some("New Folder"), None);
        dialog.set_extra_child(Some(&body));
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("create", "C_reate");
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("create", false);
        let dialog_weak = dialog.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.set_response_enabled("create", !entry.text().trim().is_empty());
            }
        });

        let window = self.window.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&window)).await == "create" {
                let name = entry.text().to_string();
                let visibility = if public.is_active() {
                    FolderVisibility::Public
                } else {
                    FolderVisibility::Private
                };
                with(&weak, |this| this.create_folder(name, visibility));
            }
        });
    }

    fn create_folder(self: &Rc<Self>, name: String, visibility: FolderVisibility) {
        let Some(client) = self.client() else { return };
        let parent = self.state.borrow().location.folder_id().cloned();
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                client
                    .create_folder(&name, parent.as_ref(), visibility, None)
                    .await
            },
            move |result| {
                with(&weak, |this| match result {
                    Ok(Ok(folder)) => {
                        this.toast(&format!("Created “{}”", folder.name));
                        this.refresh();
                    }
                    Ok(Err(err)) => this.report_error(&err),
                    Err(_cancelled) => {}
                });
            },
        );
    }

    fn confirm_delete(self: &Rc<Self>) {
        let Some(item) = self.grid.selected() else {
            return;
        };
        let body = match &item {
            GridItem::Folder(_) => {
                "The folder and every file inside it will be permanently deleted."
            }
            GridItem::File(_) => "The file will be permanently deleted.",
        };
        let heading = format!("Delete “{}”?", item.name());
        // AlertDialog heading and body are plain text unless *-use-markup is set.
        let dialog = adw::AlertDialog::new(Some(&heading), Some(body));
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("delete", "_Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let window = self.window.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&window)).await == "delete" {
                with(&weak, |this| this.delete_item(item));
            }
        });
    }

    fn delete_item(self: &Rc<Self>, item: GridItem) {
        let Some(client) = self.client() else { return };
        let name = item.name().to_owned();
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                match item {
                    GridItem::Folder(f) => client.delete_folders(&[f.id]).await,
                    GridItem::File(f) => client.delete_files(&[f.reference()]).await,
                }
            },
            move |result| {
                with(&weak, |this| match result {
                    Ok(Ok(report)) if report.failed_deletes == 0 => {
                        this.toast(&format!("Deleted “{name}”"));
                        this.refresh();
                    }
                    Ok(Ok(_)) => {
                        this.toast(&format!("Couldn't delete “{name}”"));
                        this.refresh();
                    }
                    Ok(Err(err)) => this.report_error(&err),
                    Err(_cancelled) => {}
                });
            },
        );
    }

    /// Share URL of the selected file, if it passes validation.
    fn selected_share_url(&self) -> Option<String> {
        match self.grid.selected()? {
            GridItem::File(FileEntry { url: Some(url), .. }) if is_safe_share_url(&url) => {
                Some(url)
            }
            _ => None,
        }
    }

    fn copy_link(self: &Rc<Self>) {
        if let Some(url) = self.selected_share_url() {
            self.window.clipboard().set_text(&url);
            self.toast("Link copied");
        }
    }

    fn open_link(self: &Rc<Self>) {
        if let Some(url) = self.selected_share_url() {
            self.open_url(&url);
        }
    }

    /// Open a validated share URL in the browser.
    fn open_url(self: &Rc<Self>, url: &str) {
        let weak = Rc::downgrade(self);
        gtk::UriLauncher::new(url).launch(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(err) = result
                    && !err.matches(gtk::DialogError::Dismissed)
                {
                    warn!(error = %err, "could not open link");
                    with(&weak, |this| this.toast("Couldn't open the link"));
                }
            },
        );
    }

    /// Open the selected file as the preferences say: in the browser, or
    /// (for public files) in the default local app.
    fn open_file(self: &Rc<Self>) {
        let Some(url) = self.selected_share_url() else {
            return;
        };
        if self.settings.get().open_mode == OpenMode::Browser {
            self.open_url(&url);
            return;
        }
        let Some(GridItem::File(file)) = self.grid.selected() else {
            return;
        };
        let (Some(slug), Some(downloader)) = (file.share_slug(), self.downloader.clone()) else {
            self.open_url(&url);
            return;
        };
        if self.location_is_private() {
            self.toast("Private files open in the browser");
            self.open_url(&url);
            return;
        }

        let (name, size) = (file.name, file.size);
        let (lookup_slug, lookup_name) = (slug.clone(), name.clone());
        self.with_cache(
            move |cache| Ok(cache.lookup(&lookup_slug, &lookup_name, size)),
            move |this, cached| match cached {
                Ok(Some(path)) => this.launch_local(&path, &name),
                _ => this.download_and_open(downloader, slug, name, size, url),
            },
        );
    }

    fn download_and_open(
        self: &Rc<Self>,
        downloader: PublicDownloader,
        slug: String,
        name: String,
        size: u64,
        url: String,
    ) {
        let weak = Rc::downgrade(self);
        let display_name = name.clone();
        self.transfers.enqueue_download(
            &self.worker,
            downloader,
            self.cache.clone(),
            slug,
            name,
            size,
            move |outcome| {
                with(&weak, |this| match outcome {
                    Ok(Ok(path)) => this.launch_local(&path, &display_name),
                    Ok(Err(ApiError::NotPublic)) => {
                        this.toast("This file is private, so it opens in the browser");
                        this.open_url(&url);
                    }
                    // Not report_error: a 401 from the download route says
                    // nothing about the API key, so it must not sign out.
                    Ok(Err(err)) => {
                        warn!(error = %err, "download failed");
                        this.toast(&describe_error(&err));
                    }
                    Err(_cancelled) => {}
                });
            },
        );
    }

    /// Hand a cached file to the desktop's default app for its type.
    fn launch_local(self: &Rc<Self>, path: &Path, name: &str) {
        let weak = Rc::downgrade(self);
        let name = name.to_owned();
        gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(err) = result
                    && !err.matches(gtk::DialogError::Dismissed)
                {
                    warn!(error = %err, "could not launch a local app");
                    with(&weak, |this| {
                        this.toast(&format!("No app could open “{name}”"));
                    });
                }
            },
        );
    }

    /// Whether the current folder, or any folder above it, is private. Files
    /// inherit privacy from their ancestors, so the public download route
    /// would refuse them anyway.
    fn location_is_private(&self) -> bool {
        let state = self.state.borrow();
        match &state.location {
            Location::Root => false,
            Location::Folder(id) => state.tree.path_to(id).iter().any(|f| !f.public),
        }
    }

    // --- Preferences & cache --------------------------------------------------------

    fn show_preferences(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let weak_clear = weak.clone();
        let dialog = Rc::new(PreferencesDialog::new(
            self.settings.get(),
            move |settings| with(&weak, |this| this.apply_settings(settings)),
            move || with(&weak_clear, Self::clear_cache),
        ));
        dialog.present(&self.window);
        *self.preferences.borrow_mut() = Some(dialog);
        self.update_cache_usage();
    }

    /// Adopt and save new settings. Retention takes effect at the next
    /// start, so adjusting the number never deletes files mid-edit.
    fn apply_settings(self: &Rc<Self>, settings: Settings) {
        if self.settings.replace(settings) == settings {
            return;
        }
        let data = settings.to_data();
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                tokio::task::spawn_blocking(move || {
                    settings::write_atomically(&settings::settings_path(), &data)
                })
                .await
                .map_err(|e| io::Error::other(e.to_string()))?
            },
            move |result| {
                if let Ok(Err(err)) = result {
                    warn!(error = %err, "could not save settings");
                    with(&weak, |this| this.toast("Couldn't save preferences"));
                }
            },
        );
    }

    /// Startup cleanup: always drop abandoned partial downloads, and drop
    /// files not opened within the retention period if that is enabled.
    fn prune_cache(self: &Rc<Self>) {
        let retention = self.settings.get().retention();
        self.with_cache(
            move |cache| cache.prune(retention),
            |_, result| match result {
                Ok(report) if report.files_removed > 0 => info!(
                    files = report.files_removed,
                    bytes = report.bytes_freed,
                    "removed old downloads from the cache"
                ),
                Ok(_) => {}
                Err(err) => warn!(error = %err, "download cache cleanup failed"),
            },
        );
    }

    fn clear_cache(self: &Rc<Self>) {
        self.with_cache(FileCache::clear, |this, result| {
            match result {
                Ok(report) => this.toast(&format!(
                    "Removed {} downloaded file{}",
                    report.files_removed,
                    if report.files_removed == 1 { "" } else { "s" }
                )),
                Err(err) => {
                    warn!(error = %err, "could not clear the download cache");
                    this.toast("Couldn't clear the download cache");
                }
            }
            this.update_cache_usage();
        });
    }

    fn update_cache_usage(self: &Rc<Self>) {
        self.with_cache(FileCache::usage, |this, result| {
            if let Some(dialog) = this.preferences.borrow().as_ref() {
                dialog.set_cache_usage(result.ok());
            }
        });
    }

    /// Run blocking cache work on the worker, then `done` on the GTK thread.
    fn with_cache<T: Send + 'static>(
        self: &Rc<Self>,
        work: impl FnOnce(&FileCache) -> io::Result<T> + Send + 'static,
        done: impl FnOnce(&Rc<Self>, io::Result<T>) + 'static,
    ) {
        let cache = self.cache.clone();
        let weak = Rc::downgrade(self);
        spawn_ui(
            &self.worker,
            async move {
                tokio::task::spawn_blocking(move || work(&cache))
                    .await
                    .map_err(|e| io::Error::other(e.to_string()))?
            },
            move |result| {
                if let Ok(result) = result {
                    with(&weak, |this| done(this, result));
                }
            },
        );
    }

    // --- Dialogs ------------------------------------------------------------------

    fn confirm_close(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Cancel Transfers?"),
            Some("Uploads and downloads in progress will be cancelled if you quit now."),
        );
        dialog.add_response("cancel", "_Keep Going");
        dialog.add_response("quit", "_Quit");
        dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let window = self.window.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&window)).await == "quit" {
                with(&weak, |this| {
                    this.transfers.abort_all();
                    this.window.destroy();
                });
            }
        });
    }

    fn show_about(self: &Rc<Self>) {
        adw::AboutDialog::builder()
            .application_name(config::APP_NAME)
            .application_icon(app_icon_name())
            .developer_name("scroguard")
            .version(env!("CARGO_PKG_VERSION"))
            .website(env!("CARGO_PKG_REPOSITORY"))
            .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues"))
            .license_type(gtk::License::Gpl30)
            .comments("An unofficial desktop client for Filester cloud storage. Not affiliated with Filester.")
            .build()
            .present(Some(&self.window));
    }

    // --- Small helpers --------------------------------------------------------------

    fn client(&self) -> Option<FilesterClient> {
        self.state.borrow().client.clone()
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(plain_toast(text));
    }

    fn report_error(self: &Rc<Self>, err: &ApiError) {
        if matches!(err, ApiError::Unauthorized) {
            self.session_expired();
        } else {
            warn!(error = %err, "operation failed");
            self.toast(&describe_error(err));
        }
    }

    fn show_sign_in(&self, error: Option<&str>) {
        self.set_sign_in_busy(false);
        self.signin.error.set_text(error.unwrap_or_default());
        self.signin.error.set_visible(error.is_some());
        self.pages.set_visible_child_name("signin");
        self.signin.entry.grab_focus();
    }

    fn set_sign_in_busy(&self, busy: bool) {
        self.signin.entry.set_sensitive(!busy);
        self.signin.button.set_sensitive(!busy);
        self.signin.spinner.set_spinning(busy);
        self.signin.spinner.set_visible(busy);
        if busy {
            self.signin.error.set_visible(false);
        }
    }

    fn set_session_actions(&self, signed_in: bool) {
        for action in self.actions.all() {
            action.set_enabled(signed_in);
        }
        self.update_item_actions();
    }

    fn update_header(&self, location: &Location, tree: &FolderTree) {
        let (title, subtitle) = match location {
            Location::Root => ("My Files".to_owned(), String::new()),
            Location::Folder(id) => {
                let path = tree.path_to(id);
                let title = path
                    .last()
                    .map_or_else(|| "Folder".to_owned(), |f| f.name.clone());
                let mut crumbs = vec!["My Files"];
                crumbs.extend(
                    path.iter()
                        .take(path.len().saturating_sub(1))
                        .map(|f| f.name.as_str()),
                );
                (title, crumbs.join(" / "))
            }
        };
        self.title.set_title(&title);
        self.title.set_subtitle(&subtitle);
        let signed_in = self.state.borrow().client.is_some();
        self.actions
            .go_up
            .set_enabled(signed_in && *location != Location::Root);
    }

    fn update_item_actions(&self) {
        let signed_in = self.state.borrow().client.is_some();
        let has_selection = self.grid.selected().is_some();
        let has_link = self.selected_share_url().is_some();
        self.actions.delete.set_enabled(signed_in && has_selection);
        self.actions.copy_link.set_enabled(signed_in && has_link);
        self.actions.open.set_enabled(signed_in && has_link);
        self.actions.open_link.set_enabled(signed_in && has_link);
    }
}

/// The installed application icon, or a generic themed icon when running
/// from a build tree where `data/icons` has not been installed.
fn app_icon_name() -> &'static str {
    let installed = gdk::Display::default()
        .is_some_and(|display| gtk::IconTheme::for_display(&display).has_icon(config::APP_ID));
    if installed {
        config::APP_ID
    } else {
        "folder-remote-symbolic"
    }
}

/// Open the keyring and run `op` against it, bounded by [`KEYRING_TIMEOUT`].
/// Errors are flattened to display strings, which never contain secrets.
async fn with_keyring<T, F, Fut>(op: F) -> Result<T, String>
where
    F: FnOnce(TokenStore) -> Fut,
    Fut: Future<Output = Result<T, SecretStoreError>>,
{
    let attempt = async { op(TokenStore::open().await?).await };
    match tokio::time::timeout(KEYRING_TIMEOUT, attempt).await {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_elapsed) => Err("the system keyring did not respond".to_owned()),
    }
}

/// Every top-level file, following pagination up to [`MAX_ROOT_PAGES`].
async fn fetch_root_files(client: &FilesterClient) -> Result<Vec<FileEntry>, ApiError> {
    let mut files = Vec::new();
    for page in 1..=MAX_ROOT_PAGES {
        let result = client
            .list_files(&FileQuery {
                page,
                per_page: 100,
                folder: FolderFilter::Root,
                search: None,
            })
            .await?;
        let count = result.files.len();
        files.extend(result.files);
        if count == 0 || u64::from(page) >= result.pagination.pages {
            break;
        }
    }
    Ok(files)
}

/// Content-pane header: Up, title (folder name + breadcrumb), Upload, New
/// Folder and the main menu.
fn build_content_header() -> (adw::HeaderBar, adw::WindowTitle) {
    let title = adw::WindowTitle::new("My Files", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let up = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Parent Folder")
        .action_name("win.go-up")
        .build();
    header.pack_start(&up);
    header.pack_end(&build_main_menu());
    let upload = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label("_Upload")
                .use_underline(true)
                .build(),
        )
        .tooltip_text("Upload Files")
        .action_name("win.upload")
        .build();
    upload.add_css_class("suggested-action");
    header.pack_end(&upload);
    header.pack_end(
        &gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text("New Folder")
            .action_name("win.new-folder")
            .build(),
    );
    (header, title)
}

/// Content-pane stack: "loading", "grid", "empty" and "error" pages.
fn build_content_stack(grid: &FileGrid) -> (gtk::Stack, adw::StatusPage) {
    let error_page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("Couldn't Load This Folder")
        .child(
            &gtk::Button::builder()
                .label("_Try Again")
                .use_underline(true)
                .halign(gtk::Align::Center)
                .action_name("win.refresh")
                .css_classes(["pill"])
                .build(),
        )
        .build();
    let empty_page = adw::StatusPage::builder()
        .icon_name("folder-symbolic")
        .title("This Folder Is Empty")
        .description("Drag files here or use Upload")
        .build();
    let content = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    content.add_named(&centered_spinner(), Some("loading"));
    content.add_named(grid.widget(), Some("grid"));
    content.add_named(&empty_page, Some("empty"));
    content.add_named(&error_page, Some("error"));
    (content, error_page)
}

fn build_sign_in_page() -> SignInPage {
    let entry = gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .placeholder_text("API key")
        .activates_default(true)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label("API key")]);
    let error = gtk::Label::builder()
        .wrap(true)
        .visible(false)
        .justify(gtk::Justification::Center)
        .build();
    error.add_css_class("error");
    let spinner = gtk::Spinner::builder().visible(false).build();
    let button = gtk::Button::builder()
        .label("_Sign In")
        .use_underline(true)
        .halign(gtk::Align::Center)
        .css_classes(["suggested-action", "pill"])
        .build();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
    form.append(&entry);
    form.append(&error);
    form.append(&button);
    form.append(&spinner);
    let clamp = adw::Clamp::builder().maximum_size(360).child(&form).build();
    let status = adw::StatusPage::builder()
        .icon_name(app_icon_name())
        .title("Sign In to Filester")
        .description(
            "Paste the API key from your Filester account settings. It is stored in your \
             system keyring.",
        )
        .child(&clamp)
        .vexpand(true)
        .build();
    let root = adw::ToolbarView::new();
    root.add_top_bar(&adw::HeaderBar::new());
    root.set_content(Some(&status));
    SignInPage {
        root,
        entry,
        button,
        spinner,
        error,
    }
}

fn build_loading_page() -> adw::ToolbarView {
    let label = gtk::Label::new(Some("Checking the system keyring…"));
    label.add_css_class("dim-label");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .valign(gtk::Align::Center)
        .build();
    content.append(&centered_spinner());
    content.append(&label);
    let page = adw::ToolbarView::new();
    page.add_top_bar(&adw::HeaderBar::new());
    page.set_content(Some(&content));
    page
}

fn centered_spinner() -> gtk::Spinner {
    gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build()
}

fn build_main_menu() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("_Refresh"), Some("win.refresh"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Preferences"), Some("win.preferences"));
    section.append(Some("_Sign Out"), Some("win.sign-out"));
    section.append(Some("_About Filester Manager"), Some("win.about"));
    menu.append_section(None, &section);
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .primary(true)
        .build()
}

fn build_item_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("_Open"), Some("win.open"));
    section.append(Some("Open in _Browser"), Some("win.open-link"));
    section.append(Some("_Copy Link"), Some("win.copy-link"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Delete"), Some("win.delete"));
    menu.append_section(None, &section);
    menu
}
