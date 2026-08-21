//! SQLite 会话存储：`<appdata>/index.db`（对应技术文档 5.7）。
//!
//! R8：迁移框架——`PRAGMA user_version` + 顺序迁移表（`MIGRATIONS`），
//! `open` 时自动迁移；迁移失败降级只读并记录提示，禁止运行时隐式 ALTER。

use crate::audit::AuditEntry;
use crate::error::AgentError;
use crate::session::{Session, SessionStore, SnapshotEntry};
use crate::storage_crypto::{decrypt_file_envelope, encrypt_file_envelope, StorageCryptoError};
use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// 库文件信封加密（R9）：整库拷贝加密（备份/导出面），运行时 SQLite 保持原语义。
/// 加密文件后缀（`.owo-crypt` 由 storage_crypto 信封格式承载）。
pub fn encrypt_db_copy(source: &Path, target: &Path) -> Result<(), StorageCryptoError> {
    let bytes = std::fs::read(source)?;
    encrypt_file_envelope(target, &bytes)
}

/// 库文件信封解密：`<target>.owo-crypt` → 解密写入 target（覆盖）。
pub fn decrypt_db_copy(source: &Path, target: &Path) -> Result<(), StorageCryptoError> {
    let plain = decrypt_file_envelope(source)?;
    std::fs::write(target, plain)?;
    Ok(())
}

/// 打开加密库文件的只读快照：解密到临时文件并 open（调用方负责清理临时文件）。
pub fn open_encrypted_snapshot(
    encrypted_path: &Path,
    tmp_dir: &Path,
) -> Result<(SqliteSessionStore, std::path::PathBuf), AgentError> {
    let tmp = tmp_dir.join(format!("owo-db-dec-{}", uuid::Uuid::new_v4()));
    decrypt_db_copy(encrypted_path, &tmp)
        .map_err(|error| AgentError::Session(format!("加密库解密失败：{error}")))?;
    let store = SqliteSessionStore::open(&tmp)?;
    Ok((store, tmp))
}

/// 一条注册迁移：version 必须严格递增，run 在单个事务内执行并同步 user_version。
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub run: fn(&Connection) -> Result<(), AgentError>,
}

/// 顺序迁移表：任何 schema 变更都必须以新条目显式注册（禁止隐式 ALTER）。
/// v1：sessions 列补齐（此前为运行时逐列探测的隐式 ALTER，R8 收敛为注册迁移）。
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "sessions 列补齐（message_redo_json/title/archived/pinned）",
    run: |conn| {
        let columns = table_columns(conn, "sessions")?;
        for (column, definition) in [
            ("message_redo_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("title", "TEXT"),
            ("archived", "INTEGER NOT NULL DEFAULT 0"),
            ("pinned", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.iter().any(|existing| existing == column) {
                conn.execute_batch(&format!(
                    "ALTER TABLE sessions ADD COLUMN {column} {definition}"
                ))
                .map_err(sqlite_error)?;
            }
        }
        Ok(())
    },
}];

/// 迁移运行状态（供健康/状态面板展示；只读降级时 last_error 给出原因）。
#[derive(Debug, Clone, Default)]
pub struct MigrationStatus {
    /// 当前 schema 版本（PRAGMA user_version）。
    pub schema_version: i64,
    /// 本次打开已应用的迁移（"vN: name"）。
    pub applied: Vec<String>,
    /// 未应用的迁移（version > schema_version）。
    pub pending: Vec<String>,
    /// 迁移失败后降级只读（拒绝写入，数据不损坏）。
    pub read_only: bool,
    /// 迁移失败原因（read_only 时的提示）。
    pub last_error: Option<String>,
}

pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
    status: MigrationStatus,
}

/// 审计查询条件（v0.5 生产加固：分页 + 精确过滤 + 模糊搜索）。
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// 返回条数；0 时取默认 100，上限 1000。
    pub limit: usize,
    pub offset: usize,
    pub event: Option<String>,
    pub tool: Option<String>,
    pub approved: Option<bool>,
    /// 对 detail/event/tool/session_id 做 LIKE 模糊搜索（%/_ 自动转义）。
    pub q: Option<String>,
}

fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 基表结构（CREATE TABLE IF NOT EXISTS，幂等）。
fn base_schema() -> &'static str {
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
         redo_json TEXT NOT NULL,
         message_redo_json TEXT NOT NULL DEFAULT '[]',
         title TEXT,
         archived INTEGER NOT NULL DEFAULT 0,
         pinned INTEGER NOT NULL DEFAULT 0
     );
     CREATE TABLE IF NOT EXISTS audit (
         id INTEGER PRIMARY KEY AUTOINCREMENT,
         ts TEXT NOT NULL,
         session_id TEXT NOT NULL,
         event TEXT NOT NULL,
         tool TEXT,
         approved INTEGER,
         detail TEXT NOT NULL
     );"
}

fn user_version(conn: &Connection) -> Result<i64, AgentError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sqlite_error)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, AgentError> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlite_error)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?
        .filter_map(Result::ok)
        .collect();
    Ok(columns)
}

/// 顺序执行 version > user_version 的迁移；每条在独立事务内提交并推进 user_version。
fn run_migrations(
    conn: &mut Connection,
    migrations: &[Migration],
) -> Result<Vec<String>, AgentError> {
    let mut applied = Vec::new();
    let mut current = user_version(conn)?;
    for migration in migrations {
        if migration.version <= current {
            continue;
        }
        let transaction = conn.transaction().map_err(sqlite_error)?;
        (migration.run)(&transaction)?;
        transaction
            .pragma_update(None, "user_version", migration.version)
            .map_err(sqlite_error)?;
        transaction.commit().map_err(sqlite_error)?;
        applied.push(format!("v{}: {}", migration.version, migration.name));
        current = migration.version;
    }
    Ok(applied)
}

impl SqliteSessionStore {
    pub fn open(path: &Path) -> Result<Self, AgentError> {
        Self::open_with_migrations(path, MIGRATIONS)
    }

    /// 打开并自动迁移（MIGRATIONS 或测试注入的迁移表）；迁移失败降级只读。
    fn open_with_migrations(path: &Path, migrations: &[Migration]) -> Result<Self, AgentError> {
        let mut conn = Connection::open(path).map_err(sqlite_error)?;
        conn.execute_batch(base_schema()).map_err(sqlite_error)?;
        let mut status = MigrationStatus {
            schema_version: user_version(&conn)?,
            ..MigrationStatus::default()
        };
        match run_migrations(&mut conn, migrations) {
            Ok(applied) => {
                status.applied = applied;
                status.schema_version = user_version(&conn)?;
            }
            Err(error) => {
                // 迁移失败：降级只读并提示，拒绝继续写入（数据保持原状）。
                let read_only_conn =
                    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .map_err(sqlite_error)?;
                status.read_only = true;
                status.last_error = Some(error.to_string());
                return Ok(Self {
                    conn: Mutex::new(read_only_conn),
                    status,
                });
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
            status,
        })
    }

    /// 迁移状态（含未应用清单；供 /server/status 与面板展示）。
    pub fn migration_status(&self) -> MigrationStatus {
        let mut status = self.status.clone();
        status.pending = MIGRATIONS
            .iter()
            .filter(|migration| migration.version > status.schema_version)
            .map(|migration| format!("v{}: {}", migration.version, migration.name))
            .collect();
        status
    }

    pub fn is_read_only(&self) -> bool {
        self.status.read_only
    }

