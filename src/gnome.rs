// GNOME integration helpers:
//
//  * installs the .desktop entry and the application icon inside the user's
//    data dir, so StickNotes shows up (with its icon) in the dash, in the
//    application grid, in the top bar and in notifications;
//  * applies the per-note window hints (always on top / visible on every
//    workspace).
//
// GTK4 does not expose `keep_above` / `sticky` anymore: those are compositor
// features.  On X11 (or under XWayland) we can still request them through
// EWMH, which is what `apply_window_hints` does.  On native Wayland the
// compositor deliberately does not let clients control stacking or
// workspaces, so the request is simply a no-op there.

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

pub const APP_ID: &str = "org.sticknotes.app";

// ---------------------------------------------------------------------------
// Desktop entry + icon
// ---------------------------------------------------------------------------

fn exe_for_desktop_file() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "sticknotes".to_string());
    if exe.contains(' ') || exe.contains('"') {
        format!("\"{}\"", exe.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        exe
    }
}

/// Install (or refresh) the .desktop entry and the SVG icon.
pub fn install_desktop_entry() {
    let Some(data_dir) = dirs::data_dir() else {
        return;
    };

    let apps_dir = data_dir.join("applications");
    let icon_dir = data_dir.join("icons/hicolor/scalable/apps");
    let themes_dir = data_dir.join("icons/hicolor");

    let _ = fs::create_dir_all(&apps_dir);
    let _ = fs::create_dir_all(&icon_dir);

    let _ = fs::write(icon_dir.join(format!("{APP_ID}.svg")), ICON_SVG);

    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=StickNotes\n\
         GenericName=Sticky notes\n\
         Comment=Post-it markdown per il desktop GNOME\n\
         Exec={} %U\n\
         Terminal=false\n\
         Icon={APP_ID}\n\
         StartupWMClass={APP_ID}\n\
         StartupNotify=true\n\
         Categories=Utility;\n\
         Keywords=note;sticky;postit;markdown;reminder;\n\
         X-GNOME-UsesNotifications=true\n",
        exe_for_desktop_file()
    );
    let _ = fs::write(apps_dir.join(format!("{APP_ID}.desktop")), desktop);

    // Refresh the caches, best effort: the commands may not exist.
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&apps_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let _ = std::process::Command::new("gtk-update-icon-cache")
        .args(["-f", "-t"])
        .arg(&themes_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn icons_base_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("icons")
}

