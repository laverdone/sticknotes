// StickNotes – post-it markdown per GNOME
// main.rs: application bootstrap, actions, manager window, reminder daemon.

mod db;
mod gnome;
mod md;
mod note;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use chrono::Local;
use gtk::glib;
use gtk::{gio, Align, Orientation};

use db::{Database, Note};
use note::NoteHandle;

/// In-memory registry of the currently open note windows.
type NotesMap = Rc<RefCell<HashMap<i64, NoteHandle>>>;

fn main() {
    // The program name is used by GNOME to associate the windows with the
    // .desktop entry, so it must match the application id.
    glib::set_prgname(Some(gnome::APP_ID));

    let app_id = std::env::var("STICKNOTES_APP_ID").unwrap_or_else(|_| gnome::APP_ID.to_string());
    let app = adw::Application::builder().application_id(&app_id).build();

    let db = match Database::open_default() {
        Ok(db) => Arc::new(Mutex::new(db)),
        Err(err) => {
            eprintln!("Impossibile aprire il database SQLite: {err}");
            std::process::exit(1);
        }
    };

    // GNOME integration: .desktop entry + icon so StickNotes shows up in the
    // dash / app grid with its own icon, and in notifications; plus the shell
    // extension that keeps a StickNotes icon in the top bar.
    gnome::install_desktop_entry();
    gnome::install_shell_extension();

    let notes: NotesMap = Rc::new(RefCell::new(HashMap::new()));
    let activated = Rc::new(Cell::new(false));
    // Weak reference to the manager window: if the user closes it, a later
    // activation of the application must be able to create it again.
    let manager: Rc<RefCell<Option<glib::WeakRef<gtk::ApplicationWindow>>>> =
        Rc::new(RefCell::new(None));

    app.connect_activate({
        let db = db.clone();
        let notes = notes.clone();
        let activated = activated.clone();
        let manager = manager.clone();
        move |app| {
            if activated.replace(true) {
                // Second activation (e.g. the launcher is clicked again):
                // just bring the existing windows back.
                for handle in notes.borrow().values() {
                    handle.window.present();
                }
                let existing = manager.borrow().as_ref().and_then(|weak| weak.upgrade());
                match existing {
                    // A destroyed window may still be alive because of a
                    // reference cycle: is_realized() tells us it is usable.
                    Some(window) if window.is_realized() => window.present(),
                    _ => {
                        let window = build_manager(app, db.clone(), notes.clone());
                        window.present();
                        *manager.borrow_mut() = Some(window.downgrade());
                    }
                }
                return;
            }

            gnome::register_icon();
            load_css();
            note::rebuild_cache(&db);
            setup_actions(app, &db, &notes);
            start_reminder_daemon(&db);

            // If the GNOME Shell extension is active, keep the process alive
            // in the background so its top-bar icon always works.
            if gnome::shell_extension_enabled() {
                // Keep the hold guard alive for the whole process lifetime:
                // the top-bar icon must keep working with no open windows.
                std::mem::forget(app.hold());
            }

            // Restore the notes that are marked as "open at startup".
            let saved = db
                .lock()
                .ok()
                .and_then(|conn| conn.all_notes().ok())
                .unwrap_or_default();
            for note in saved {
                if note.auto_open {
                    // Notes restored from the database open in Markdown view.
                    open_note_window(app, &db, &notes, note.id, true);
                }
            }

            let window = build_manager(app, db.clone(), notes.clone());
            window.present();
            *manager.borrow_mut() = Some(window.downgrade());
        }
    });

    let _ = app.run();
}