    /// 清空会话与审计，返回 (会话数, 审计数)。
    pub fn clear_all(&self) -> Result<(usize, usize), AgentError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| AgentError::Session("SQLite 锁中毒".into()))?;
        let sessions = conn
            .execute("DELETE FROM sessions", [])
            .map_err(sqlite_error)?;
        let audit = conn
            .execute("DELETE FROM audit", [])
            .map_err(sqlite_error)?;
        Ok((sessions, audit))
    }

    /// PRAGMA integrity_check 结果（"ok" 表示完整）。
    pub fn integrity_check(&self) -> Result<String, AgentError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| AgentError::Session("SQLite 锁中毒".into()))?;
        conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(sqlite_error)
    }

    /// 当前 (会话数, 审计数)。
    pub fn counts(&self) -> (usize, usize) {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return (0, 0),
        };
        let sessions = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or_default();
        let audit = conn
            .query_row("SELECT COUNT(*) FROM audit", [], |row| row.get::<_, i64>(0))
            .unwrap_or_default();
        (sessions as usize, audit as usize)
    }

    fn save_locked(conn: &Connection, session: &Session) -> Result<(), AgentError> {
        conn.execute(
            "INSERT INTO sessions (
                 id, workspace, model, system_prompt, messages_json, snapshots_json,
                 created_at, updated_at, parent_id, fork_point, redo_json, message_redo_json,
                 title, archived, pinned
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
                 workspace=excluded.workspace,
                 model=excluded.model,
                 system_prompt=excluded.system_prompt,
                 messages_json=excluded.messages_json,
                 snapshots_json=excluded.snapshots_json,
                 updated_at=excluded.updated_at,
                 parent_id=excluded.parent_id,
                 fork_point=excluded.fork_point,
                 redo_json=excluded.redo_json,
                 message_redo_json=excluded.message_redo_json,
                 title=excluded.title,
                 archived=excluded.archived,
                 pinned=excluded.pinned",
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
                serde_json::to_string(&session.message_redo_stack).map_err(json_error)?,
                session.title,
                i64::from(session.archived),
                i64::from(session.pinned),
            ],
        )
        .map_err(sqlite_error)?;
        Ok(())
    }

    fn load_locked(conn: &Connection, id: &str) -> Result<Session, AgentError> {
        let row = conn
            .query_row(
                "SELECT id, workspace, model, system_prompt, messages_json, snapshots_json,
                        created_at, updated_at, parent_id, fork_point, redo_json, message_redo_json,
                        title, archived, pinned
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
                        row.get::<_, String>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, bool>(13)?,
                        row.get::<_, bool>(14)?,
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
            message_redo_stack: serde_json::from_str(&row.11).map_err(json_error)?,
            title: row.12,
            archived: row.13,
            pinned: row.14,
        })
    }
}

impl SessionStore for SqliteSessionStore {
    fn clear(&self) -> Result<(), AgentError> {
        self.clear_all().map(|_| ())
    }

    fn is_read_only(&self) -> bool {
        self.is_read_only()
    }

    fn migration_warning(&self) -> Option<String> {
        let status = self.migration_status();
        if status.read_only {
            return Some(
                status
                    .last_error
                    .unwrap_or_else(|| "迁移失败，已降级只读".into()),
            );
        }
        if !status.pending.is_empty() {
            return Some(format!(
                "存在未应用迁移：{}（重启服务自动应用）",
                status.pending.join("；")
            ));
        }
        None
    }
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