/// Make sure the icon we just installed is visible to the running process too.
pub fn register_icon() {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_search_path(icons_base_dir());
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

// ---------------------------------------------------------------------------
// Window hints
// ---------------------------------------------------------------------------

/// True when the current GDK backend is X11 (including XWayland).
pub fn is_x11() -> bool {
    gtk::gdk::Display::default()
        .map(|display| display.type_().name() == "GdkX11Display")
        .unwrap_or(false)
}

/// Apply "always on top" (`pinned`) and "visible on every workspace"
/// (`all_workspaces`) to a note window.
///
/// On X11 the EWMH state is applied as soon as the window has been mapped.
/// The window title is used to identify our own toplevel among the windows
/// of this process.
pub fn apply_window_hints(window: &gtk::Window, pinned: bool, all_workspaces: bool) {
    if !is_x11() {
        // Native Wayland: the compositor owns stacking/workspaces.  At least
        // make sure a pinned note is raised when it is created/shown.
        if pinned {
            window.present();
        }
        return;
    }

    let title = window.title().map(|t| t.to_string()).unwrap_or_default();
    let attempts = Rc::new(Cell::new(0u32));

    glib::timeout_add_local(Duration::from_millis(150), move || {
        attempts.set(attempts.get() + 1);
        let found = x11_apply_state(&title, pinned, all_workspaces);
        if found || attempts.get() >= 8 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

#[cfg(target_os = "linux")]
fn x11_apply_state(title: &str, pinned: bool, all_workspaces: bool) -> bool {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return false;
    };
    let root = conn.setup().roots[screen_num].root;
    let pid = std::process::id();

    let atom = |name: &str| -> Option<u32> {
        conn.intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()
            .map(|reply| reply.atom)
    };

    let (Some(client_list), Some(wm_state), Some(wm_pid), Some(state_above), Some(state_sticky)) = (
        atom("_NET_CLIENT_LIST"),
        atom("_NET_WM_STATE"),
        atom("_NET_WM_PID"),
        atom("_NET_WM_STATE_ABOVE"),
        atom("_NET_WM_STATE_STICKY"),
    ) else {
        return false;
    };

    let Ok(cookie) = conn.get_property(false, root, client_list, AtomEnum::WINDOW, 0, 4096) else {
        return false;
    };
    let Ok(reply) = cookie.reply() else {
        return false;
    };
    let Some(windows) = reply.value32() else {
        return false;
    };

    let mut found = false;
    for window in windows {
        let belongs_to_us = conn
            .get_property(false, window, wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| reply.value32().and_then(|mut values| values.next()))
            .map(|value| value == pid)
            .unwrap_or(false);
        if !belongs_to_us {
            continue;
        }

        let name = conn
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
            .unwrap_or_default();
        if name != title {
            continue;
        }

        found = true;
        send_wm_state(&conn, root, window, wm_state, state_above, pinned);
        send_wm_state(&conn, root, window, wm_state, state_sticky, all_workspaces);
    }

    let _ = conn.flush();
    found
}

#[cfg(not(target_os = "linux"))]
fn x11_apply_state(_title: &str, _pinned: bool, _all_workspaces: bool) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn send_wm_state<C: x11rb::connection::Connection>(
    conn: &C,
    root: u32,
    window: u32,
    wm_state_atom: u32,
    state_atom: u32,
    enable: bool,
) {
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};

    // _NET_WM_STATE_ADD = 1, _NET_WM_STATE_REMOVE = 0
    let action: u32 = u32::from(enable);
    let event = ClientMessageEvent::new(32, window, wm_state_atom, [action, state_atom, 0, 1, 0]);
    let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
    let _ = conn.send_event(false, root, mask, event);
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Sound name from the freedesktop sound theme (the GNOME alarm sound).
const ALARM_SOUND: &str = "alarm-clock-elapsed";

/// Show a GNOME Shell notification and play a sound.
///
/// `id` identifies the notification: sending another one with the same id
/// replaces it (GNotification semantics).
pub fn notify(id: &str, title: &str, body: &str) {
    let sound_started = play_alarm_sound();

    // Prefer GNotification: it is the native GNOME API, it attaches the
    // application's desktop entry to the notification (so GNOME Shell can
    // show it as a banner with the correct icon and per-app settings).
    // A unique id makes every notification a new one: reusing an id makes
    // GLib treat it as an update and GNOME may not show a new banner.
    let unique_id = format!(
        "{id}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0)
    );

    if let Some(app) = gio::Application::default() {
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        notification.set_icon(&gio::ThemedIcon::new(APP_ID));
        // High priority keeps the reminder prominent.
        notification.set_priority(gio::NotificationPriority::High);
        app.send_notification(Some(&unique_id), &notification);
        return;
    }

    // Fallback: raw org.freedesktop.Notifications call.
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("StickNotes")
        .icon(APP_ID)
        .summary(title)
        .body(body)
        .hint(notify_rust::Hint::DesktopEntry(APP_ID.to_string()))
        .timeout(notify_rust::Timeout::Milliseconds(12_000));

    // If we could not play the sound ourselves, ask the notification daemon
    // to do it (GNOME Shell honours the sound-name hint).
    if !sound_started {
        notification.sound_name(ALARM_SOUND);
    }

    if let Err(error) = notification.show() {
        eprintln!("StickNotes: impossibile inviare la notifica: {error}");
    }
}

/// Play the alarm through the freedesktop sound theme.  Returns true when a
/// player was started, so the notification does not play a second sound.
fn play_alarm_sound() -> bool {
    if std::process::Command::new("canberra-gtk-play")
        .args(["-i", ALARM_SOUND])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
    {
        return true;
    }

    let path = format!("/usr/share/sounds/freedesktop/stereo/{ALARM_SOUND}.oga");
    if std::path::Path::new(&path).exists() {
        for player in ["paplay", "pw-play"] {
            if std::process::Command::new(player)
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return true;
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Icon
// ---------------------------------------------------------------------------

pub const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#fff3b0"/>
      <stop offset="1" stop-color="#ffd35c"/>
    </linearGradient>
  </defs>
  <rect x="14" y="10" width="100" height="100" rx="12" fill="url(#g)" stroke="#c9a227" stroke-width="3"/>
  <path d="M14 96 h72 a22 22 0 0 0 28 -28 v28 z" fill="#f6de8c"/>
  <line x1="30" y1="36" x2="98" y2="36" stroke="#a88b3d" stroke-width="5" stroke-linecap="round"/>
  <line x1="30" y1="54" x2="98" y2="54" stroke="#a88b3d" stroke-width="5" stroke-linecap="round"/>
  <line x1="30" y1="72" x2="74" y2="72" stroke="#a88b3d" stroke-width="5" stroke-linecap="round"/>
</svg>"##;

// ---------------------------------------------------------------------------
// Autostart at login (XDG autostart)
// ---------------------------------------------------------------------------

fn autostart_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("autostart").join(format!("{APP_ID}.desktop")))
}

pub fn autostart_enabled() -> bool {
    autostart_path().map(|path| path.exists()).unwrap_or(false)
}

/// Enable/disable starting StickNotes when the user logs in.
pub fn set_autostart(enabled: bool) {
    let Some(path) = autostart_path() else {
        return;
    };

    if enabled {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=StickNotes\n\
             Comment=Post-it markdown per il desktop GNOME\n\
             Exec={}\n\
             Terminal=false\n\
             Icon={APP_ID}\n\
             X-GNOME-Autostart-enabled=true\n\
             X-GNOME-Autostart-Delay=4\n",
            exe_for_desktop_file()
        );
        let _ = fs::write(path, content);
    } else {
        let _ = fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// GNOME Shell extension: persistent icon in the top bar
// ---------------------------------------------------------------------------

pub const SHELL_UUID: &str = "sticknotes@org.sticknotes.app";

fn shell_extension_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("gnome-shell/extensions").join(SHELL_UUID))
}

/// True when the shell extension is listed as enabled by GNOME Shell.
pub fn shell_extension_enabled() -> bool {
    std::process::Command::new("gnome-extensions")
        .args(["list", "--enabled"])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == SHELL_UUID)
        })
        .unwrap_or(false)
}

