# StickNotes

Post-it in Markdown per GNOME, scritti in Rust con GTK4 + libadwaita.

Ogni nota è una finestra **senza bordi di sistema**, ridimensionabile
trascinando i bordi, con editor Markdown, barra di formattazione e anteprima
dal vivo. L'header (client-side titlebar) contiene salva/modifica, proprietà
ed elimina, ma nessun pulsante di sistema. Le note sono salvate in SQLite e
ripristinate all'avvio.

## Requisiti

Dipendenze di sistema (Debian/Ubuntu):

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
                 libsqlite3-dev
```

Su Fedora:

```bash
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel sqlite-devel
```

SQLite viene compilato staticamente (`rusqlite` con feature `bundled`), quindi
`libsqlite3-dev` non è strettamente necessario.

## Compilazione e avvio

```bash
cargo build --release
./target/release/sticknotes
```

In sviluppo:

```bash
cargo run
cargo test
```

## Funzionalità

| Requisito | Implementazione |
|---|---|
| Persistenza su SQLite | `src/db.rs`, database in `~/.local/share/org.sticknotes.app/sticknotes.db` (WAL). Le immagini sono BLOB nella tabella `assets`. |
| Icona GNOME | All'avvio l'app installa `~/.local/share/applications/org.sticknotes.app.desktop` e l'icona SVG in `~/.local/share/icons/hicolor/scalable/apps/`. L'`application-id` (`org.sticknotes.app`) coincide con il nome del file `.desktop`, quindi GNOME Shell associa le finestre all'icona (dash, app grid, selettore finestre, notifiche). |
| Finestre senza bordo di sistema, ridimensionabili | `src/note.rs`: header personalizzato usato come **titlebar client-side** (`set_titlebar`), senza pulsanti di sistema: GTK gestisce nativamente ridimensionamento dai bordi e trascinamento. Restano i pulsanti applicativi salva/modifica, proprietà ed elimina. |
| Titolo semi-trasparente | Il titlebar usa il colore della nota con opacità 75% (`rgba(...)` generato dal colore della nota): resta semi-trasparente sul desktop, mentre titolo e icone restano scuri e leggibili (verificato anche su sfondo scuro). |
| Salva / modifica | Il pulsante con il disco salva la nota e chiude l'editor in alto, lasciando l'anteprima a tutta finestra; l'icona diventa una matita che riapre l'editor. |
| Testo leggibile | I colori sono impostati esplicitamente e solo sui nodi foglia (titolo, icone, editor, anteprima): la nota resta leggibile con tema GNOME chiaro o scuro, e i popover (che erediterebbero il colore da un contenitore) restano leggibili. Editor e anteprima hanno lo stesso sfondo trasparente della scheda. |
| Editor Markdown + anteprima | Editor `GtkTextView` con anteprima `GtkTextView` renderizzata con `TextTag` (`src/md.rs`). Titolo ricavato automaticamente dal primo heading. |
| Barra di formattazione | Grassetto (`Ctrl+B`), corsivo (`Ctrl+I`), barrato, codice inline, H1, H2, elenco puntato, elenco numerato, citazione, casella di controllo, link e immagine. I pulsanti applicano/tolgono i marcatori alla selezione o alla riga corrente. |
| Immagini | `Ctrl+V` incolla un'immagine dagli appunti; il pulsante 🖼 apre il file chooser e importa PNG/JPEG/WebP/GIF/BMP/SVG. L'immagine viene convertita in PNG, salvata in SQLite e mostrata nell'anteprima. |
| Proprietà per nota | Popover ⚙: visibile su tutti i workspace, sempre in primo piano, **apertura automatica all'avvio**, data di creazione, promemoria giornaliero (`HH:MM`) con **notifica GNOME Shell + suono** (`alarm-clock-elapsed`, riprodotto con `canberra-gtk-play`, fallback `paplay`/`pw-play`). |
| Prova notifica | Pulsante **«Prova notifica»** nel popover delle proprietà della nota (con conferma «Notifica inviata ✓» nell'interfaccia) e voce **«Prova notifica»** nel menu della finestra elenco. |
| Notifica nativa GNOME | Le notifiche usano **GNotification** (`org.gtk.Notifications`) con l'app-id `org.sticknotes.app`: GNOME Shell le associa al `.desktop` (icona e impostazioni per-app) e le mostra come banner. Ogni notifica ha un id univoco e priorità alta; fallback a `org.freedesktop.Notifications` con hint `desktop-entry`. |
| Avvio automatico dell'app | Menu ☰ → «Avvia StickNotes all'accesso» (crea/rimuove `~/.config/autostart/org.sticknotes.app.desktop`). |
| Icona persistente nella barra superiore | All'avvio viene installata l'estensione GNOME Shell `sticknotes@org.sticknotes.app` in `~/.local/share/gnome-shell/extensions/`: aggiunge un'icona permanente nella barra in alto con menu «Nuova nota / Mostra tutte / Esci». Se l'app non è in esecuzione, l'estensione la avvia via D-Bus. |

Note:

* `Alt+F4` / chiusura dal window manager **nasconde** la nota (non la elimina):
  per cancellarla definitivamente usare il pulsante ✕ o la finestra elenco;
* la finestra «StickNotes – Elenco note» permette di creare, aprire,
  ripristinare ed eliminare le note; scorciatoie `Ctrl+N` (nuova nota),
  `Ctrl+Q` (esci);
* se l'estensione della barra è attiva, l'app resta in esecuzione in background
  anche senza finestre (l'icona nella barra continua a funzionare). Per uscire
  davvero: menu ☰ → «Esci» oppure menu dell'icona → «Esci da StickNotes».

### Icona nella barra: primo avvio

GNOME Shell scopre le estensioni solo all'avvio della sessione. La prima volta
che si installa StickNotes:

1. avvia l'app (che installa l'estensione e tenta di abilitarla);
2. se l'icona non compare, **esci e rientra dalla sessione GNOME** una volta;
3. alla sessione successiva l'icona sarà nella barra superiore. In alternativa:
   `gnome-extensions enable sticknotes@org.sticknotes.app`
   (menu ☰ → «Icona nella barra GNOME» ripete installazione e tentativo).

## Nota su Wayland

Su Wayland il compositor non permette alle applicazioni di:

* forzare una finestra "sempre in primo piano";
* rendere una finestra visibile su tutti i workspace;
* ripristinare la posizione assoluta delle finestre.

StickNotes salva comunque queste proprietà e le applica tramite EWMH quando
gira su X11/XWayland. Per avere "sempre in primo piano" e "tutti i workspace"
funzionanti su una sessione GNOME Wayland, avviare l'app con il backend X11:

```bash
GDK_BACKEND=x11 ./target/release/sticknotes
```

L'interfaccia mostra un avviso nel popover delle proprietà quando si è su
Wayland nativo.

## Struttura

```
src/main.rs    bootstrap dell'applicazione, azioni, finestra elenco, promemoria, CSS
src/note.rs    finestra nota: header, toolbar markdown, editor, anteprima, immagini, proprietà
src/md.rs      renderer Markdown → GtkTextBuffer (con immagini)
src/db.rs      persistenza SQLite (note + asset), migrazioni
src/gnome.rs   integrazione GNOME: .desktop/icona, autostart, estensione Shell, window hints EWMH
```