fn new_blank_note() -> Note {
    Note {
        id: 0,
        title: String::new(),
        body_md: "# Nuova nota\n\nScrivi in **markdown**…\n\n- elenca qui\n- incolla immagini con Ctrl+V\n"
            .to_string(),
        color: "#fff4c2".to_string(),
        x: 120,
        y: 120,
        w: 360,
        h: 400,
        pinned: false,
        all_workspaces: true,
        created_at: Local::now().format("%Y-%m-%d %H:%M").to_string(),
        daily_reminder: None,
        last_reminder_date: None,
        auto_open: true,
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

fn setup_actions(app: &adw::Application, db: &Arc<Mutex<Database>>, notes: &NotesMap) {
    let new_note = gio::SimpleAction::new("new-note", None);
    {
        let app = app.clone();
        let db = db.clone();
        let notes = notes.clone();
        new_note.connect_activate(move |_, _| create_and_open(&app, &db, &notes));
    }
    app.add_action(&new_note);

    let show_all = gio::SimpleAction::new("show-all", None);
    {
        let notes = notes.clone();
        show_all.connect_activate(move |_, _| {
            for handle in notes.borrow().values() {
                handle.window.present();
            }
        });
    }
    app.add_action(&show_all);

    let quit = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        quit.connect_activate(move |_, _| app.quit());
    }
    app.add_action(&quit);

    // Application autostart at login (stateful: the menu shows a checkmark).
    let autostart = gio::SimpleAction::new_stateful(
        "autostart",
        None,
        &gnome::autostart_enabled().to_variant(),
    );
    autostart.connect_activate(|action, _| {
        let current = action
            .state()
            .and_then(|state| state.get::<bool>())
            .unwrap_or(false);
        let next = !current;
        gnome::set_autostart(next);
        action.set_state(&next.to_variant());
    });
    app.add_action(&autostart);

    // (Re)install the GNOME Shell extension that shows the persistent icon in
    // the top bar and lets the user create notes from there.
    let shell_icon = gio::SimpleAction::new("install-shell-icon", None);
    shell_icon.connect_activate(|_, _| {
        gnome::install_shell_extension();
        let enabled = gnome::shell_extension_enabled();
        gnome::notify(
            "shell-icon",
            "StickNotes",
            if enabled {
                "Icona della barra superiore attiva. Se non la vedi, esci e rientra dalla sessione GNOME una volta."
            } else {
                "Estensione installata. Abilitala con «gnome-extensions enable sticknotes@org.sticknotes.app» (potrebbe servire un nuovo accesso alla sessione)."
            },
        );
    });
    app.add_action(&shell_icon);

    let test_notification = gio::SimpleAction::new("test-notification", None);
    test_notification.connect_activate(|_, _| {
        gnome::notify(
            "test",
            "StickNotes",
            "Notifica di prova: il promemoria funziona.",
        );
    });
    app.add_action(&test_notification);

    app.set_accels_for_action("app.new-note", &["<Control>n"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

fn create_and_open(app: &adw::Application, db: &Arc<Mutex<Database>>, notes: &NotesMap) {
    let mut note = new_blank_note();
    let id = {
        let Ok(conn) = db.lock() else {
            return;
        };
        conn.create_note(&mut note).unwrap_or(0)
    };
    if id > 0 {
        // A brand new note starts in edit mode.
        open_note_window(app, db, notes, id, false);
    }
}

fn open_note_window(
    app: &adw::Application,
    db: &Arc<Mutex<Database>>,
    notes: &NotesMap,
    id: i64,
    start_in_view: bool,
) {
    if let Some(handle) = notes.borrow().get(&id) {
        handle.window.present();
        return;
    }

    let Some(note) = db
        .lock()
        .ok()
        .and_then(|conn| conn.get_note(id).ok().flatten())
    else {
        return;
    };

    // The callback is stored inside the window signals, while the registry
    // owns the window: use a weak reference to avoid a reference cycle.
    let db_for_close = db.clone();
    let notes_weak: Weak<RefCell<HashMap<i64, NoteHandle>>> = Rc::downgrade(notes);
    let on_closed: Rc<dyn Fn(i64)> = Rc::new(move |note_id| {
        if let Ok(conn) = db_for_close.lock() {
            let _ = conn.delete_note(note_id);
        }
        if let Some(map) = notes_weak.upgrade() {
            if let Some(handle) = map.borrow_mut().remove(&note_id) {
                handle.window.destroy();
            }
        }
    });

    let handle = note::build_note_window(app, db.clone(), &note, on_closed, start_in_view);
    notes.borrow_mut().insert(id, handle);
}

// ---------------------------------------------------------------------------
// Manager window: lists all notes, lets you reopen/delete them and create
// new ones.
// ---------------------------------------------------------------------------

fn build_manager(
    app: &adw::Application,
    db: Arc<Mutex<Database>>,
    notes: NotesMap,
) -> gtk::ApplicationWindow {
    // A plain GtkApplicationWindow is used here because AdwApplicationWindow
    // does not support set_titlebar().
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("StickNotes")
        .default_width(440)
        .default_height(540)
        .build();
    window.set_icon_name(Some(gnome::APP_ID));

    let header = adw::HeaderBar::new();
    let title = gtk::Label::new(Some("StickNotes"));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));

    let new_button = gtk::Button::from_icon_name("list-add-symbolic");
    new_button.set_tooltip_text(Some("Nuova nota (Ctrl+N)"));
    {
        let app = app.clone();
        let db = db.clone();
        let notes = notes.clone();
        new_button.connect_clicked(move |_| create_and_open(&app, &db, &notes));
    }
    header.pack_start(&new_button);

    // A plain Button + Popover is used instead of GtkMenuButton: GTK 4.24
    // emits "Duplicate call to gdk_frame_timings_presented()" on the first
    // click of a menu button (upstream issue #8305).
    let menu_button = gtk::Button::from_icon_name("open-menu-symbolic");
    menu_button.add_css_class("flat");
    menu_button.set_tooltip_text(Some("Menu"));

    let menu_popover = gtk::Popover::new();
    menu_popover.set_parent(&menu_button);

    let menu_box = gtk::Box::new(Orientation::Vertical, 2);
    menu_box.set_margin_start(6);
    menu_box.set_margin_end(6);
    menu_box.set_margin_top(6);
    menu_box.set_margin_bottom(6);

    let show_all = menu_item("Mostra tutte le note");
    {
        let notes = notes.clone();
        let popover = menu_popover.clone();
        show_all.connect_clicked(move |_| {
            for handle in notes.borrow().values() {
                handle.window.present();
            }
            popover.popdown();
        });
    }
    menu_box.append(&show_all);

    let autostart = gtk::CheckButton::with_label("Avvia StickNotes all'accesso");
    autostart.set_active(gnome::autostart_enabled());
    autostart.connect_toggled(|check| gnome::set_autostart(check.is_active()));
    menu_box.append(&autostart);

    let shell_icon = menu_item("Icona nella barra GNOME");
    {
        let app = app.clone();
        let popover = menu_popover.clone();
        shell_icon.connect_clicked(move |_| {
            app.activate_action("install-shell-icon", None);
            popover.popdown();
        });
    }
    menu_box.append(&shell_icon);

    let test_notification = menu_item("Prova notifica");
    {
        let app = app.clone();
        let popover = menu_popover.clone();
        test_notification.connect_clicked(move |_| {
            app.activate_action("test-notification", None);
            popover.popdown();
        });
    }
    menu_box.append(&test_notification);

    menu_box.append(&gtk::Separator::new(Orientation::Horizontal));

    let quit = menu_item("Esci");
    {
        let app = app.clone();
        quit.connect_clicked(move |_| app.quit());
    }
    menu_box.append(&quit);

    menu_popover.set_child(Some(&menu_box));
    menu_button.connect_clicked(move |_| menu_popover.popup());
    header.pack_end(&menu_button);

    window.set_titlebar(Some(&header));

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_top(12);
    list.set_margin_bottom(12);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&list));

    window.set_child(Some(&scrolled));

    refresh_list(window.upcast_ref(), app, &db, &notes, &list);

    // Keep the list in sync whenever the manager is shown again.
    window.connect_map({
        let app = app.clone();
        let db = db.clone();
        let notes = notes.clone();
        let list = list.clone();
        // Weak reference: the closure is owned by the window, so a strong
        // reference here would keep the window alive after it is destroyed.
        let window = window.downgrade();
        move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            refresh_list(window.upcast_ref(), &app, &db, &notes, &list);
        }
    });

    window
}

