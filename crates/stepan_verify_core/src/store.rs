//! App-owned state in one SQLite file, `~/.stepan_verify/state/stepan.db`:
//! what the window had open, an index of agent session files so the session
//! list is there before any transcript is read, and the last transcript of
//! each open session so a restored tab shows its conversation at once.
//!
//! Only state the app owns lives here. What people and agents edit (tasks,
//! docs, memory, config.toml) stays plain files.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::{Home, Provider};

/// Bump with a new arm in [`migrate`] when the schema changes.
const SCHEMA: i64 = 1;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

/// What a scan learned about one session file. `offset` is how far a Claude
/// transcript has been read: the files only grow, so the next scan reads on
/// from there instead of from the start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileRow {
    pub provider: Option<Provider>,
    pub mtime_ns: i64,
    pub size: i64,
    pub offset: i64,
    pub id: String,
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    pub cwd: Option<PathBuf>,
}

impl Store {
    pub fn open(home: &Home) -> Result<Self> {
        Self::open_at(&home.state().join("stepan.db"))
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        // WAL: the app writes while a background scan reads; another Stepan on the same home waits instead of failing.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&conn)?)
    }

    // ------------------------------------------------------------------ kv

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let text: Option<String> =
            self.with(|c| c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()).ok()??;
        serde_json::from_str(&text?).ok()
    }

    pub fn put<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let text = serde_json::to_string(value)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO kv(key, value, updated_at) VALUES(?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, text, now()],
            )
        })?;
        Ok(())
    }

    // ----------------------------------------------------------- snapshots

    /// The last transcript of a session, as JSON the app wrote.
    pub fn snapshot(&self, key: &str) -> Option<String> {
        self.with(|c| c.query_row("SELECT items FROM snapshots WHERE key = ?1", [key], |r| r.get(0)).optional()).ok()?
    }

    pub fn put_snapshot(&self, key: &str, items: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO snapshots(key, items, updated_at) VALUES(?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET items = excluded.items, updated_at = excluded.updated_at",
                params![key, items, now()],
            )
        })?;
        Ok(())
    }

    /// Drop snapshots not written for `days`: the session was closed long ago.
    pub fn prune_snapshots(&self, days: i64) -> Result<usize> {
        self.with(|c| c.execute("DELETE FROM snapshots WHERE updated_at < ?1", [now() - days * 86_400]))
    }

    // -------------------------------------------------------- session index

    pub fn files(&self) -> HashMap<PathBuf, FileRow> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT path, provider, mtime_ns, size, offset, id, title, first_prompt, cwd FROM session_files")?;
            let rows = stmt.query_map([], |r| {
                let path: String = r.get(0)?;
                let provider: String = r.get(1)?;
                Ok((
                    PathBuf::from(path),
                    FileRow {
                        provider: provider_from(&provider),
                        mtime_ns: r.get(2)?,
                        size: r.get(3)?,
                        offset: r.get(4)?,
                        id: r.get(5)?,
                        title: r.get(6)?,
                        first_prompt: r.get(7)?,
                        cwd: r.get::<_, Option<String>>(8)?.map(PathBuf::from),
                    },
                ))
            })?;
            rows.collect()
        })
        .unwrap_or_default()
    }

    /// Replace the index with what one scan saw: rows for files it did not see are gone.
    pub fn replace_files(&self, rows: &HashMap<PathBuf, FileRow>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM session_files", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO session_files(path, provider, mtime_ns, size, offset, id, title, first_prompt, cwd)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (path, row) in rows {
                stmt.execute(params![
                    path.to_string_lossy(),
                    row.provider.map_or("", provider_str),
                    row.mtime_ns,
                    row.size,
                    row.offset,
                    row.id,
                    row.title,
                    row.first_prompt,
                    row.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv(
                 key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS snapshots(
                 key TEXT PRIMARY KEY, items TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS session_files(
                 path TEXT PRIMARY KEY, provider TEXT NOT NULL, mtime_ns INTEGER NOT NULL,
                 size INTEGER NOT NULL, offset INTEGER NOT NULL, id TEXT NOT NULL,
                 title TEXT, first_prompt TEXT, cwd TEXT);",
        )?;
    }
    if version < SCHEMA {
        conn.pragma_update(None, "user_version", SCHEMA)?;
    }
    Ok(())
}

fn provider_str(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

fn provider_from(s: &str) -> Option<Provider> {
    match s {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        _ => None,
    }
}

pub(crate) fn mtime_ns(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

fn now() -> i64 {
    mtime_ns(SystemTime::now()) / 1_000_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("stepan-store-{}-{}", std::process::id(), rand_suffix()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open_at(&dir.join("stepan.db")).unwrap();
        (dir, store)
    }

    fn rand_suffix() -> u128 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn kv_and_snapshots_round_trip() {
        let (dir, store) = temp();
        assert_eq!(store.get::<Vec<String>>("open"), None);
        store.put("open", &vec!["a".to_owned(), "б".to_owned()]).unwrap();
        store.put("open", &vec!["в".to_owned()]).unwrap();
        assert_eq!(store.get::<Vec<String>>("open"), Some(vec!["в".to_owned()]));
        store.put_snapshot("claude:1", "[1]").unwrap();
        assert_eq!(store.snapshot("claude:1").as_deref(), Some("[1]"));
        assert_eq!(store.prune_snapshots(1).unwrap(), 0);
        // Reopening sees the same data (and does not re-run the migration).
        drop(store);
        let again = Store::open_at(&dir.join("stepan.db")).unwrap();
        assert_eq!(again.snapshot("claude:1").as_deref(), Some("[1]"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_index_is_replaced_whole() {
        let (dir, store) = temp();
        let row = |id: &str| FileRow { provider: Some(Provider::Claude), mtime_ns: 5, size: 10, offset: 10, id: id.into(), title: Some("т".into()), ..Default::default() };
        store.replace_files(&HashMap::from([(PathBuf::from("/a.jsonl"), row("a")), (PathBuf::from("/b.jsonl"), row("b"))])).unwrap();
        store.replace_files(&HashMap::from([(PathBuf::from("/b.jsonl"), row("b2"))])).unwrap();
        let files = store.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[Path::new("/b.jsonl")].id, "b2");
        assert_eq!(files[Path::new("/b.jsonl")].provider, Some(Provider::Claude));
        let _ = std::fs::remove_dir_all(dir);
    }
}
