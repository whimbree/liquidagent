use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS conversations (
    id          INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,
    session_id  TEXT,
    model       TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_conversation
    ON messages(conversation_id, id);
CREATE TABLE IF NOT EXISTS attachments (
    id              TEXT PRIMARY KEY,
    conversation_id INTEGER NOT NULL,
    message_id      INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    mime            TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_id);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS auth_sessions (
    token_hash TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    role       TEXT NOT NULL DEFAULT 'owner'
);
CREATE TABLE IF NOT EXISTS kv (
    app        TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (app, key)
);
CREATE TABLE IF NOT EXISTS push_subscriptions (
    endpoint   TEXT PRIMARY KEY,
    p256dh     TEXT NOT NULL,
    auth       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
";

#[derive(Clone, Debug, Serialize)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    pub session_id: Option<String>,
    /// Per-conversation model override (a pinned model id). None = inherit the
    /// global default. See `crate::api::effective_model`.
    pub model: Option<String>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoredMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    /// Image attachments on this message (rendered as thumbnails in the shell).
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
}

/// A reference to a stored attachment: its id (also the filename) and mime.
#[derive(Clone, Debug, Serialize)]
pub struct AttachmentRef {
    pub id: String,
    pub mime: String,
}

/// SQLite handle. rusqlite is sync; a Mutex is plenty at single-user scale —
/// every call is a short transaction, and WAL keeps readers cheap.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs() as i64
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA).context("applying schema")?;
        // Idempotent migration: add conversations.model to DBs created before it
        // existed. New DBs already have it from SCHEMA, so this errors with
        // "duplicate column name" — which we ignore.
        if let Err(err) = conn.execute("ALTER TABLE conversations ADD COLUMN model TEXT", []) {
            if !err.to_string().contains("duplicate column") {
                return Err(err).context("adding conversations.model column");
            }
        }
        if let Err(err) =
            conn.execute("ALTER TABLE auth_sessions ADD COLUMN role TEXT NOT NULL DEFAULT 'owner'", [])
        {
            if !err.to_string().contains("duplicate column") {
                return Err(err).context("adding auth_sessions.role column");
            }
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("db mutex poisoned")
    }

    // --- conversations -------------------------------------------------------

    pub fn create_conversation(&self, title: &str) -> anyhow::Result<i64> {
        let ts = now();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO conversations (title, created_at, updated_at) VALUES (?1, ?2, ?2)",
            rusqlite::params![title, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, session_id, model, updated_at FROM conversations ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                session_id: row.get(2)?,
                model: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The per-conversation model override, if one is set (None = inherit global).
    pub fn conversation_model(&self, id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let model = conn
            .query_row("SELECT model FROM conversations WHERE id = ?1", [id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
        Ok(model)
    }

    /// Set (Some) or clear (None) a conversation's model override.
    pub fn set_conversation_model(&self, id: i64, model: Option<&str>) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE conversations SET model = ?2 WHERE id = ?1",
            rusqlite::params![id, model],
        )?;
        Ok(())
    }

    pub fn conversation_title(&self, id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let title = conn
            .query_row("SELECT title FROM conversations WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(title)
    }

    pub fn conversation_session(&self, id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let session = conn
            .query_row(
                "SELECT session_id FROM conversations WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .context("conversation not found")?;
        Ok(session)
    }

    pub fn set_conversation_session(&self, id: i64, session_id: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE conversations SET session_id = ?2 WHERE id = ?1",
            rusqlite::params![id, session_id],
        )?;
        Ok(())
    }

    pub fn delete_conversation(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        Ok(())
    }

    // --- messages ------------------------------------------------------------

    /// Append a message and return its row id (used to link attachments).
    pub fn append_message(&self, conversation_id: i64, role: &str, content: &str) -> anyhow::Result<i64> {
        let ts = now();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO messages (conversation_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation_id, role, content, ts],
        )?;
        conn.execute(
            "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
            rusqlite::params![conversation_id, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Record an image attachment on a message. The image bytes live in a file
    /// named `id` under the attachments dir; only metadata is in the DB.
    pub fn add_attachment(&self, id: &str, conversation_id: i64, message_id: i64, mime: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO attachments (id, conversation_id, message_id, mime, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, conversation_id, message_id, mime, now()],
        )?;
        Ok(())
    }

    /// The mime of a stored attachment (also confirms it exists) — for serving.
    pub fn attachment_mime(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .lock()
            .query_row("SELECT mime FROM attachments WHERE id = ?1", [id], |row| row.get(0))
            .optional()?)
    }

    pub fn list_messages(&self, conversation_id: i64) -> anyhow::Result<Vec<StoredMessage>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, role, content, created_at FROM messages WHERE conversation_id = ?1 ORDER BY id",
        )?;
        let mut messages: Vec<StoredMessage> = stmt
            .query_map([conversation_id], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                    attachments: Vec::new(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut att = conn.prepare("SELECT id, mime FROM attachments WHERE message_id = ?1 ORDER BY rowid")?;
        for m in &mut messages {
            m.attachments = att
                .query_map([m.id], |row| Ok(AttachmentRef { id: row.get(0)?, mime: row.get(1)? }))?
                .collect::<Result<_, _>>()?;
        }
        Ok(messages)
    }

    // --- settings ------------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let value = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    // --- per-app key-value storage ----------------------------------------------

    pub fn kv_get(&self, app: &str, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let value = conn
            .query_row(
                "SELECT value FROM kv WHERE app = ?1 AND key = ?2",
                rusqlite::params![app, key],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(value)
    }

    pub fn kv_set(&self, app: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO kv (app, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(app, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![app, key, value, now()],
        )?;
        Ok(())
    }

    pub fn kv_delete(&self, app: &str, key: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "DELETE FROM kv WHERE app = ?1 AND key = ?2",
            rusqlite::params![app, key],
        )?;
        Ok(())
    }

    pub fn kv_list(&self, app: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT key FROM kv WHERE app = ?1 ORDER BY key")?;
        let rows = stmt.query_map([app], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // --- push subscriptions --------------------------------------------------------

    pub fn add_push_subscription(&self, endpoint: &str, p256dh: &str, auth: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO push_subscriptions (endpoint, p256dh, auth, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(endpoint) DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth",
            rusqlite::params![endpoint, p256dh, auth, now()],
        )?;
        Ok(())
    }

    pub fn remove_push_subscription(&self, endpoint: &str) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM push_subscriptions WHERE endpoint = ?1", [endpoint])?;
        Ok(())
    }

    pub fn list_push_subscriptions(&self) -> anyhow::Result<Vec<(String, String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT endpoint, p256dh, auth FROM push_subscriptions")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // --- auth sessions ---------------------------------------------------------

    pub fn insert_auth_session(&self, token_hash: &str, ttl_secs: i64, role: &str) -> anyhow::Result<()> {
        let ts = now();
        self.lock().execute(
            "INSERT INTO auth_sessions (token_hash, created_at, expires_at, role) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token_hash, ts, ts + ttl_secs, role],
        )?;
        Ok(())
    }

    /// The role of a live session (None = invalid/expired). The primitive the
    /// guest shell and SSO will build on: today only "owner" sessions exist.
    pub fn auth_session_role(&self, token_hash: &str) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        conn.execute("DELETE FROM auth_sessions WHERE expires_at < ?1", [now()])?;
        Ok(conn
            .query_row(
                "SELECT role FROM auth_sessions WHERE token_hash = ?1 AND expires_at >= ?2",
                rusqlite::params![token_hash, now()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Revoke every session. Used on a password change: rotating the password
    /// must invalidate any token that could have leaked under the old one.
    pub fn clear_auth_sessions(&self) -> anyhow::Result<()> {
        self.lock().execute("DELETE FROM auth_sessions", [])?;
        Ok(())
    }

    pub fn auth_session_valid(&self, token_hash: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        // Opportunistic cleanup keeps the table tiny.
        conn.execute("DELETE FROM auth_sessions WHERE expires_at < ?1", [now()])?;
        let found = conn
            .query_row(
                "SELECT 1 FROM auth_sessions WHERE token_hash = ?1 AND expires_at >= ?2",
                rusqlite::params![token_hash, now()],
                |_| Ok(()),
            )
            .map(|_| true)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_and_message_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let id = db.create_conversation("hello world").unwrap();
        db.append_message(id, "user", "hi").unwrap();
        db.append_message(id, "assistant", "hello!").unwrap();

        let conversations = db.list_conversations().unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].title, "hello world");
        assert_eq!(conversations[0].session_id, None);

        let messages = db.list_messages(id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].content, "hello!");
    }

    #[test]
    fn session_id_persists_per_conversation() {
        let db = Db::open_in_memory().unwrap();
        let a = db.create_conversation("a").unwrap();
        let b = db.create_conversation("b").unwrap();
        db.set_conversation_session(a, "session-a").unwrap();
        assert_eq!(db.conversation_session(a).unwrap(), Some("session-a".into()));
        assert_eq!(db.conversation_session(b).unwrap(), None);
    }

    #[test]
    fn delete_conversation_cascades_messages() {
        let db = Db::open_in_memory().unwrap();
        let id = db.create_conversation("bye").unwrap();
        db.append_message(id, "user", "x").unwrap();
        db.delete_conversation(id).unwrap();
        assert!(db.list_conversations().unwrap().is_empty());
        assert!(db.list_messages(id).unwrap().is_empty());
    }

    #[test]
    fn kv_is_scoped_per_app() {
        let db = Db::open_in_memory().unwrap();
        db.kv_set("calc", "state", "1+1").unwrap();
        db.kv_set("notes", "state", "todo").unwrap();
        assert_eq!(db.kv_get("calc", "state").unwrap(), Some("1+1".into()));
        assert_eq!(db.kv_get("notes", "state").unwrap(), Some("todo".into()));
        assert_eq!(db.kv_get("calc", "missing").unwrap(), None);

        db.kv_set("calc", "state", "2+2").unwrap(); // upsert
        assert_eq!(db.kv_get("calc", "state").unwrap(), Some("2+2".into()));
        assert_eq!(db.kv_list("calc").unwrap(), vec!["state".to_string()]);

        db.kv_delete("calc", "state").unwrap();
        assert_eq!(db.kv_get("calc", "state").unwrap(), None);
        assert_eq!(db.kv_get("notes", "state").unwrap(), Some("todo".into()));
    }

    #[test]
    fn settings_upsert() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("k").unwrap(), None);
        db.set_setting("k", "v1").unwrap();
        db.set_setting("k", "v2").unwrap();
        assert_eq!(db.get_setting("k").unwrap(), Some("v2".into()));
    }

    #[test]
    fn auth_sessions_expire() {
        let db = Db::open_in_memory().unwrap();
        db.insert_auth_session("fresh", 3600, "owner").unwrap();
        db.insert_auth_session("stale", -10, "owner").unwrap();
        assert!(db.auth_session_valid("fresh").unwrap());
        assert!(!db.auth_session_valid("stale").unwrap());
        assert!(!db.auth_session_valid("unknown").unwrap());
    }

    #[test]
    fn clear_auth_sessions_revokes_all() {
        let db = Db::open_in_memory().unwrap();
        db.insert_auth_session("a", 3600, "owner").unwrap();
        db.insert_auth_session("b", 3600, "owner").unwrap();
        assert!(db.auth_session_valid("a").unwrap());
        db.clear_auth_sessions().unwrap();
        assert!(!db.auth_session_valid("a").unwrap());
        assert!(!db.auth_session_valid("b").unwrap());
    }
}
