// The borderless note window.
//
// It is a normal GtkWindow with client-side decorations turned off, so the
// user sees only our own header bar with:
//
//   • save/edit → saves the note and collapses the editor (save icon);
//                 a pencil icon brings the editor back
//   • properties → popover with:
//   a) visible on all workspaces   b) always on top
//   c) creation date (read only)   d) daily reminder (HH:MM)
//   e) open automatically at startup
//
//   • close     → deletes the note permanently (with a confirmation popover)
//
// The header is used as a client-side titlebar, so GTK provides the native
// edge resize handling (the note has no window buttons but remains resizable)
// while dragging the title starts a compositor-driven window move.
//
// The body is a Markdown source editor (GtkTextView) with a live preview.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::glib;
use gtk::prelude::*;
use gtk::{gdk, gio, pango, Align, Application, Orientation, Picture, TextView, Window};

use crate::db::{Database, Note};
use crate::gnome;
use crate::md;

pub const CSS_CLASS: &str = "sticknote";

#[derive(Clone)]
pub struct NoteHandle {
    pub window: Window,
}

pub fn build_note_window<A: IsA<Application>>(
    app: &A,
    db: Arc<Mutex<Database>>,
    note: &Note,
    on_closed: Rc<dyn Fn(i64)>,
    start_in_view: bool,
) -> NoteHandle {
    let note_id = note.id;
    let display_title = md::derived_title(&note.body_md);

    // ---------- window ----------
    let win = Window::new();
    win.set_application(Some(app));
    win.set_resizable(true);
    win.set_default_size(note.w.max(260), note.h.max(200));
    win.set_title(Some(display_title.as_str()));
    win.set_icon_name(Some(gnome::APP_ID));
    win.add_css_class(CSS_CLASS);

    // ---------- card (rounded, colored) ----------
    let card = gtk::Box::new(Orientation::Vertical, 0);
    card.add_css_class("sticknote-card");

    let header_class = if let Some(display) = gdk::Display::default() {
        let color = sanitize_color(&note.color);
        let color_class = format!("sticknote-color-{note_id}");
        let header_class = format!("sticknote-header-{note_id}");
        // The body is opaque; the titlebar is the same color but translucent
        // so the note keeps its colored "glass" header without hiding the
        // title or the icons (the text stays dark and readable).
        let translucent = rgba_with_alpha(&color, 0.75);

        let provider = gtk::CssProvider::new();
        provider.load_from_data(&format!(
            ".{color_class} {{ background-color: {color}; }}\n\
             .{header_class} {{ background-color: {translucent}; }}"
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        card.add_css_class(&color_class);
        Some(header_class)
    } else {
        None
    };

    // ---------- header (used as the client-side titlebar) ----------
    // A custom titlebar gives us client-side decorations, so GTK provides
    // the native edge resize handling while no window buttons are shown:
    // the note remains borderless-looking but is resizable.
    let header = gtk::CenterBox::new();
    header.add_css_class("note-header");
    if let Some(class) = &header_class {
        // The header is the client-side titlebar: translucent note color.
        header.add_css_class(class);
    }

    let title_lbl = gtk::Label::new(Some(display_title.as_str()));
    title_lbl.add_css_class("note-title");
    title_lbl.set_ellipsize(pango::EllipsizeMode::End);
    title_lbl.set_hexpand(true);
    title_lbl.set_xalign(0.0);
    title_lbl.set_margin_start(6);
    title_lbl.set_margin_end(6);
    title_lbl.set_tooltip_text(Some("Trascina per spostare la nota"));

    let btn_save = icon_button("document-save-symbolic", "Salva e chiudi l'editor");
    let btn_props = icon_button("emblem-system-symbolic", "Proprietà della nota");
    let btn_close = icon_button("window-close-symbolic", "Elimina la nota");
    btn_close.add_css_class("destructive-action");

    let left_box = gtk::Box::new(Orientation::Horizontal, 0);
    left_box.append(&title_lbl);

    let right_box = gtk::Box::new(Orientation::Horizontal, 0);
    right_box.append(&btn_save);
    right_box.append(&btn_props);
    right_box.append(&btn_close);

    header.set_start_widget(Some(&left_box));
    header.set_end_widget(Some(&right_box));
    win.set_titlebar(Some(&header));

    // Move the window by dragging the title.
    let drag = gtk::GestureDrag::new();
    drag.connect_drag_begin({
        let win = win.clone();
        move |gesture, x, y| {
            let (Some(device), Some(native)) = (gesture.device(), win.native()) else {
                return;
            };
            if let Ok(toplevel) = native.surface().downcast::<gdk::Toplevel>() {
                toplevel.begin_move(&device, 1, x, y, gesture.current_event_time());
            }
        }
    });
    left_box.add_controller(drag);

    // ---------- body: editor + preview ----------
    let editor = TextView::new();
    editor.set_wrap_mode(gtk::WrapMode::WordChar);
    editor.set_monospace(true);
    editor.set_left_margin(10);
    editor.set_right_margin(10);
    editor.set_top_margin(8);
    editor.set_bottom_margin(8);
    editor.buffer().set_text(&note.body_md);
    editor.add_css_class("note-editor-view");

    let ed_scroll = gtk::ScrolledWindow::new();
    ed_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    ed_scroll.set_child(Some(&editor));
    ed_scroll.add_css_class("note-editor");

    let preview = TextView::new();
    preview.set_editable(false);
    preview.set_cursor_visible(false);
    preview.set_wrap_mode(gtk::WrapMode::WordChar);
    preview.set_left_margin(12);
    preview.set_right_margin(12);
    preview.set_top_margin(8);
    preview.set_bottom_margin(12);
    preview.add_css_class("note-preview");
    preview.add_css_class("note-preview-view");

    let pv_scroll = gtk::ScrolledWindow::new();
    pv_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    pv_scroll.set_child(Some(&preview));
    pv_scroll.add_css_class("note-preview-scroll");

    let paned = gtk::Paned::new(Orientation::Vertical);
    paned.set_wide_handle(true);
    paned.set_resize_start_child(true);
    paned.set_shrink_start_child(false);
    paned.set_resize_end_child(true);
    paned.set_shrink_end_child(false);
    paned.set_start_child(Some(&ed_scroll));
    paned.set_end_child(Some(&pv_scroll));
    paned.set_position((note.h * 55) / 100);
    paned.set_vexpand(true);

    let toolbar = build_format_toolbar(&editor, &win, &db, note_id);
    card.append(&toolbar);
    card.append(&paned);

    // ---------- live preview ----------
    let textures: Rc<RefCell<HashMap<i64, gdk::Texture>>> = Rc::new(RefCell::new(HashMap::new()));

    {
        let preview = preview.clone();
        let textures = textures.clone();
        let title_lbl = title_lbl.clone();
        let window = win.clone();
        let db = db.clone();
        let scroll = pv_scroll.vadjustment();

        editor.buffer().connect_changed(move |buffer| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            let title = md::derived_title(&text);

            if let Ok(conn) = db.lock() {
                let _ = conn.set_body(note_id, &text);
                let _ = conn.set_title(note_id, &title);
            }

            if title_lbl.text() != title {
                title_lbl.set_text(&title);
                window.set_title(Some(title.as_str()));
            }

            let position = scroll.value();
            render_preview(&preview, &textures, &text);
            let adjustment = scroll.clone();
            glib::idle_add_local_once(move || adjustment.set_value(position));
        });
    }

    render_preview(&preview, &textures, &note.body_md);

    // ---------- Ctrl+V with an image on the clipboard ----------
    {
        let db = db.clone();
        let editor_c = editor.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            if ctrl && (keyval == gdk::Key::b || keyval == gdk::Key::B) {
                wrap_selection(&editor_c, "**", "**", "grassetto");
                return glib::Propagation::Stop;
            }
            if ctrl && (keyval == gdk::Key::i || keyval == gdk::Key::I) {
                wrap_selection(&editor_c, "*", "*", "corsivo");
                return glib::Propagation::Stop;
            }
            let is_v = keyval == gdk::Key::v || keyval == gdk::Key::V;
            if ctrl && is_v && paste_image(&db, note_id, &editor_c) {
                glib::Propagation::Stop
            } else {
                // Normal text paste keeps working through the TextView.
                glib::Propagation::Proceed
            }
        });
        editor.add_controller(keys);
    }

    // ---------- save / edit toggle ----------
    let view_mode = Rc::new(Cell::new(start_in_view));
    btn_save.connect_clicked({
        let view_mode = view_mode.clone();
        let editor_scroll = ed_scroll.clone();
        let toolbar = toolbar.clone();
        let editor = editor.clone();
        let db = db.clone();
        let button = btn_save.clone();
        move |_| {
            if view_mode.get() {
                // Back to editing.
                view_mode.set(false);
                editor_scroll.set_visible(true);
                toolbar.set_visible(true);
                button.set_icon_name("document-save-symbolic");
                button.set_tooltip_text(Some("Salva e chiudi l'editor"));
                editor.grab_focus();
            } else {
                // Save explicitly, then collapse the editor and show only
                // the rendered Markdown.
                let buffer = editor.buffer();
                let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                let title = md::derived_title(&text);
                if let Ok(conn) = db.lock() {
                    let _ = conn.set_body(note_id, &text);
                    let _ = conn.set_title(note_id, &title);
                }
                view_mode.set(true);
                editor_scroll.set_visible(false);
                toolbar.set_visible(false);
                button.set_icon_name("document-edit-symbolic");
                button.set_tooltip_text(Some("Modifica la nota"));
            }
        }
    });

    // Existing notes open directly in rendered Markdown view; new notes start
    // in edit mode.  The save/edit button reflects the initial state.
    if start_in_view {
        ed_scroll.set_visible(false);
        toolbar.set_visible(false);
        btn_save.set_icon_name("document-edit-symbolic");
        btn_save.set_tooltip_text(Some("Modifica la nota"));
    }
    // Confirmation popover for the destructive close button.
    let confirm = gtk::Popover::new();
    confirm.set_parent(&btn_close);
    {
        let content = gtk::Box::new(Orientation::Vertical, 8);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        let label = gtk::Label::new(Some("Eliminare definitivamente questa nota?"));
        label.set_wrap(true);
        label.set_max_width_chars(30);
        let row = gtk::Box::new(Orientation::Horizontal, 6);
        row.set_halign(Align::End);
        let cancel = gtk::Button::with_label("Annulla");
        let delete = gtk::Button::with_label("Elimina");
        delete.add_css_class("destructive-action");
        row.append(&cancel);
        row.append(&delete);
        content.append(&label);
        content.append(&row);
        confirm.set_child(Some(&content));

        cancel.connect_clicked({
            let confirm = confirm.clone();
            move |_| confirm.popdown()
        });
        delete.connect_clicked({
            let on_closed = on_closed.clone();
            move |_| on_closed(note_id)
        });
    }
    btn_close.connect_clicked({
        let confirm = confirm.clone();
        move |_| confirm.popup()
    });

    // ---------- properties popover ----------
    btn_props.connect_clicked({
        let win = win.clone();
        let db = db.clone();
        let button = btn_props.clone();
        move |_| {
            let Some(current) = db
                .lock()
                .ok()
                .and_then(|conn| conn.get_note(note_id).ok().flatten())
            else {
                return;
            };

            let popover = gtk::Popover::new();
            popover.set_parent(&button);
            popover.connect_closed(|popover| {
                let popover = popover.clone();
                glib::idle_add_local_once(move || {
                    if popover.parent().is_some() {
                        popover.unparent();
                    }
                });
            });

            let content = gtk::Box::new(Orientation::Vertical, 10);
            content.set_margin_start(14);
            content.set_margin_end(14);
            content.set_margin_top(12);
            content.set_margin_bottom(12);
            content.set_width_request(280);

            let created = gtk::Label::new(Some(&format!("Creata il {}", current.created_at)));
            created.set_xalign(0.0);
            created.add_css_class("dim-label");
            content.append(&created);

            let workspaces = gtk::Switch::new();
            workspaces.set_active(current.all_workspaces);
            workspaces.set_valign(Align::Center);
            content.append(&labeled_row("Tutti i workspace", &workspaces));

            let pinned = gtk::Switch::new();
            pinned.set_active(current.pinned);
            pinned.set_valign(Align::Center);
            content.append(&labeled_row("Sempre in primo piano", &pinned));

            let auto_open = gtk::Switch::new();
            auto_open.set_active(current.auto_open);
            auto_open.set_valign(Align::Center);
            content.append(&labeled_row("Apri all'avvio", &auto_open));

            let reminder_check = gtk::CheckButton::with_label("Promemoria giornaliero");
            reminder_check.set_active(current.daily_reminder.is_some());
            let reminder_time = gtk::Entry::new();
            reminder_time.set_width_chars(6);
            reminder_time.set_max_width_chars(6);
            reminder_time.set_placeholder_text(Some("09:00"));
            reminder_time.set_text(current.daily_reminder.as_deref().unwrap_or("09:00"));
            reminder_time.set_sensitive(current.daily_reminder.is_some());
            reminder_check.connect_toggled({
                let reminder_time = reminder_time.clone();
                move |check| reminder_time.set_sensitive(check.is_active())
            });
            let reminder_row = gtk::Box::new(Orientation::Horizontal, 8);
            reminder_row.append(&reminder_check);
            reminder_row.append(&reminder_time);
            content.append(&reminder_row);

            let test_notification = gtk::Button::with_label("Prova notifica");
            test_notification.add_css_class("flat");
            test_notification.set_halign(Align::Start);
            test_notification.set_tooltip_text(Some("Invia una notifica di prova con audio"));
            {
                let title = if current.title.trim().is_empty() {
                    "StickNotes".to_string()
                } else {
                    current.title.clone()
                };
                test_notification.connect_clicked(move |button| {
                    gnome::notify("test", &title, "Notifica di prova: il promemoria funziona.");
                    // Immediate in-app feedback: the desktop banner is managed
                    // by the shell, this tells the user the app sent it.
                    button.set_label("Notifica inviata ✓");
                    let button = button.clone();
                    glib::timeout_add_seconds_local_once(2, move || {
                        button.set_label("Prova notifica");
                    });
                });
            }
            content.append(&test_notification);

            if !gnome::is_x11() {
                let hint = gtk::Label::new(Some(
                    "Su Wayland il compositor non permette alle app di forzare \
                     «sempre in primo piano» o «tutti i workspace»: avvia StickNotes \
                     con GDK_BACKEND=x11 per abilitarli.",
                ));
                hint.set_wrap(true);
                hint.set_max_width_chars(34);
                hint.set_xalign(0.0);
                hint.add_css_class("dim-label");
                hint.add_css_class("caption");
                content.append(&hint);
            }

            let apply = gtk::Button::with_label("Applica");
            apply.add_css_class("suggested-action");
            apply.set_halign(Align::End);
            apply.connect_clicked({
                let db = db.clone();
                let win = win.clone();
                let popover = popover.clone();
                let workspaces = workspaces.clone();
                let pinned = pinned.clone();
                let auto_open = auto_open.clone();
                let reminder_check = reminder_check.clone();
                let reminder_time = reminder_time.clone();
                move |_| {
                    let pinned_value = pinned.is_active();
                    let workspaces_value = workspaces.is_active();
                    let auto_open_value = auto_open.is_active();
                    let reminder = if reminder_check.is_active() {
                        Some(sanitize_time(reminder_time.text().as_str()))
                    } else {
                        None
                    };
                    if let Ok(conn) = db.lock() {
                        let _ = conn.set_properties(
                            note_id,
                            pinned_value,
                            workspaces_value,
                            reminder.as_deref(),
                            auto_open_value,
                        );
                    }
                    gnome::apply_window_hints(&win, pinned_value, workspaces_value);
                    popover.popdown();
                }
            });
            content.append(&apply);

            popover.set_child(Some(&content));
            popover.popup();
        }
    });

    // ---------- persist size / hide on close ----------
    win.connect_close_request({
        let db = db.clone();
        let win = win.clone();
        move |_| {
            let (width, height) = (win.width(), win.height());
            if width > 0 && height > 0 {
                if let Ok(conn) = db.lock() {
                    let _ = conn.set_geometry(note_id, width, height);
                }
            }
            // Alt+F4 (or the WM close) only hides the note: deleting it must
            // be explicit through the ✕ button or the manager window.
            win.hide();
            glib::Propagation::Stop
        }
    });

    win.set_child(Some(&card));
    win.show();

    gnome::apply_window_hints(&win, note.pinned, note.all_workspaces);

    NoteHandle { window: win }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Markdown formatting toolbar
