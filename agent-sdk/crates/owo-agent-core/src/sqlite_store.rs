//! SQLite 会话存储：`<appdata>/index.db`（对应技术文档 5.7）。

use crate::error::AgentError;
use crate::session::{Session, SessionStore, SnapshotEntry};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
}

impl SqliteSessionStore {
    pub fn open(path: &Path) -> Result<Self, AgentError> {
        let conn = Connection::open(path).map_err(sqlite_error)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS sessions (
                 id TEXT PRIMARY KEY,
                 workspace TEXT NOT NULL,
                 model TEXT NOT NULL,
                 system_prompt TEXT,
                 messages_json TEXT NOT NULL,
                 snapshots_json TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 parent_id TEXT,
                 fork_point INTEGER,
                 redo_json TEXT NOT NULL
             );",
        )
        .map_err(sqlite_error)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn save_locked(conn: &Connection, session: &Session) -> Result<(), AgentError> {
        conn.execute(
            "INSERT INTO sessions (
                 id, workspace, model, system_prompt, messages_json, snapshots_json,
                 created_at, updated_at, parent_id, fork_point, redo_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 workspace=excluded.workspace,
                 model=excluded.model,
                 system_prompt=excluded.system_prompt,
                 messages_json=excluded.messages_json,
                 snapshots_json=excluded.snapshots_json,
                 updated_at=excluded.updated_at,
                 parent_id=excluded.parent_id,
                 fork_point=excluded.fork_point,
                 redo_json=excluded.redo_json",
            params![
                session.id,
                session.workspace.to_string_lossy(),
                session.model,
                session.system_prompt,
                serde_json::to_string(&session.messages).map_err(json_error)?,
                serde_json::to_string(&session.snapshots).map_err(json_error)?,
                session.created_at,
                session.updated_at,
                session.parent_id,
                session.fork_point.map(|point| point as i64),
                serde_json::to_string(&session.redo_stack).map_err(json_error)?,
            ],
        )
        .map_err(sqlite_error)?;
        Ok(())
    }

    fn load_locked(conn: &Connection, id: &str) -> Result<Session, AgentError> {
        let row = conn
            .query_row(
                "SELECT id, workspace, model, system_prompt, messages_json, snapshots_json,
                        created_at, updated_at, parent_id, fork_point, redo_json
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                },
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    AgentError::Session(format!("会话不存在：{id}"))
                }
                other => sqlite_error(other),
            })?;
        Ok(Session {
            id: row.0,
            workspace: row.1.into(),
            model: row.2,
            system_prompt: row.3,
            messages: serde_json::from_str(&row.4).map_err(json_error)?,
            snapshots: serde_json::from_str::<HashMap<String, SnapshotEntry>>(&row.5)
                .map_err(json_error)?,
            created_at: row.6,
            updated_at: row.7,
            parent_id: row.8,
            fork_point: row.9.map(|point| point as usize),
            redo_stack: serde_json::from_str(&row.10).map_err(json_error)?,
        })
    }
}

impl SessionStore for SqliteSessionStore {
    fn create(
        &self,
        workspace: &Path,
        model: &str,
        system_prompt: Option<&str>,
    ) -> Result<Session, AgentError> {
        let session = Session::new(workspace, model, system_prompt.map(str::to_string));
        let conn = self
            .conn
            .lock()
            .map_err(|_| AgentError::Session("SQLite 锁中毒".into()))?;
        Self::save_locked(&conn, &session)?;
        Ok(session)
    }

    fn load(&self, id: &str) -> Result<Session, AgentError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| AgentError::Session("SQLite 锁中毒".into()))?;
        Self::load_locked(&conn, id)
    }

    fn save(&self, session: &Session) -> Result<(), AgentError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| AgentError::Session("SQLite 锁中毒".into()))?;
        Self::save_locked(&conn, session)
    }

    fn list(&self) -> Vec<String> {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return Vec::new(),
        };
        let mut statement = match conn.prepare("SELECT id FROM sessions ORDER BY updated_at DESC") {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }
}

fn sqlite_error(error: rusqlite::Error) -> AgentError {
    AgentError::Session(format!("SQLite 错误：{error}"))
}

fn json_error(error: serde_json::Error) -> AgentError {
    AgentError::Json(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ChatMessage;

    #[test]
    fn sqlite_session_round_trip_and_list() {
        let path =
            std::env::temp_dir().join(format!("owo-sqlite-test-{}.db", uuid::Uuid::new_v4()));
        let store = SqliteSessionStore::open(&path).unwrap();
        let mut session = store.create(Path::new("."), "mock", None).unwrap();
        session.push(ChatMessage::user("你好".to_string()));
        session.push(ChatMessage::assistant_text("收到".to_string()));
        store.save(&session).unwrap();
        let child = session.fork(1);
        store.save(&child).unwrap();

        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].content.as_deref(), Some("你好"));
        let loaded_child = store.load(&child.id).unwrap();
        assert_eq!(loaded_child.parent_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(loaded_child.fork_point, Some(1));
        assert_eq!(store.list().len(), 2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
