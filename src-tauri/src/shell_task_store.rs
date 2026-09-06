//! Versioned, exclusively owned shell-task tombstones. This store never starts a process.

use std::{fs::{File, OpenOptions}, path::Path, sync::Mutex};

use fs2::FileExt;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::shell_tasks::ShellTaskSnapshot;

pub(crate) const DATABASE_FILE_NAME: &str = "shell-tasks.v1.sqlite3";
const STORE_VERSION: i32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ShellTaskRecord {
    pub snapshot: ShellTaskSnapshot,
    pub output: String,
    pub dropped_head_bytes: u64,
    pub sequence: u64,
}

pub(crate) struct ShellTaskStore {
    connection: Mutex<Connection>,
    // An OS lock, not a PID heuristic: it is released by process death and
    // prevents a second live host from falsely recovering the first one's tasks.
    _authority: File,
}

impl ShellTaskStore {
    pub(crate) fn open(directory: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(directory).map_err(|error| format!("无法创建命令历史目录：{error}"))?;
        let authority = OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(directory.join("shell-tasks.v1.lock"))
            .map_err(|error| format!("无法打开命令历史锁：{error}"))?;
        authority.try_lock_exclusive()
            .map_err(|error| format!("另一个宿主仍持有命令历史，拒绝恢复活任务：{error}"))?;
        let mut connection = Connection::open(directory.join(DATABASE_FILE_NAME))
            .map_err(|error| format!("无法打开命令历史：{error}"))?;
        connection.busy_timeout(std::time::Duration::from_secs(5)).map_err(|error| error.to_string())?;
        connection.pragma_update(None, "journal_mode", "WAL").map_err(|error| error.to_string())?;
        connection.pragma_update(None, "synchronous", "FULL").map_err(|error| error.to_string())?;
        let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| format!("无法读取命令历史版本：{error}"))?;
        if version == 0 {
            let tx = connection.transaction().map_err(|error| error.to_string())?;
            tx.execute_batch("CREATE TABLE shell_task (id TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;")
                .map_err(|error| format!("无法创建命令历史：{error}"))?;
            tx.pragma_update(None, "user_version", STORE_VERSION).map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())?;
        } else if version != STORE_VERSION {
            return Err(format!("不支持的命令历史版本 {version}，未清空历史"));
        }
        Ok(Self { connection: Mutex::new(connection), _authority: authority })
    }

    pub(crate) fn load(&self) -> Result<Vec<ShellTaskRecord>, String> {
        let connection = self.connection.lock().unwrap_or_else(|error| error.into_inner());
        let mut statement = connection.prepare("SELECT id, data FROM shell_task")
            .map_err(|error| format!("无法查询命令历史：{error}"))?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|error| error.to_string())?;
        let mut records = Vec::new();
        for row in rows {
            let (id, data) = row.map_err(|error| error.to_string())?;
            let record: ShellTaskRecord = serde_json::from_str(&data)
                .map_err(|error| format!("命令历史 {id} 损坏，未丢弃记录：{error}"))?;
            if id != record.snapshot.shell_task_id || record.snapshot.conversation_id.is_empty()
                || record.output.len() > 256 * 1024 || record.snapshot.command.len() > 4096 {
                return Err(format!("命令历史 {id} 的身份或大小无效，未丢弃记录"));
            }
            records.push(record);
        }
        records.sort_by_key(|record| record.sequence);
        Ok(records)
    }

    /// A single transaction covers additions, terminal outcomes, forks, and
    /// removals, so retention/deletion cannot resurrect an independently saved row.
    pub(crate) fn replace(&self, records: &[ShellTaskRecord]) -> Result<(), String> {
        let mut connection = self.connection.lock().unwrap_or_else(|error| error.into_inner());
        let tx = connection.transaction().map_err(|error| error.to_string())?;
        tx.execute("DELETE FROM shell_task", []).map_err(|error| error.to_string())?;
        for record in records {
            let data = serde_json::to_string(record).map_err(|error| error.to_string())?;
            tx.execute("INSERT INTO shell_task (id, data) VALUES (?1, ?2)",
                rusqlite::params![record.snapshot.shell_task_id, data])
                .map_err(|error| format!("命令历史落盘失败：{error}"))?;
        }
        tx.commit().map_err(|error| format!("命令历史提交失败：{error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_store_blocks_another_host_and_releases_authority_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let first = ShellTaskStore::open(directory.path()).unwrap();
        assert!(ShellTaskStore::open(directory.path()).err().unwrap().contains("仍持有"));
        drop(first);
        ShellTaskStore::open(directory.path()).unwrap();
    }

    #[test]
    fn malformed_history_is_reported_and_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let store = ShellTaskStore::open(directory.path()).unwrap();
        let connection = Connection::open(directory.path().join(DATABASE_FILE_NAME)).unwrap();
        connection.execute("INSERT INTO shell_task VALUES ('broken', '{')", []).unwrap();
        assert!(store.load().unwrap_err().contains("损坏"));
        let count: i64 = connection.query_row("SELECT COUNT(*) FROM shell_task", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }
}