// ---------------------------------------------------------------------------

fn build_format_toolbar(
    editor: &TextView,
    window: &Window,
    db: &Arc<Mutex<Database>>,
    note_id: i64,
) -> gtk::Widget {
    let bar = gtk::Box::new(Orientation::Horizontal, 1);
    bar.add_css_class("note-toolbar");
    bar.set_margin_start(4);
    bar.set_margin_end(4);
    bar.set_margin_top(2);
    bar.set_margin_bottom(2);

    let tool = |label: &str, tooltip: &str, classes: &[&str]| -> gtk::Button {
        let button = gtk::Button::with_label(label);
        button.add_css_class("flat");
        button.add_css_class("note-tool-button");
        for class in classes {
            button.add_css_class(class);
        }
        button.set_tooltip_text(Some(tooltip));
        bar.append(&button);
        button
    };

    tool("B", "Grassetto (Ctrl+B)", &["note-tool-bold"]).connect_clicked({
        let editor = editor.clone();
        move |_| wrap_selection(&editor, "**", "**", "grassetto")
    });

    tool("I", "Corsivo (Ctrl+I)", &["note-tool-italic"]).connect_clicked({
        let editor = editor.clone();
        move |_| wrap_selection(&editor, "*", "*", "corsivo")
    });

    tool("S", "Barrato", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| wrap_selection(&editor, "~~", "~~", "barrato")
    });

    tool("</>", "Codice inline", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| wrap_selection(&editor, "`", "`", "codice")
    });

    tool("H1", "Titolo livello 1", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| toggle_line_prefix(&editor, "# ")
    });

    tool("H2", "Titolo livello 2", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| toggle_line_prefix(&editor, "## ")
    });

    tool("•", "Elenco puntato", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| toggle_line_prefix(&editor, "- ")
    });

    tool("1.", "Elenco numerato", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| toggle_line_prefix(&editor, "1. ")
    });

    tool("❝", "Citazione", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| toggle_line_prefix(&editor, "> ")
    });

    tool("☑", "Casella di controllo", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| toggle_line_prefix(&editor, "- [ ] ")
    });

    tool("🔗", "Inserisci un link", &[]).connect_clicked({
        let editor = editor.clone();
        move |_| insert_link(&editor)
    });

    tool("🖼", "Inserisci un'immagine dal file system", &[]).connect_clicked({
        let editor = editor.clone();
        let window = window.clone();
        let db = db.clone();
        move |_| choose_image(&window, &db, note_id, &editor)
    });

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(&bar));
    scroller.add_css_class("note-toolbar-scroll");
    scroller.upcast::<gtk::Widget>()
}