/// Left-aligned, flat menu item for the popover menu.
fn menu_item(label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.set_halign(Align::Fill);

    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    button.set_child(Some(&text));
    button
}

fn refresh_list(
    parent: &gtk::Window,
    app: &adw::Application,
    db: &Arc<Mutex<Database>>,
    notes: &NotesMap,
    list: &gtk::ListBox,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let all = db
        .lock()
        .ok()
        .and_then(|conn| conn.all_notes().ok())
        .unwrap_or_default();

    if all.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("Nessuna nota");
        row.set_subtitle("Premi + per crearne una");
        row.set_activatable(false);
        list.append(&row);
        return;
    }

    for note in all {
        let row = adw::ActionRow::new();
        let title = if note.title.trim().is_empty() {
            preview_of(&note)
        } else {
            note.title.clone()
        };
        row.set_title(&title);
        row.set_subtitle(&format!("{} · {}", note.created_at, preview_of(&note)));
        row.set_activatable(true);

        let open_button = gtk::Button::from_icon_name("go-next-symbolic");
        open_button.add_css_class("flat");
        open_button.set_valign(Align::Center);
        open_button.set_tooltip_text(Some("Apri la nota"));
        {
            let app = app.clone();
            let db = db.clone();
            let notes = notes.clone();
            let id = note.id;
            open_button.connect_clicked(move |_| open_note_window(&app, &db, &notes, id, true));
        }
        row.add_suffix(&open_button);

        let delete_button = gtk::Button::from_icon_name("user-trash-symbolic");
        delete_button.add_css_class("flat");
        delete_button.set_valign(Align::Center);
        delete_button.set_tooltip_text(Some("Elimina la nota"));
        {
            let app = app.clone();
            let db = db.clone();
            let notes = notes.clone();
            // Weak references: this callback is owned by a row inside the
            // list, and keeping the manager window alive here would leave a
            // destroyed window behind after closing it.
            let list = list.downgrade();
            let parent = parent.downgrade();
            let id = note.id;
            let title = title.clone();
            delete_button.connect_clicked(move |_| {
                let Some(parent) = parent.upgrade() else {
                    return;
                };
                let Some(list) = list.upgrade() else {
                    return;
                };
                let dialog = adw::MessageDialog::new(
                    Some(&parent),
                    Some("Eliminare la nota?"),
                    Some(&format!("«{title}» verrà eliminata definitivamente.")),
                );
                dialog.add_response("cancel", "Annulla");
                dialog.add_response("delete", "Elimina");
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                {
                    let app = app.clone();
                    let db = db.clone();
                    let notes = notes.clone();
                    let list = list.clone();
                    let parent = parent.clone();
                    dialog.connect_response(
                        None,
                        move |dialog: &adw::MessageDialog, response: &str| {
                            if response == "delete" {
                                if let Ok(conn) = db.lock() {
                                    let _ = conn.delete_note(id);
                                }
                                if let Some(handle) = notes.borrow_mut().remove(&id) {
                                    handle.window.destroy();
                                }
                                refresh_list(&parent, &app, &db, &notes, &list);
                            }
                            dialog.close();
                        },
                    );
                }

                dialog.present();
            });
        }
        row.add_suffix(&delete_button);

        {
            let app = app.clone();
            let db = db.clone();
            let notes = notes.clone();
            let id = note.id;
            row.connect_activated(move |_| open_note_window(&app, &db, &notes, id, true));
        }

        list.append(&row);
    }
}