/// Install (or refresh) the GNOME Shell extension and try to enable it.
///
/// The extension adds a permanent StickNotes icon to the top bar with a small
/// menu (new note, show all, quit).  It talks to the running application over
/// D-Bus and launches it if needed.
pub fn install_shell_extension() {
    let Some(dir) = shell_extension_dir() else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let icon_path = dir.join("sticknotes.svg");
    let _ = fs::write(&icon_path, ICON_SVG);

    let _ = fs::write(
        dir.join("metadata.json"),
        format!(
            "{{\n\
             \"uuid\": \"{SHELL_UUID}\",\n\
             \"name\": \"StickNotes\",\n\
             \"description\": \"Icona persistente e menu rapido per StickNotes nella barra superiore.\",\n\
             \"shell-version\": [\"45\", \"46\", \"47\", \"48\", \"49\", \"50\", \"51\", \"52\"],\n\
             \"version\": 1\n\
             }}\n"
        ),
    );

    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "sticknotes".to_string());
    let icon = icon_path.to_string_lossy().into_owned();

    let script = EXTENSION_JS
        .replace("__EXEC__", &js_escape(&executable))
        .replace("__ICON__", &js_escape(&icon));
    let _ = fs::write(dir.join("extension.js"), script);

    if !shell_extension_enabled() {
        // Best effort: on some GNOME versions a newly installed extension is
        // only picked up after a new login session.
        let _ = std::process::Command::new("gnome-extensions")
            .args(["enable", SHELL_UUID])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn js_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

const EXTENSION_JS: &str = r##"// StickNotes top-bar indicator (GNOME Shell 45+)
import St from 'gi://St';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

const APP_ID = 'org.sticknotes.app';
const OBJECT_PATH = '/org/sticknotes/app';
const EXEC = "__EXEC__";
const ICON = "__ICON__";

export default class StickNotesExtension extends Extension {
    enable() {
        this._indicator = new PanelMenu.Button(0.0, 'StickNotes', false);
        this._indicator.add_child(new St.Icon({
            gicon: new Gio.FileIcon({ file: Gio.File.new_for_path(ICON) }),
            style_class: 'system-status-icon',
        }));

        const menu = this._indicator.menu;
        menu.addMenuItem(this._item('Nuova nota', () => this._invoke('new-note')));
        menu.addMenuItem(this._item('Mostra tutte le note', () => this._activate()));
        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        menu.addMenuItem(this._item('Esci da StickNotes', () => this._invoke('quit')));

        Main.panel.addToStatusArea('sticknotes-indicator', this._indicator, 0, 'right');
    }

    disable() {
        if (this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
    }

    _item(label, callback) {
        const item = new PopupMenu.PopupMenuItem(label);
        item.connect('activate', callback);
        return item;
    }

    _activate() {
        this._call('org.freedesktop.Application', 'Activate',
            new GLib.Variant('(a{sv})', [{}]));
    }

    _invoke(action) {
        this._call('org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', [action, [], {}]));
    }

    _call(iface, method, params, allowSpawn = true) {
        this._lastCall = { iface, method, params };
        try {
            Gio.DBus.session.call(APP_ID, OBJECT_PATH, iface, method, params,
                null, Gio.DBusCallFlags.NONE, -1, null,
                (connection, result) => {
                    try {
                        connection.call_finish(result);
                    } catch (e) {
                        if (allowSpawn)
                            this._spawn();
                    }
                });
        } catch (e) {
            if (allowSpawn)
                this._spawn();
        }
    }

    _spawn() {
        try {
            GLib.spawn_command_line_async(GLib.shell_quote(EXEC));
        } catch (e) {
            console.error(`StickNotes: impossibile avviare l'applicazione: ${e}`);
            return;
        }
        // The app needs a moment to register on the bus; retry the action once.
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1800, () => {
            if (this._lastCall) {
                const { iface, method, params } = this._lastCall;
                this._call(iface, method, params, false);
            }
            return GLib.SOURCE_REMOVE;
        });
    }
}
"##;