fn selection_offsets(buffer: &gtk::TextBuffer) -> (i32, i32) {
    match buffer.selection_bounds() {
        Some((start, end)) => (start.offset(), end.offset()),
        None => {
            let offset = buffer.cursor_position();
            (offset, offset)
        }
    }
}

/// Wrap the current selection (or insert a placeholder) with `open` / `close`.
fn wrap_selection(editor: &TextView, open: &str, close: &str, placeholder: &str) {
    let buffer = editor.buffer();
    let (start, end) = selection_offsets(&buffer);
    let selected = buffer
        .text(
            &buffer.iter_at_offset(start),
            &buffer.iter_at_offset(end),
            false,
        )
        .to_string();
    let content = if selected.is_empty() {
        placeholder.to_string()
    } else {
        selected
    };

    if end > start {
        let mut start_iter = buffer.iter_at_offset(start);
        let mut end_iter = buffer.iter_at_offset(end);
        buffer.delete(&mut start_iter, &mut end_iter);
    }

    let mut iter = buffer.iter_at_offset(start);
    buffer.insert(&mut iter, &format!("{open}{content}{close}"));

    let content_start = start + open.chars().count() as i32;
    let content_end = content_start + content.chars().count() as i32;
    buffer.select_range(
        &buffer.iter_at_offset(content_start),
        &buffer.iter_at_offset(content_end),
    );
    editor.grab_focus();
}