fn preview_of(note: &Note) -> String {
    let line = note
        .body_md
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '>', ' ']))
        .find(|line| !line.is_empty() && !line.starts_with("![") && !line.starts_with("```"))
        .unwrap_or("");

    if line.is_empty() {
        "(vuota)".to_string()
    } else {
        let snippet: String = line.chars().take(50).collect();
        snippet
    }
}

// ---------------------------------------------------------------------------
// Reminder daemon: checks every 30 seconds whether a note has a daily
// reminder matching HH:MM today and hasn't fired yet → desktop notification.
// ---------------------------------------------------------------------------

fn start_reminder_daemon(db: &Arc<Mutex<Database>>) {
    let db = db.clone();
    glib::timeout_add_seconds_local(30, move || {
        let now = Local::now();
        let hhmm = now.format("%H:%M").to_string();
        let today = now.format("%Y-%m-%d").to_string();

        let due: Vec<Note> = {
            let Ok(conn) = db.lock() else {
                return glib::ControlFlow::Continue;
            };
            conn.all_notes()
                .unwrap_or_default()
                .into_iter()
                .filter(|note| {
                    note.daily_reminder.as_deref() == Some(hhmm.as_str())
                        && note.last_reminder_date.as_deref() != Some(today.as_str())
                })
                .collect()
        };

        for mut note in due {
            let title = if note.title.trim().is_empty() {
                "StickNotes".to_string()
            } else {
                note.title.clone()
            };
            gnome::notify(
                &format!("reminder-{}", note.id),
                &title,
                &format!("Promemoria: {}", preview_of(&note)),
            );

            note.last_reminder_date = Some(today.clone());
            if let Ok(conn) = db.lock() {
                let _ = conn.update_note(&note);
            }
        }

        glib::ControlFlow::Continue
    });
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

fn load_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

const CSS: &str = r#"
window.sticknote {
    background-color: transparent;
}

.sticknote-card {
    /* The header is the client-side titlebar: round the bottom corners only. */
    border-radius: 0 0 12px 12px;
    border: 1px solid rgba(0, 0, 0, 0.18);
    border-top: none;
}

/* The note card is always light, so its text must always be dark, even with
   a dark GNOME theme.  The color is applied only to leaf nodes / specific
   classes: setting it on a container would be inherited by the popovers
   (which are parented to the header buttons) and make them unreadable. */
.note-title,
.note-tool-button label {
    color: #1f1f1f;
}

.note-header-button image {
    color: #1f1f1f;
}

.note-header-button.destructive-action image {
    color: #c01c28;
}

/* Editor and live preview: same transparent background as the note card,
   with dark text (the dark theme would otherwise paint them black). */
.note-editor,
.note-preview-scroll {
    background: transparent;
    background-color: transparent;
}

.note-editor-view,
.note-editor-view text,
.note-preview-view,
.note-preview-view text {
    background: transparent;
    background-color: transparent;
    color: #1f1f1f;
}

.note-editor-view {
    caret-color: #1f1f1f;
}

.note-editor-view selection,
.note-preview-view selection {
    background-color: rgba(0, 0, 0, 0.18);
    color: #1f1f1f;
}

.note-toolbar {
    background-color: rgba(0, 0, 0, 0.04);
    border-bottom: 1px solid rgba(0, 0, 0, 0.10);
}

.note-tool-button {
    padding: 1px 5px;
    min-height: 20px;
    min-width: 20px;
    font-size: 90%;
}

.note-tool-bold {
    font-weight: 700;
}

.note-tool-italic {
    font-style: italic;
}

.note-header {
    border-radius: 12px 12px 0 0;
    background-image: linear-gradient(to bottom, rgba(255,255,255,0.55), rgba(255,255,255,0.05));
    border: 1px solid rgba(0, 0, 0, 0.18);
    border-bottom: 1px solid rgba(0, 0, 0, 0.10);
    padding: 2px 4px;
    min-height: 0;
}

.note-title {
    font-weight: 700;
}

.note-preview-view {
    font-size: 0.95em;
}

.resize-grip {
    background: transparent;
}
"#;