    fn append_audit(&self, entries: &[AuditEntry]) -> Result<(), AgentError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| AgentError::Session("SQLite 锁中毒".into()))?;
        let transaction = conn.transaction().map_err(sqlite_error)?;
        for entry in entries {
            transaction
                .execute(
                    "INSERT INTO audit (ts, session_id, event, tool, approved, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        entry.ts,
                        entry.session_id,
                        entry.event,
                        entry.tool,
                        entry.approved.map(|approved| approved as i64),
                        entry.detail,
                    ],
                )
                .map_err(sqlite_error)?;
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    fn recent_audit(&self, limit: usize) -> Vec<AuditEntry> {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(_) => return Vec::new(),
        };
        let mut statement = match conn.prepare(
            "SELECT ts, session_id, event, tool, approved, detail
             FROM audit ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = match statement.query_map([limit as i64], |row| {
            Ok(AuditEntry {
                ts: row.get(0)?,
                session_id: row.get(1)?,
                event: row.get(2)?,
                tool: row.get(3)?,
                approved: row.get::<_, Option<i64>>(4)?.map(|value| value != 0),
                detail: row.get(5)?,
            })
        }) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(Result::ok).collect()
    }

    /// 分页查询审计：返回 (当前页条目, 满足条件总数)。
    fn query_audit(&self, query: &AuditQuery) -> (Vec<AuditEntry>, usize) {
        let conn = match self.conn.lock() {
            Ok(conn) => conn,
            Err(error) => {
                eprintln!("AUDIT LOCK ERROR: {error}");
                return (Vec::new(), 0);
            }
        };
        let mut conditions: Vec<String> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(event) = &query.event {
            conditions.push("event = ?".to_string());
            args.push(event.clone().into());
        }
        if let Some(tool) = &query.tool {
            conditions.push("tool = ?".to_string());
            args.push(tool.clone().into());
        }
        if let Some(approved) = query.approved {
            conditions.push("approved = ?".to_string());
            args.push(i64::from(approved).into());
        }
        if let Some(q) = &query.q {
            let pattern = format!("%{}%", like_escape(q));
            conditions.push(
                "(detail LIKE ? ESCAPE '\\' OR event LIKE ? ESCAPE '\\' \
                 OR tool LIKE ? ESCAPE '\\' OR session_id LIKE ? ESCAPE '\\')"
                    .to_string(),
            );
            for _ in 0..4 {
                args.push(pattern.clone().into());
            }
        }
        let where_sql = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let total_sql = format!("SELECT COUNT(*) FROM audit{where_sql}");
        let total = conn
            .query_row(&total_sql, rusqlite::params_from_iter(args.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or_default();
        let limit = if query.limit == 0 {
            100
        } else {
            query.limit.min(1000)
        };
        args.push((limit as i64).into());
        args.push((query.offset as i64).into());
        let sql = format!(
            "SELECT ts, session_id, event, tool, approved, detail
             FROM audit{where_sql} ORDER BY id DESC LIMIT ? OFFSET ?"
        );
        let mut statement = match conn.prepare(&sql) {
            Ok(statement) => statement,
            Err(_) => return (Vec::new(), total as usize),
        };
        let rows = match statement.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            Ok(AuditEntry {
                ts: row.get(0)?,
                session_id: row.get(1)?,
                event: row.get(2)?,
                tool: row.get(3)?,
                approved: row.get::<_, Option<i64>>(4)?.map(|value| value != 0),
                detail: row.get(5)?,
            })
        }) {
            Ok(rows) => rows,
            Err(_) => return (Vec::new(), total as usize),
        };
        (rows.filter_map(Result::ok).collect(), total as usize)
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
    use rusqlite::Connection;

    #[test]
    fn sqlite_session_round_trip_and_list() {
        let path =
            std::env::temp_dir().join(format!("owo-sqlite-test-{}.db", uuid::Uuid::new_v4()));
        let store = SqliteSessionStore::open(&path).unwrap();
        let mut session = store.create(Path::new("."), "mock", None).unwrap();
        session.push(ChatMessage::user("你好".to_string()));
        session.push(ChatMessage::assistant_text("收到".to_string()));
        session.rename("SQLite 会话".to_string());
        session.set_pinned(true);
        session.set_archived(true);
        store.save(&session).unwrap();
        let child = session.fork(1);
        store.save(&child).unwrap();

        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].content.as_deref(), Some("你好"));
        assert_eq!(loaded.title.as_deref(), Some("SQLite 会话"));
        assert!(loaded.pinned);
        assert!(loaded.archived);
        let loaded_child = store.load(&child.id).unwrap();
        assert_eq!(loaded_child.parent_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(loaded_child.fork_point, Some(1));
        assert!(loaded_child.title.is_none());
        assert!(!loaded_child.pinned);
        assert!(!loaded_child.archived);
        assert_eq!(store.list().len(), 2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn migrates_legacy_database_without_message_redo_column() {
        let path =
            std::env::temp_dir().join(format!("owo-sqlite-legacy-{}.db", uuid::Uuid::new_v4()));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
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
             );
             INSERT INTO sessions VALUES (
                 'legacy', '.', 'mock', NULL, '[]', '{}',
                 '2026-08-11T00:00:00Z', '2026-08-11T00:00:00Z', NULL, NULL, '[]'
             );",
        )
        .unwrap();
        drop(conn);

        let store = SqliteSessionStore::open(&path).unwrap();
        let loaded = store.load("legacy").unwrap();
        assert!(loaded.message_redo_stack.is_empty());
        assert_eq!(loaded.model, "mock");
        assert!(loaded.title.is_none());
        assert!(!loaded.pinned);
        assert!(!loaded.archived);
        let mut session = store.create(Path::new("."), "mock", None).unwrap();
        session.rename("迁移后新会话".to_string());
        store.save(&session).unwrap();
        assert_eq!(
            store.load(&session.id).unwrap().title.as_deref(),
            Some("迁移后新会话")
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn appends_audit_entries_to_sqlite() {
        let path =
            std::env::temp_dir().join(format!("owo-sqlite-audit-{}.db", uuid::Uuid::new_v4()));
        let store = SqliteSessionStore::open(&path).unwrap();
        let entry = AuditEntry {
            ts: "2026-08-11T00:00:00Z".to_string(),
            session_id: "s1".to_string(),
            event: "tool_call".to_string(),
            tool: Some("read_file".to_string()),
            approved: None,
            detail: "ok".to_string(),
        };
        store.append_audit(&[entry]).unwrap();
        let recent = store.recent_audit(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].event, "tool_call");
        assert_eq!(recent[0].session_id, "s1");
        assert_eq!(recent[0].approved, None);
        let conn = Connection::open(&path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn migrations_advance_user_version_and_are_idempotent() {
        let path =
            std::env::temp_dir().join(format!("owo-sqlite-migrate-{}.db", uuid::Uuid::new_v4()));
        let store = SqliteSessionStore::open(&path).unwrap();
        assert_eq!(store.migration_status().schema_version, 1);
        assert!(store.migration_status().pending.is_empty());
        assert!(!store.is_read_only());
        drop(store);
        // 再次打开：无新迁移应用，schema_version 保持。
        let reopened = SqliteSessionStore::open(&path).unwrap();
        assert_eq!(reopened.migration_status().schema_version, 1);
        assert!(reopened.migration_status().applied.is_empty());
        assert!(!reopened.is_read_only());
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn legacy_schema_gets_migrated_by_v1() {
        // 旧库（user_version=0、缺 4 列）由注册迁移 v1 补齐，而不是隐式 ALTER。
        let path = std::env::temp_dir().join(format!("owo-sqlite-v1-{}.db", uuid::Uuid::new_v4()));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
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
        .unwrap();
        drop(conn);
        let store = SqliteSessionStore::open(&path).unwrap();
        assert!(!store.is_read_only());
        assert_eq!(
            store.migration_status().applied,
            vec!["v1: sessions 列补齐（message_redo_json/title/archived/pinned）"]
        );
        let mut session = store.create(Path::new("."), "mock", None).unwrap();
        session.rename("v1 迁移会话".to_string());
        store.save(&session).unwrap();
        assert_eq!(
            store.load(&session.id).unwrap().title.as_deref(),
            Some("v1 迁移会话")
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn migration_failure_degrades_to_read_only_with_hint() {
        let path = std::env::temp_dir().join(format!("owo-sqlite-ro-{}.db", uuid::Uuid::new_v4()));
        let failing = Migration {
            version: 99,
            name: "boom",
            run: |_| Err(AgentError::Session("boom".into())),
        };
        let store = SqliteSessionStore::open_with_migrations(&path, &[failing]).unwrap();
        assert!(store.is_read_only(), "迁移失败必须降级只读，不得静默写入");
        let status = store.migration_status();
        assert!(status.last_error.is_some(), "降级必须携带原因提示");
        assert!(
            store.create(Path::new("."), "mock", None).is_err(),
            "只读库必须拒绝写入"
        );
        assert_eq!(store.list().len(), 0, "读取仍可用");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn clear_wipes_sessions_and_audit_and_integrity_ok() {
        let path =
            std::env::temp_dir().join(format!("owo-sqlite-clear-{}.db", uuid::Uuid::new_v4()));
        let store = SqliteSessionStore::open(&path).unwrap();
        store.create(Path::new("."), "mock", None).unwrap();
        let entry = AuditEntry {
            ts: "2026-08-11T00:00:00Z".to_string(),
            session_id: "s1".to_string(),
            event: "test".to_string(),
            tool: None,
            approved: None,
            detail: "d".to_string(),
        };
        store.append_audit(&[entry]).unwrap();
        assert_eq!(store.counts(), (1, 1));
        assert_eq!(store.integrity_check().unwrap(), "ok");
        assert_eq!(store.clear_all().unwrap(), (1, 1));
        assert_eq!(store.counts(), (0, 0));
        assert_eq!(
            store.integrity_check().unwrap(),
            "ok",
            "清空后完整性校验必须通过"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