/// Toggle a Markdown line prefix (heading, list, quote, task list) on the line
/// where the cursor is.
fn toggle_line_prefix(editor: &TextView, prefix: &str) {
    let buffer = editor.buffer();
    let cursor = buffer.cursor_position() as usize;
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string();
    let chars: Vec<char> = text.chars().collect();

    let mut line_start = cursor.min(chars.len());
    while line_start > 0 && chars[line_start - 1] != '\n' {
        line_start -= 1;
    }
    let mut line_end = cursor.min(chars.len());
    while line_end < chars.len() && chars[line_end] != '\n' {
        line_end += 1;
    }
    let line: String = chars[line_start..line_end].iter().collect();

    if line.starts_with(prefix) {
        let mut start = buffer.iter_at_offset(line_start as i32);
        let mut end = buffer.iter_at_offset((line_start + prefix.chars().count()) as i32);
        buffer.delete(&mut start, &mut end);
    } else {
        // Replace an existing heading marker (`# ` … `###### `), if any.
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && line.chars().nth(hashes) == Some(' ') {
            let mut start = buffer.iter_at_offset(line_start as i32);
            let mut end = buffer.iter_at_offset((line_start + hashes + 1) as i32);
            buffer.delete(&mut start, &mut end);
        }
        let mut iter = buffer.iter_at_offset(line_start as i32);
        buffer.insert(&mut iter, prefix);
    }
    editor.grab_focus();
}

