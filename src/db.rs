// Persistence layer: every note lives in a single SQLite database
// (~/.local/share/org.sticknotes.app/sticknotes.db).
//
// Schema (new columns are added with ALTER TABLE if missing, never dropped):
//   notes(id, title, body_md, color, x, y, w, h, pinned, all_workspaces,
//         created_at, daily_reminder TEXT 'HH:MM' NULL, last_reminder_date,
//         auto_open, due_date TEXT 'YYYY-MM-DD' NULL,
//         priority TEXT 'none'|'low'|'medium'|'high',
//         deleted_at TEXT NULL)  -- soft delete: row kept, only hidden
//   assets(id, note_id, filename, mime, blob)  -- images pasted in the notes

use std::fs;
use std::path::PathBuf;

use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};

/// Priority of a note. Persisted as a lowercase string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::None => "none",
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "high" => Priority::High,
            "medium" => Priority::Medium,
            "low" => Priority::Low,
            _ => Priority::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Note {
    pub id: i64,
    pub title: String,
    pub body_md: String,
    pub color: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub pinned: bool,
    pub all_workspaces: bool,
    pub created_at: String,
    /// Daily reminder as "HH:MM", None = no reminder.
    pub daily_reminder: Option<String>,
    /// Last date (YYYY-MM-DD) the reminder fired, to avoid duplicates.
    pub last_reminder_date: Option<String>,
    /// Reopen this note automatically when StickNotes starts.
    pub auto_open: bool,
    /// Optional deadline, "YYYY-MM-DD".
    pub due_date: Option<String>,
    /// Note priority (none/low/medium/high).
    pub priority: Priority,
    /// Soft delete timestamp; None = active note.
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // full row, only `blob` is consumed by the cache
pub struct Asset {
    pub id: i64,
    pub note_id: i64,
    pub filename: String,
    pub mime: String,
    pub blob: Vec<u8>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (and create if needed) the database inside the user's data dir.
    pub fn open_default() -> rusqlite::Result<Self> {
        let dir = Self::data_dir();
        fs::create_dir_all(&dir).ok();
        let path = dir.join("sticknotes.db");
        Self::open(&path)
    }

    pub fn data_dir() -> PathBuf {
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("org.sticknotes.app")
    }

    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL DEFAULT '',
                body_md TEXT NOT NULL DEFAULT '',
                color TEXT NOT NULL DEFAULT '#fff4c2',
                x INTEGER NOT NULL DEFAULT 120,
                y INTEGER NOT NULL DEFAULT 120,
                w INTEGER NOT NULL DEFAULT 360,
                h INTEGER NOT NULL DEFAULT 380,
                pinned INTEGER NOT NULL DEFAULT 0,
                all_workspaces INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                daily_reminder TEXT,
                last_reminder_date TEXT,
                auto_open INTEGER NOT NULL DEFAULT 1,
                due_date TEXT,
                priority TEXT NOT NULL DEFAULT 'none',
                deleted_at TEXT
             );
             CREATE TABLE IF NOT EXISTS assets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                note_id INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                filename TEXT NOT NULL,
                mime TEXT NOT NULL,
                blob BLOB NOT NULL
             );",
        )?;

        // Additive migrations. Existing databases keep all their notes: we
        // only add the missing columns, never recreate or delete anything.
        Self::ensure_column(&conn, "auto_open", "auto_open INTEGER NOT NULL DEFAULT 1")?;
        Self::ensure_column(&conn, "due_date", "due_date TEXT")?;
        Self::ensure_column(&conn, "priority", "priority TEXT NOT NULL DEFAULT 'none'")?;
        Self::ensure_column(&conn, "deleted_at", "deleted_at TEXT")?;

        Ok(Self { conn })
    }

    fn ensure_column(conn: &Connection, column: &str, ddl: &str) -> rusqlite::Result<()> {
        let exists = {
            let mut stmt = conn.prepare("PRAGMA table_info(notes)")?;
            let mut rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            rows.any(|name| name.map(|name| name == column).unwrap_or(false))
        };
        if !exists {
            conn.execute(&format!("ALTER TABLE notes ADD COLUMN {ddl}"), [])?;
        }
        Ok(())
    }

    fn row_to_note(r: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
        let priority: String = r.get(15)?;
        Ok(Note {
            id: r.get(0)?,
            title: r.get(1)?,
            body_md: r.get(2)?,
            color: r.get(3)?,
            x: r.get(4)?,
            y: r.get(5)?,
            w: r.get(6)?,
            h: r.get(7)?,
            pinned: r.get::<_, i64>(8)? != 0,
            all_workspaces: r.get::<_, i64>(9)? != 0,
            created_at: r.get(10)?,
            daily_reminder: r.get(11)?,
            last_reminder_date: r.get(12)?,
            auto_open: r.get::<_, i64>(13)? != 0,
            due_date: r.get(14)?,
            priority: Priority::from_db(&priority),
            deleted_at: r.get(16)?,
        })
    }

    const NOTE_COLS: &'static str =
        "id, title, body_md, color, x, y, w, h, pinned, all_workspaces, created_at, daily_reminder, last_reminder_date, auto_open, due_date, priority, deleted_at";

    /// Active notes only (soft-deleted notes are excluded).
    pub fn all_notes(&self) -> rusqlite::Result<Vec<Note>> {
        let sql = format!(
            "SELECT {} FROM notes WHERE deleted_at IS NULL ORDER BY id",
            Self::NOTE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::row_to_note)?;
        rows.collect()
    }

    /// Notes in the trash, most recently deleted first.
    pub fn deleted_notes(&self) -> rusqlite::Result<Vec<Note>> {
        let sql = format!(
            "SELECT {} FROM notes WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC, id DESC",
            Self::NOTE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::row_to_note)?;
        rows.collect()
    }

    /// Any note, including soft-deleted ones (used by the trash view).
    pub fn get_note(&self, id: i64) -> rusqlite::Result<Option<Note>> {
        let sql = format!("SELECT {} FROM notes WHERE id = ?1", Self::NOTE_COLS);
        self.conn
            .query_row(&sql, params![id], Self::row_to_note)
            .optional()
    }

    /// Only active notes: the windows must never reopen a deleted note.
    pub fn get_active_note(&self, id: i64) -> rusqlite::Result<Option<Note>> {
        let sql = format!(
            "SELECT {} FROM notes WHERE id = ?1 AND deleted_at IS NULL",
            Self::NOTE_COLS
        );
        self.conn
            .query_row(&sql, params![id], Self::row_to_note)
            .optional()
    }

    pub fn create_note(&self, n: &mut Note) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO notes (title, body_md, color, x, y, w, h, pinned, all_workspaces, created_at, daily_reminder, last_reminder_date, auto_open, due_date, priority, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                n.title,
                n.body_md,
                n.color,
                n.x,
                n.y,
                n.w,
                n.h,
                n.pinned as i64,
                n.all_workspaces as i64,
                n.created_at,
                n.daily_reminder,
                n.last_reminder_date,
                n.auto_open as i64,
                n.due_date,
                n.priority.as_str(),
                n.deleted_at
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        n.id = id;
        Ok(id)
    }

    /// Full update of a note row (used when closing / applying properties).
    pub fn update_note(&self, n: &Note) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE notes SET title=?1, body_md=?2, color=?3, x=?4, y=?5, w=?6, h=?7,
             pinned=?8, all_workspaces=?9, daily_reminder=?10, last_reminder_date=?11,
             auto_open=?12, due_date=?13, priority=?14, deleted_at=?15
             WHERE id=?16",
            params![
                n.title,
                n.body_md,
                n.color,
                n.x,
                n.y,
                n.w,
                n.h,
                n.pinned as i64,
                n.all_workspaces as i64,
                n.daily_reminder,
                n.last_reminder_date,
                n.auto_open as i64,
                n.due_date,
                n.priority.as_str(),
                n.deleted_at,
                n.id
            ],
        )?;
        Ok(())
    }

    pub fn set_body(&self, id: i64, body_md: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE notes SET body_md=?1 WHERE id=?2",
            params![body_md, id],
        )?;
        Ok(())
    }

    pub fn set_title(&self, id: i64, title: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("UPDATE notes SET title=?1 WHERE id=?2", params![title, id])?;
        Ok(())
    }

    pub fn set_properties(
        &self,
        id: i64,
        pinned: bool,
        all_workspaces: bool,
        daily_reminder: Option<&str>,
        auto_open: bool,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE notes SET pinned=?1, all_workspaces=?2, daily_reminder=?3, auto_open=?4 WHERE id=?5",
            params![
                pinned as i64,
                all_workspaces as i64,
                daily_reminder,
                auto_open as i64,
                id
            ],
        )?;
        Ok(())
    }

    pub fn set_due_date(&self, id: i64, due_date: Option<&str>) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE notes SET due_date=?1 WHERE id=?2",
            params![due_date, id],
        )?;
        Ok(())
    }

    pub fn set_priority(&self, id: i64, priority: Priority) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE notes SET priority=?1 WHERE id=?2",
            params![priority.as_str(), id],
        )?;
        Ok(())
    }

    pub fn set_color(&self, id: i64, color: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("UPDATE notes SET color=?1 WHERE id=?2", params![color, id])?;
        Ok(())
    }

    pub fn set_geometry(&self, id: i64, w: i32, h: i32) -> rusqlite::Result<()> {
        self.conn
            .execute("UPDATE notes SET w=?1, h=?2 WHERE id=?3", params![w, h, id])?;
        Ok(())
    }

    /// Soft delete: the row (and its images) stays in the database.
    pub fn delete_note(&self, id: i64) -> rusqlite::Result<()> {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.conn.execute(
            "UPDATE notes SET deleted_at=?1 WHERE id=?2 AND deleted_at IS NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Restore a note from the trash.
    pub fn restore_note(&self, id: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("UPDATE notes SET deleted_at=NULL WHERE id=?1", params![id])?;
        Ok(())
    }

    // ---- assets (pasted images) ----

    pub fn add_asset(
        &self,
        note_id: i64,
        filename: &str,
        mime: &str,
        blob: &[u8],
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO assets (note_id, filename, mime, blob) VALUES (?1,?2,?3,?4)",
            params![note_id, filename, mime, blob],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn asset_by_id(&self, id: i64) -> rusqlite::Result<Option<Asset>> {
        self.conn
            .query_row(
                "SELECT id, note_id, filename, mime, blob FROM assets WHERE id=?1",
                params![id],
                |r| {
                    Ok(Asset {
                        id: r.get(0)?,
                        note_id: r.get(1)?,
                        filename: r.get(2)?,
                        mime: r.get(3)?,
                        blob: r.get(4)?,
                    })
                },
            )
            .optional()
    }

    pub fn all_asset_ids(&self) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare("SELECT id FROM assets ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect()
    }

    /// Directory where image assets are exported as PNG so that GdkTexture
    /// can load them from disk.
    pub fn cache_dir() -> PathBuf {
        Self::data_dir().join("cache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_note() -> Note {
        Note {
            id: 0,
            title: "Test".into(),
            body_md: "# Titolo\n\ncorpo".into(),
            color: "#fff4c2".into(),
            x: 10,
            y: 20,
            w: 300,
            h: 400,
            pinned: true,
            all_workspaces: false,
            created_at: "2026-01-01 10:00".into(),
            daily_reminder: Some("09:30".into()),
            last_reminder_date: None,
            auto_open: true,
            due_date: Some("2026-02-01".into()),
            priority: Priority::High,
            deleted_at: None,
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sticknotes-{name}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn note_and_asset_roundtrip_with_soft_delete() {
        let path = temp_path("roundtrip");
        let db = Database::open(&path).expect("open db");

        let mut note = sample_note();
        let id = db.create_note(&mut note).expect("create");
        assert!(id > 0);

        let loaded = db.get_note(id).expect("get").expect("existing");
        assert_eq!(loaded.body_md, note.body_md);
        assert!(loaded.pinned);
        assert_eq!(loaded.daily_reminder.as_deref(), Some("09:30"));
        assert_eq!(loaded.due_date.as_deref(), Some("2026-02-01"));
        assert_eq!(loaded.priority, Priority::High);

        db.set_body(id, "nuovo corpo").unwrap();
        db.set_properties(id, false, true, None, false).unwrap();
        db.set_priority(id, Priority::Low).unwrap();
        db.set_due_date(id, None).unwrap();
        db.set_color(id, "#d8f5c8").unwrap();
        let updated = db.get_note(id).unwrap().unwrap();
        assert_eq!(updated.body_md, "nuovo corpo");
        assert!(!updated.pinned);
        assert!(updated.all_workspaces);
        assert!(updated.daily_reminder.is_none());
        assert!(!updated.auto_open);
        assert_eq!(updated.priority, Priority::Low);
        assert!(updated.due_date.is_none());
        assert_eq!(updated.color, "#d8f5c8");

        let asset = db
            .add_asset(id, "img.png", "image/png", &[1, 2, 3, 4])
            .unwrap();

        // Soft delete: the row and the image remain in the database.
        db.delete_note(id).unwrap();
        assert!(db.get_note(id).unwrap().unwrap().deleted_at.is_some());
        assert!(db.get_active_note(id).unwrap().is_none());
        assert!(db.all_notes().unwrap().is_empty());
        assert_eq!(db.deleted_notes().unwrap().len(), 1);
        assert!(db.asset_by_id(asset).unwrap().is_some());

        // Restore brings it back.
        db.restore_note(id).unwrap();
        assert!(db.get_active_note(id).unwrap().is_some());
        assert_eq!(db.all_notes().unwrap().len(), 1);
        assert!(db.deleted_notes().unwrap().is_empty());

        drop(db);
        cleanup(&path);
    }

    #[test]
    fn migration_keeps_existing_rows() {
        let path = temp_path("migration");
        {
            // Old schema, with one real note and no new columns.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE notes (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL DEFAULT '',
                    body_md TEXT NOT NULL DEFAULT '',
                    color TEXT NOT NULL DEFAULT '#fff4c2',
                    x INTEGER NOT NULL DEFAULT 120,
                    y INTEGER NOT NULL DEFAULT 120,
                    w INTEGER NOT NULL DEFAULT 360,
                    h INTEGER NOT NULL DEFAULT 380,
                    pinned INTEGER NOT NULL DEFAULT 0,
                    all_workspaces INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL,
                    daily_reminder TEXT,
                    last_reminder_date TEXT
                 );
                 INSERT INTO notes (title, body_md, created_at)
                 VALUES ('Nota reale', 'contenuto', '2026-01-01 10:00');",
            )
            .unwrap();
        }

        let db = Database::open(&path).expect("migrate");
        let notes = db.all_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "Nota reale");
        assert_eq!(notes[0].body_md, "contenuto");
        assert_eq!(notes[0].priority, Priority::None);
        assert!(notes[0].due_date.is_none());
        assert!(notes[0].deleted_at.is_none());

        drop(db);
        cleanup(&path);
    }

    #[test]
    fn priority_strings() {
        for priority in [
            Priority::None,
            Priority::Low,
            Priority::Medium,
            Priority::High,
        ] {
            assert_eq!(Priority::from_db(priority.as_str()), priority);
        }
        assert_eq!(Priority::from_db("unknown"), Priority::None);
    }
}
