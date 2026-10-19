//! The Preferences dialog: how files open, and download-cache retention.
//!
//! The dialog only edits a [`Settings`] value. Saving, applying and the cache
//! work itself belong to the window, which passes callbacks in.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::settings::{MAX_RETENTION_DAYS, MIN_RETENTION_DAYS, OpenMode, Settings};

/// Widgets whose state makes up a [`Settings`] value.
struct Inputs {
    local_app: gtk::CheckButton,
    auto_clean: adw::SwitchRow,
    retention: adw::SpinRow,
}

impl Inputs {
    fn read(&self) -> Settings {
        Settings {
            open_mode: if self.local_app.is_active() {
                OpenMode::LocalApp
            } else {
                OpenMode::Browser
            },
            auto_clean: self.auto_clean.is_active(),
            // The spin row's range keeps this within MIN..=MAX.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            retention_days: self.retention.value().round() as u32,
        }
    }
}

/// An open Preferences dialog.
pub struct PreferencesDialog {
    dialog: adw::PreferencesDialog,
    cache_row: adw::ActionRow,
    clear: gtk::Button,
}

impl PreferencesDialog {
    /// Build the dialog showing `settings`. `on_change` runs with the new
    /// value after every edit. `on_clear` runs when Clear is pressed; call
    /// [`set_cache_usage`](Self::set_cache_usage) when it finishes.
    pub fn new(
        settings: Settings,
        on_change: impl Fn(Settings) + 'static,
        on_clear: impl Fn() + 'static,
    ) -> Self {
        let (opening, local_app) = build_opening_group(settings);
        let (cache, cache_inputs) = build_cache_group(settings);
        let CacheInputs {
            auto_clean,
            retention,
            cache_row,
            clear,
        } = cache_inputs;

        let page = adw::PreferencesPage::new();
        page.add(&opening);
        page.add(&cache);
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Preferences");
        dialog.add(&page);

        let inputs = Rc::new(Inputs {
            local_app,
            auto_clean,
            retention,
        });
        let on_change = Rc::new(on_change);
        let emit = {
            let inputs = Rc::clone(&inputs);
            move || on_change(inputs.read())
        };
        let emit = Rc::new(emit);
        {
            let emit = Rc::clone(&emit);
            // Choosing either radio button toggles `local_app`, so this sees
            // every change of mode exactly once.
            inputs.local_app.connect_toggled(move |_| emit());
        }
        {
            let emit = Rc::clone(&emit);
            inputs
                .auto_clean
                .connect_notify_local(Some("active"), move |_, _| emit());
        }
        {
            let emit = Rc::clone(&emit);
            inputs
                .retention
                .connect_notify_local(Some("value"), move |_, _| emit());
        }
        let row = cache_row.downgrade();
        clear.connect_clicked(move |button| {
            button.set_sensitive(false);
            if let Some(row) = row.upgrade() {
                row.set_subtitle("Clearing…");
            }
            on_clear();
        });

        Self {
            dialog,
            cache_row,
            clear,
        }
    }

    /// Show the dialog over `parent`.
    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
    }

    /// Show the cache size, or that it could not be measured.
    pub fn set_cache_usage(&self, bytes: Option<u64>) {
        let text = match bytes {
            Some(0) => "Empty".to_owned(),
            Some(n) => glib::format_size(n).to_string(),
            None => "Unknown".to_owned(),
        };
        self.cache_row.set_subtitle(&text);
        self.clear.set_sensitive(bytes != Some(0));
    }
}

/// "Opening Files": browser vs. default app. Returns the group and the
/// "default app" radio button.
fn build_opening_group(settings: Settings) -> (adw::PreferencesGroup, gtk::CheckButton) {
    let browser = gtk::CheckButton::builder()
        .active(settings.open_mode == OpenMode::Browser)
        .valign(gtk::Align::Center)
        .build();
    let local_app = gtk::CheckButton::builder()
        .group(&browser)
        .active(settings.open_mode == OpenMode::LocalApp)
        .valign(gtk::Align::Center)
        .build();
    let browser_row = adw::ActionRow::builder()
        .title("Open in Browser")
        .subtitle("View files on filester.me. Nothing is saved on this computer.")
        .activatable_widget(&browser)
        .build();
    browser_row.add_prefix(&browser);
    let local_row = adw::ActionRow::builder()
        .title("Open in Default App")
        .subtitle(
            "Public files are downloaded to this computer, then opened with the app for their \
             type. Private files still open in the browser.",
        )
        .activatable_widget(&local_app)
        .build();
    local_row.add_prefix(&local_app);
    let group = adw::PreferencesGroup::builder()
        .title("Opening Files")
        .description("What happens when you open a file")
        .build();
    group.add(&browser_row);
    group.add(&local_row);
    (group, local_app)
}

/// Interactive widgets of the "Downloaded Files" group.
struct CacheInputs {
    auto_clean: adw::SwitchRow,
    retention: adw::SpinRow,
    cache_row: adw::ActionRow,
    clear: gtk::Button,
}

/// "Downloaded Files": automatic cleanup, retention, size and Clear.
fn build_cache_group(settings: Settings) -> (adw::PreferencesGroup, CacheInputs) {
    let auto_clean = adw::SwitchRow::builder()
        .title("Remove Old Files Automatically")
        .subtitle("Checked each time Filester starts")
        .active(settings.auto_clean)
        .build();
    let retention = adw::SpinRow::with_range(
        f64::from(MIN_RETENTION_DAYS),
        f64::from(MAX_RETENTION_DAYS),
        1.0,
    );
    retention.set_title("Keep Files For");
    retention.set_subtitle("Days since a file was last opened");
    retention.set_value(f64::from(settings.retention_days));
    auto_clean
        .bind_property("active", &retention, "sensitive")
        .sync_create()
        .build();
    let clear = gtk::Button::builder()
        .label("_Clear")
        .use_underline(true)
        .valign(gtk::Align::Center)
        .tooltip_text("Delete every downloaded file from this computer")
        .build();
    let cache_row = adw::ActionRow::builder()
        .title("Cache Size")
        .subtitle("Calculating…")
        .build();
    cache_row.add_suffix(&clear);
    let group = adw::PreferencesGroup::builder()
        .title("Downloaded Files")
        .description(
            "Files opened in a local app stay in a cache on this computer, readable only by your \
             user account, until they are removed. Signing out removes them all.",
        )
        .build();
    group.add(&auto_clean);
    group.add(&retention);
    group.add(&cache_row);
    (
        group,
        CacheInputs {
            auto_clean,
            retention,
            cache_row,
            clear,
        },
    )
}