fn insert_link(editor: &TextView) {
    let buffer = editor.buffer();
    let (start, end) = selection_offsets(&buffer);
    let selected = buffer
        .text(
            &buffer.iter_at_offset(start),
            &buffer.iter_at_offset(end),
            false,
        )
        .to_string();
    let label = if selected.is_empty() {
        "testo".to_string()
    } else {
        selected
    };

    if end > start {
        let mut start_iter = buffer.iter_at_offset(start);
        let mut end_iter = buffer.iter_at_offset(end);
        buffer.delete(&mut start_iter, &mut end_iter);
    }

    let mut iter = buffer.iter_at_offset(start);
    buffer.insert(&mut iter, &format!("[{label}](https://)"));

    // Select the URL placeholder so the user can type it immediately.
    let url_start = start + label.chars().count() as i32 + 3;
    let url_end = url_start + "https://".chars().count() as i32;
    buffer.select_range(
        &buffer.iter_at_offset(url_start),
        &buffer.iter_at_offset(url_end),
    );
    editor.grab_focus();
}

fn choose_image(window: &Window, db: &Arc<Mutex<Database>>, note_id: i64, editor: &TextView) {
    let dialog = gtk::FileChooserDialog::new(
        Some("Inserisci un'immagine"),
        Some(window),
        gtk::FileChooserAction::Open,
        &[
            ("Annulla", gtk::ResponseType::Cancel),
            ("Apri", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);

    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Immagini"));
    for mime in [
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/gif",
        "image/bmp",
        "image/svg+xml",
    ] {
        filter.add_mime_type(mime);
    }
    dialog.add_filter(&filter);

    let db = db.clone();
    let editor = editor.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let Some(path) = dialog.file().and_then(|file| file.path()) {
                insert_image_from_path(&db, note_id, &editor, &path);
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn insert_image_from_path(
    db: &Arc<Mutex<Database>>,
    note_id: i64,
    editor: &TextView,
    path: &std::path::Path,
) {
    let Ok(texture) = gdk::Texture::from_filename(path) else {
        return;
    };
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "immagine.png".to_string());
    let Some(asset_id) = store_texture(db, note_id, &texture, &filename) else {
        return;
    };
    let alt = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "immagine".to_string());
    editor
        .buffer()
        .insert_at_cursor(&format!("\n![{alt}](sticknotes://{asset_id})\n"));
    editor.grab_focus();
}

/// Store a texture as PNG in SQLite and in the on-disk cache used by the
/// preview.  Returns the asset id.
fn store_texture(
    db: &Arc<Mutex<Database>>,
    note_id: i64,
    texture: &gdk::Texture,
    filename: &str,
) -> Option<i64> {
    let bytes = texture.save_to_png_bytes();
    let png: &[u8] = bytes.as_ref();

    let asset_id = {
        let conn = db.lock().ok()?;
        conn.add_asset(note_id, filename, "image/png", png).ok()?
    };
    if asset_id <= 0 {
        return None;
    }

    let cache_dir = Database::cache_dir();
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = std::fs::write(cache_dir.join(format!("asset-{asset_id}.png")), png);
    Some(asset_id)
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("note-header-button");
    button.set_valign(Align::Center);
    button
}

fn labeled_row(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 10);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    widget.set_hexpand(false);
    widget.set_halign(Align::End);
    row.append(&text);
    row.append(widget);
    row
}

/// Render the Markdown into the preview view, replacing the images that were
/// added by the previous run.
fn render_preview(
    preview: &TextView,
    textures: &Rc<RefCell<HashMap<i64, gdk::Texture>>>,
    markdown: &str,
) {
    let buffer = preview.buffer();

    let resolve = |source: &str| -> Option<gdk::Texture> {
        if let Some(asset) = source.strip_prefix("sticknotes://") {
            let id = asset.parse::<i64>().ok()?;
            if let Some(texture) = textures.borrow().get(&id) {
                return Some(texture.clone());
            }
            let path = Database::cache_dir().join(format!("asset-{id}.png"));
            let texture = gdk::Texture::from_filename(&path).ok()?;
            textures.borrow_mut().insert(id, texture.clone());
            Some(texture)
        } else if source.starts_with('/') {
            gdk::Texture::from_filename(source).ok()
        } else {
            None
        }
    };

    // The previous child widgets are removed by the buffer itself when
    // `render` clears it: manually unparenting them first would leave the
    // text anchors pointing at already-unparented widgets and crash in
    // gtk_text_view_remove() on the next render.
    // Available width for the image: the preview minus its horizontal margins.
    // Before realization `width()` is 0, so use a sensible default.
    let available = {
        let width = preview.width();
        if width > 80 {
            (width - 24) as f64
        } else {
            300.0
        }
    };

    let mut add_image = |anchor: &gtk::TextChildAnchor, texture: &gdk::Texture, alt: &str| {
        // A GtkPicture with can-shrink but *no* width request gets a 0-width
        // allocation inside a GtkTextView child anchor (invisible!).  Give it
        // an explicit size that fits the preview and keeps the aspect ratio.
        let (width, height) = image_size(texture.width(), texture.height(), available);

        let picture = Picture::for_paintable(texture);
        picture.set_can_shrink(true);
        picture.set_halign(Align::Start);
        picture.set_size_request(width, height);
        if !alt.is_empty() {
            picture.set_tooltip_text(Some(alt));
        }
        preview.add_child_at_anchor(&picture, anchor);
    };

    md::render(&buffer, markdown, &resolve, &mut add_image);
}

/// Size of an image inside the preview: fit `width`x`height` in the available
/// width (and in a 360 px tall box), never upscaling it.  Returns a size with
/// both dimensions >= 1 so that GtkPicture always has a real allocation.
fn image_size(width: i32, height: i32, available_width: f64) -> (i32, i32) {
    let source_width = width.max(1) as f64;
    let source_height = height.max(1) as f64;
    let scale = (available_width / source_width)
        .min(360.0 / source_height)
        .min(1.0);
    (
        ((source_width * scale).round() as i32).max(1),
        ((source_height * scale).round() as i32).max(1),
    )
}

/// Convert `#rrggbb` to an `rgba()` CSS color with the given alpha.
fn rgba_with_alpha(color: &str, alpha: f64) -> String {
    let fallback = "#fff4c2";
    let color = if color.len() == 7 && color.starts_with('#') {
        color
    } else {
        fallback
    };
    let channel =
        |range: std::ops::Range<usize>| u8::from_str_radix(&color[range], 16).unwrap_or(255);
    let red = channel(1..3);
    let green = channel(3..5);
    let blue = channel(5..7);
    format!("rgba({red}, {green}, {blue}, {alpha:.2})")
}

fn sanitize_color(color: &str) -> String {
    let color = color.trim();
    if color.len() == 7
        && color.starts_with('#')
        && color[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        color.to_string()
    } else {
        "#fff4c2".to_string()
    }
}

fn sanitize_time(value: &str) -> String {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.len() == 2 {
        if let (Ok(hours), Ok(minutes)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return format!("{:02}:{:02}", hours.min(23), minutes.min(59));
        }
    }
    "09:00".to_string()
}

/// Ctrl+V inside the editor: if the clipboard holds an image, store it in
/// SQLite and insert `![immagine](sticknotes://<id>)` at the cursor.
/// Returns `true` when the paste was handled, `false` to let the TextView do
/// its normal text paste.
fn paste_image(db: &Arc<Mutex<Database>>, note_id: i64, editor: &TextView) -> bool {
    let clipboard = editor.clipboard();
    let formats = clipboard.formats();
    let has_image = formats.contain_mime_type("image/png")
        || formats.contain_mime_type("image/jpeg")
        || formats.contain_mime_type("image/webp")
        || formats.contain_mime_type("image/gif");

    if !has_image {
        return false;
    }

    let db = db.clone();
    let editor = editor.clone();
    clipboard.read_texture_async(gio::Cancellable::NONE, move |result| {
        let Ok(Some(texture)) = result else {
            return;
        };
        let Some(asset_id) = store_texture(&db, note_id, &texture, "incollata.png") else {
            return;
        };
        editor
            .buffer()
            .insert_at_cursor(&format!("\n![immagine](sticknotes://{asset_id})\n"));
    });

    true
}

/// Re-create every cached PNG from SQLite (called at startup).
pub fn rebuild_cache(db: &Arc<Mutex<Database>>) {
    let cache_dir = Database::cache_dir();
    let _ = std::fs::remove_dir_all(&cache_dir);
    let _ = std::fs::create_dir_all(&cache_dir);

    let assets: Vec<(i64, Vec<u8>)> = {
        let Ok(conn) = db.lock() else {
            return;
        };
        conn.all_asset_ids()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| {
                conn.asset_by_id(id)
                    .ok()
                    .flatten()
                    .map(|asset| (id, asset.blob))
            })
            .collect()
    };

    for (id, blob) in assets {
        let _ = std::fs::write(cache_dir.join(format!("asset-{id}.png")), blob);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_size_fits_width() {
        assert_eq!(image_size(400, 300, 336.0), (336, 252));
    }

    #[test]
    fn image_size_never_upscales() {
        assert_eq!(image_size(100, 80, 336.0), (100, 80));
    }

    #[test]
    fn image_size_fits_height() {
        assert_eq!(image_size(1080, 1920, 336.0), (203, 360));
    }

    #[test]
    fn image_size_always_positive() {
        let (w, h) = image_size(0, 0, 300.0);
        assert!(w >= 1 && h >= 1);
    }
}

#[cfg(test)]
mod color_tests {
    use super::*;

    #[test]
    fn rgba_from_hex() {
        assert_eq!(
            rgba_with_alpha("#fff4c2", 0.75),
            "rgba(255, 244, 194, 0.75)"
        );
    }

    #[test]
    fn rgba_fallback() {
        assert_eq!(rgba_with_alpha("nope", 0.5), "rgba(255, 244, 194, 0.50)");
    }
}
