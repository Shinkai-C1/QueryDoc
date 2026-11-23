//! SQLite 连接管理与建表迁移（rusqlite bundled，库文件在 app_data_dir/querydoc.db）。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::error::AppError;

/// 迁移清单：按版本号顺序应用，`schema_migrations` 表记录已应用版本。
const MIGRATIONS: &[(u32, &str)] = &[
    (1, INIT_SQL),
    (2, MIGRATION_V2),
    (3, MIGRATION_V3),
    (4, MIGRATION_V4),
    (5, MIGRATION_V5),
    (6, MIGRATION_V6),
    (7, MIGRATION_V7),
    (8, MIGRATION_V8),
    (9, MIGRATION_V9),
    (10, MIGRATION_V10),
    (11, MIGRATION_V11),
    (12, MIGRATION_V12),
    (13, MIGRATION_V13),
];

/// v2：extract_results 增加 error_msg（文件级提取失败也落行，
/// 供 queue_status 的 extract_error 与失败重提判断）。
const MIGRATION_V2: &str = r"
ALTER TABLE extract_results ADD COLUMN error_msg TEXT NOT NULL DEFAULT '';
";

/// v3：projects 增加 group_name（所属项目组）；确认旧数据不再保留，
/// 按依赖顺序清空全部项目相关表（磁盘目录不动，由用户自行清理）。
const MIGRATION_V3: &str = r"
ALTER TABLE projects ADD COLUMN group_name TEXT NOT NULL DEFAULT '';
DELETE FROM export_tasks;
DELETE FROM extract_configs;
DELETE FROM extract_results;
DELETE FROM parse_results;
DELETE FROM source_files;
DELETE FROM upload_batches;
DELETE FROM projects;
";

/// v4：settings 删除 max_retries（提取次数已固定 1 次，开发遗留字段清理）。
const MIGRATION_V4: &str = r"
ALTER TABLE settings DROP COLUMN max_retries;
";

/// v5：settings 重建为按 provider 分行（ollama / openai_compatible 各存一份
/// 完整配置，is_active 标记当前使用的 provider）；旧单例行迁入其 provider。
const MIGRATION_V5: &str = r"
CREATE TABLE settings_new (
    provider TEXT PRIMARY KEY,
    base_url TEXT NOT NULL DEFAULT '',
    api_key TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    timeout INTEGER NOT NULL DEFAULT 300,
    temperature REAL NOT NULL DEFAULT 0.0,
    is_active INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT ''
);
INSERT INTO settings_new(provider, base_url, api_key, model, timeout, temperature,
                        is_active, updated_at)
    SELECT llm_provider, base_url, api_key, model, timeout, temperature, 1, updated_at
    FROM settings;
DROP TABLE settings;
ALTER TABLE settings_new RENAME TO settings;
";

/// v6：温度固定 1.0（不再提供设置项；Kimi 思考模型等仅允许 1），
/// 存量库统一改写。
const MIGRATION_V6: &str = r"
UPDATE settings SET temperature = 1.0;
";

/// v7：source_files 增加 estimated_tokens（进度面板「预估Token」列的持久化；
/// NULL=未估算/上次估算失败）。随 source_files 行删除连带清除，无需级联代码。
const MIGRATION_V7: &str = r"
ALTER TABLE source_files ADD COLUMN estimated_tokens INTEGER;
";

/// v8：清理写而不读的冗余列（温度已固定 1.0；row_id 无任何消费方，行唯一性由
/// UNIQUE(project_id, source_file_id) 兜底；started_at/completed_at/error_msg
/// 的诊断信息任务日志已覆盖）。extract_results 因表级 UNIQUE 含 row_id 需重建，
/// 数据按列搬迁保留；其余直接 DROP COLUMN。
const MIGRATION_V8: &str = r"
ALTER TABLE settings DROP COLUMN temperature;
ALTER TABLE parse_results DROP COLUMN started_at;
ALTER TABLE parse_results DROP COLUMN completed_at;
ALTER TABLE export_tasks DROP COLUMN completed_at;
ALTER TABLE export_tasks DROP COLUMN error_msg;
CREATE TABLE extract_results_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    source_file_id INTEGER NOT NULL REFERENCES source_files(id),
    cell_data TEXT NOT NULL DEFAULT '{}',
    is_edited INTEGER NOT NULL DEFAULT 0,
    error_msg TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, source_file_id)
);
INSERT INTO extract_results_new(id, project_id, source_file_id, cell_data, is_edited,
                                error_msg, created_at, updated_at)
    SELECT id, project_id, source_file_id, cell_data, is_edited,
           error_msg, created_at, updated_at FROM extract_results;
DROP TABLE extract_results;
ALTER TABLE extract_results_new RENAME TO extract_results;
CREATE INDEX IF NOT EXISTS idx_extract_results_source_file
    ON extract_results(source_file_id);
";

/// v9：OpenAI 兼容 API 全面对接 Kimi 后统一更名：provider 键
/// openai_compatible → kimi（前后端展示均为 KIMI-API；存量配置行改写保留）。
const MIGRATION_V9: &str = r"
UPDATE settings SET provider = 'kimi' WHERE provider = 'openai_compatible';
";

/// v10：内控测试模块数据底座（纯追加：projects 加 task_type 列 + ic_* 新表，
/// 不动 v1–v9 任何既有表/索引/数据；旧库升级后既有项目自动为 task_type='parse'）。
const MIGRATION_V10: &str = r"
ALTER TABLE projects ADD COLUMN task_type TEXT NOT NULL DEFAULT 'parse'; -- 'parse'|'audit'

-- 制度文件（导入的 pdf，按源文件夹相对路径组织树）
CREATE TABLE ic_documents(
  id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  name TEXT NOT NULL, rel_path TEXT NOT NULL,  -- 相对导入文件夹的路径，用于树层级
  file_path TEXT NOT NULL,                     -- 项目目录内落盘路径
  created_at TEXT NOT NULL DEFAULT (datetime('now')));

-- 流程节点；code 全局唯一（前缀+序号），删除即废弃不重用
CREATE TABLE ic_nodes(
  id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  code TEXT NOT NULL,                -- ACT/EVT/ROL/DAT/LOG + 序号
  node_type TEXT NOT NULL,           -- ACT|EVT|ROL|DAT|LOG
  subtype TEXT,                      -- EVT: time|message; ROL: person|dept|it; DAT: electronic|paper|record; LOG: and|or|not
  name TEXT NOT NULL DEFAULT '', content TEXT NOT NULL DEFAULT '',
  doc_id INTEGER, page INTEGER, rect TEXT,   -- 来源 pdf 与 0-1000 归一化矩形坐标(json)
  canvas_x REAL NOT NULL DEFAULT 0, canvas_y REAL NOT NULL DEFAULT 0,
  group_id INTEGER,                  -- 所属环境组
  props TEXT NOT NULL DEFAULT '{}',  -- ACT 扩展属性 json（控制性质/预防检查/频率/关键控制/特别风险）
  created_at TEXT NOT NULL DEFAULT (datetime('now')));

CREATE TABLE ic_edges(
  id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  from_node INTEGER NOT NULL, to_node INTEGER NOT NULL,
  edge_kind TEXT NOT NULL DEFAULT 'flow');  -- flow(上下流)|role(角色)|data(数据)

CREATE TABLE ic_groups(
  id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  env_type TEXT NOT NULL,            -- it(IT环境)|manual(人工环境)
  rect TEXT NOT NULL);               -- 画布包围矩形 json

-- 制度索引：源 pdf 矩形 → 目标 pdf 文件
CREATE TABLE ic_doc_links(
  id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  src_doc_id INTEGER NOT NULL, page INTEGER NOT NULL, rect TEXT NOT NULL,
  target_doc_id INTEGER NOT NULL);

CREATE TABLE ic_roster(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  name TEXT, dept TEXT, position TEXT, duty TEXT, superior TEXT, subordinate TEXT, remark TEXT);
CREATE TABLE ic_org(id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL,
  dept TEXT, duty TEXT, superior TEXT, subordinate TEXT, remark TEXT);

-- 编号计数器：保证全局唯一且删除不复用
CREATE TABLE ic_counters(project_id INTEGER NOT NULL, prefix TEXT NOT NULL,
  next_val INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(project_id, prefix));
";

/// v11：制度文件改为直接入库（ic_documents.data BLOB 存 pdf 字节，size 记字节数；
/// 内控任务不再生成本地文件夹）。旧行 data 为 NULL，读取时回落 file_path 磁盘路径。
const MIGRATION_V11: &str = r"
ALTER TABLE ic_documents ADD COLUMN data BLOB;
ALTER TABLE ic_documents ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
";

/// v12：ic_nodes 增加画布尺寸列（用户拖边框改节点大小后持久化；0=内容自适应）。
const MIGRATION_V12: &str = r"
ALTER TABLE ic_nodes ADD COLUMN canvas_w REAL NOT NULL DEFAULT 0;
ALTER TABLE ic_nodes ADD COLUMN canvas_h REAL NOT NULL DEFAULT 0;
";

/// v13：extract_configs 增加关键词定义与信息类型（Instructor 字段描述理念：
/// 选填提示注入 LLM prompt/schema 提高提取准确度；空串=未设置，行为同旧版）。
const MIGRATION_V13: &str = r"
ALTER TABLE extract_configs ADD COLUMN definition TEXT NOT NULL DEFAULT '';
ALTER TABLE extract_configs ADD COLUMN info_type TEXT NOT NULL DEFAULT '';
";

const INIT_SQL: &str = r"
CREATE TABLE IF NOT EXISTS projects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    base_path TEXT NOT NULL,
    project_dir TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'idle'          -- idle/parsing/extracting/error
);

CREATE TABLE IF NOT EXISTS upload_batches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    folder_name TEXT NOT NULL,
    uploaded_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_upload_batches_project ON upload_batches(project_id);

CREATE TABLE IF NOT EXISTS source_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id INTEGER NOT NULL REFERENCES upload_batches(id),
    original_name TEXT NOT NULL,
    stored_name TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',      -- pending/parsing/done/error
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_source_files_batch ON source_files(batch_id);
CREATE INDEX IF NOT EXISTS idx_source_files_status ON source_files(status);

CREATE TABLE IF NOT EXISTS parse_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_file_id INTEGER NOT NULL UNIQUE REFERENCES source_files(id),
    output_dir TEXT NOT NULL DEFAULT '',
    json_path TEXT NOT NULL DEFAULT '',
    page_count INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending',      -- pending/running/done/error
    error_msg TEXT NOT NULL DEFAULT '',
    started_at TEXT,
    completed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_parse_results_status ON parse_results(status);

CREATE TABLE IF NOT EXISTS extract_configs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    column_index INTEGER NOT NULL,
    keyword TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(project_id, column_index)
);
CREATE INDEX IF NOT EXISTS idx_extract_configs_project ON extract_configs(project_id);

CREATE TABLE IF NOT EXISTS extract_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    row_id TEXT NOT NULL,
    source_file_id INTEGER NOT NULL REFERENCES source_files(id),
    cell_data TEXT NOT NULL DEFAULT '{}',
    is_edited INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, row_id),
    UNIQUE(project_id, source_file_id)
);
CREATE INDEX IF NOT EXISTS idx_extract_results_row ON extract_results(row_id);
CREATE INDEX IF NOT EXISTS idx_extract_results_source_file ON extract_results(source_file_id);

CREATE TABLE IF NOT EXISTS export_tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    kind TEXT NOT NULL,                          -- table/full
    status TEXT NOT NULL DEFAULT 'pending',      -- pending/running/done/error
    output_path TEXT NOT NULL DEFAULT '',
    error_msg TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_export_tasks_project ON export_tasks(project_id);

CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),       -- 全局单例
    llm_provider TEXT NOT NULL DEFAULT 'ollama', -- ollama/openai_compatible
    base_url TEXT NOT NULL DEFAULT 'http://localhost:11434',
    api_key TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    timeout INTEGER NOT NULL DEFAULT 300,
    max_retries INTEGER NOT NULL DEFAULT 2,
    temperature REAL NOT NULL DEFAULT 0.0,
    updated_at TEXT NOT NULL DEFAULT ''
);
";

/// 线程共享的数据库句柄（单连接 + Mutex；桌面单用户场景足够）。
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// 打开（必要时创建）指定路径的库文件并应用迁移；父目录自动创建。
    pub fn open(path: &Path) -> Result<Self, AppError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// 内存库，供单元测试使用。
    pub fn open_in_memory() -> Result<Self, AppError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, AppError> {
        conn.pragma_update(None, "foreign_keys", true)?;
        let db = Db {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// 获取连接锁。Mutex 中毒说明持锁线程已 panic，库状态不可信。
    pub fn conn(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.conn
            .lock()
            .map_err(|_| AppError::Internal("数据库锁已中毒".to_string()))
    }

    /// 应用未执行过的迁移（可重复调用，幂等）。
    fn migrate(&self) -> Result<(), AppError> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );",
        )?;
        let applied: u32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        for (version, sql) in MIGRATIONS {
            if *version > applied {
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(sql)?;
                tx.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
                    rusqlite::params![version, crate::paths::now_display()],
                )?;
                tx.commit()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_TABLES: [&str; 17] = [
        "schema_migrations",
        "projects",
        "upload_batches",
        "source_files",
        "parse_results",
        "extract_configs",
        "extract_results",
        "export_tasks",
        "settings",
        "ic_documents",
        "ic_nodes",
        "ic_edges",
        "ic_groups",
        "ic_doc_links",
        "ic_roster",
        "ic_org",
        "ic_counters",
    ];

    fn table_names(db: &Db) -> Vec<String> {
        let conn = db.conn().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    }

    #[test]
    fn migrate_creates_all_tables() {
        let db = Db::open_in_memory().unwrap();
        let names = table_names(&db);
        for t in EXPECTED_TABLES {
            assert!(names.contains(&t.to_string()), "缺表 {t}");
        }
        // 迁移版本已记录
        let version: u32 = db
            .conn()
            .unwrap()
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as u32);
    }

    #[test]
    fn migration_v2_adds_error_msg_column_on_old_db() {
        // 模拟 Phase 2 旧库：只有 v1 表结构（extract_results 无 error_msg）
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (1, 'x')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v2：error_msg 列出现（缺列时此查询直接报错）
        let db = Db::open(&path).unwrap();
        let n: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM extract_results WHERE error_msg = ''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let version: u32 = db
            .conn()
            .unwrap()
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        // 重开后会补齐全部后续迁移
        assert_eq!(version, MIGRATIONS.len() as u32);
    }

    #[test]
    fn migration_v3_adds_group_name_and_purges_old_rows() {
        // 模拟 v2 旧库：projects 无 group_name，且有遗留项目数据
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (2, 'x')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('旧项目', '2026-01-01 00:00:00', '/b', 'c', 'idle')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v3：group_name 列出现、旧数据被清空
        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE group_name = ''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "旧数据应已清空（缺 group_name 列时此查询直接报错）");
        let version: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as u32);
    }

    #[test]
    fn migration_v4_drops_settings_max_retries() {
        // 模拟 v3 旧库：settings 表仍有 max_retries 列且有数据
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute_batch(MIGRATION_V3).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (3, 'x')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO settings(id, llm_provider, max_retries) VALUES (1, 'ollama', 2)",
                [],
            )
            .unwrap();
        }
        // 重新打开：v4 删除该列、v5 重建为按 provider 分行，读取设置正常
        let db = Db::open(&path).unwrap();
        let s = crate::settings::get_settings(&db).unwrap();
        assert_eq!(s.active_provider, "ollama");
        let err = db
            .conn()
            .unwrap()
            .query_row("SELECT max_retries FROM settings", [], |_| Ok(()));
        assert!(err.is_err(), "max_retries 列应已删除");
    }

    #[test]
    fn migration_v7_adds_estimated_tokens_column() {
        // 模拟 v6 旧库：source_files 无 estimated_tokens 列且有数据
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            for (v, sql) in MIGRATIONS.iter().take(6) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('旧项目', '2026-01-01 00:00:00', '/b', 'c', 'idle')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO upload_batches(project_id, folder_name, uploaded_at)
                 VALUES (1, 'b', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO source_files(batch_id, original_name, stored_name, relative_path,
                                        file_size, status, created_at)
                 VALUES (1, 'a.pdf', 'a.pdf', 'x/a.pdf', 10, 'pending', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v7：estimated_tokens 列出现且存量行默认为 NULL
        let db = Db::open(&path).unwrap();
        let v: Option<i64> = db
            .conn()
            .unwrap()
            .query_row("SELECT estimated_tokens FROM source_files", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            v, None,
            "存量行 estimated_tokens 应为 NULL（缺列时此查询直接报错）"
        );
    }

    #[test]
    fn migration_v13_adds_definition_and_info_type_on_old_db() {
        // 模拟 v12 旧库：extract_configs 无 definition/info_type 且有配置数据
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            for (v, sql) in MIGRATIONS.iter().take(12) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('旧项目', '2026-01-01 00:00:00', '/b', 'c', 'idle')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO extract_configs(project_id, column_index, keyword, created_at)
                 VALUES (1, 2, '金额', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v13：两列出现且存量行默认为空串（行为同旧版）
        let db = Db::open(&path).unwrap();
        let (def, ty): (String, String) = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT definition, info_type FROM extract_configs WHERE column_index = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (def.as_str(), ty.as_str()),
            ("", ""),
            "存量行 definition/info_type 应为空串（缺列时此查询直接报错）"
        );
    }

    #[test]
    fn migration_v8_drops_redundant_columns_keeps_data() {
        // 模拟 v7 旧库：含全部冗余列且有提取结果数据
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            for (v, sql) in MIGRATIONS.iter().take(7) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('旧项目', '2026-01-01 00:00:00', '/b', 'c', 'idle')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO upload_batches(project_id, folder_name, uploaded_at)
                 VALUES (1, 'b', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO source_files(batch_id, original_name, stored_name, relative_path,
                                        file_size, status, created_at)
                 VALUES (1, 'a.pdf', 'a.pdf', 'x/a.pdf', 10, 'done', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO extract_results(project_id, row_id, source_file_id, cell_data,
                                            is_edited, error_msg, created_at, updated_at)
                 VALUES (1, '1234567890', 1, '{\"2\":{\"value\":\"v\"}}', 1, '',
                        '2026-01-01 00:00:00', '2026-01-02 00:00:00')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v8：冗余列消失、数据保留
        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        for bad in [
            ("settings", "temperature"),
            ("parse_results", "started_at"),
            ("parse_results", "completed_at"),
            ("export_tasks", "completed_at"),
            ("export_tasks", "error_msg"),
            ("extract_results", "row_id"),
        ] {
            let err = conn.query_row(&format!("SELECT {} FROM {}", bad.1, bad.0), [], |_| Ok(()));
            assert!(err.is_err(), "{}.{} 列应已删除", bad.0, bad.1);
        }
        let (cell, edited): (String, i64) = conn
            .query_row(
                "SELECT cell_data, is_edited FROM extract_results",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cell, r#"{"2":{"value":"v"}}"#, "提取结果数据应保留");
        assert_eq!(edited, 1);
        // 新表仍受 UNIQUE(project_id, source_file_id) 约束
        let dup = conn.execute(
            "INSERT INTO extract_results(project_id, source_file_id, cell_data,
                                        is_edited, error_msg, created_at, updated_at)
             VALUES (1, 1, '{}', 0, '', '2026-01-03 00:00:00', '2026-01-03 00:00:00')",
            [],
        );
        assert!(dup.is_err(), "重复 source_file_id 应被唯一约束拒绝");
    }

    #[test]
    fn migration_v9_renames_openai_compatible_to_kimi() {
        // 模拟 v8 旧库：settings 含 openai_compatible 激活行
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            for (v, sql) in MIGRATIONS.iter().take(8) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO settings(provider, base_url, api_key, model, timeout,
                                    is_active, updated_at)
                 VALUES ('openai_compatible', 'https://api.moonshot.cn/v1', 'sk-x',
                        'kimi-k2.6', 300, 1, '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v9：provider 更名 kimi，配置与激活位保留
        let db = Db::open(&path).unwrap();
        let view = crate::settings::get_settings(&db).unwrap();
        assert_eq!(view.active_provider, "kimi");
        assert_eq!(view.kimi.api_key, "sk-x", "存量配置应随更名保留");
        assert_eq!(view.kimi.model, "kimi-k2.6");
        // 旧键已不存在（read_one 查不到会落默认空 api_key）
        let old: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM settings WHERE provider = 'openai_compatible'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);
    }

    #[test]
    fn migration_v10_adds_task_type_and_ic_tables_keeps_old_data() {
        // 模拟 v9 旧库（v0.0.10）：projects 无 task_type 列且有完整项目数据
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            for (v, sql) in MIGRATIONS.iter().take(9) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('旧项目', '2026-01-01 00:00:00', '/b', 'c', 'idle')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO upload_batches(project_id, folder_name, uploaded_at)
                 VALUES (1, 'b', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO source_files(batch_id, original_name, stored_name, relative_path,
                                        file_size, status, created_at)
                 VALUES (1, 'a.pdf', 'a.pdf', 'x/a.pdf', 10, 'done', '2026-01-01 00:00:00')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v10：task_type 列出现且存量行默认 'parse'，旧数据完整
        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let (name, task_type): (String, String) = conn
            .query_row(
                "SELECT name, task_type FROM projects WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "旧项目", "存量项目数据应保留");
        assert_eq!(
            task_type, "parse",
            "存量项目 task_type 应默认 'parse'（缺列时此查询直接报错）"
        );
        // 既有子表数据不受影响
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM source_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "source_files 数据应保留");
        // ic_* 新表齐备且为空
        for t in [
            "ic_documents",
            "ic_nodes",
            "ic_edges",
            "ic_groups",
            "ic_doc_links",
            "ic_roster",
            "ic_org",
            "ic_counters",
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{t} 应为空表（缺表时此查询直接报错）");
        }
        let version: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as u32);
    }

    #[test]
    fn migration_v12_adds_node_canvas_size_columns() {
        // 模拟 v11 库：ic_nodes 有一行且无 canvas_w/h 列
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            for (v, sql) in MIGRATIONS.iter().take(11) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('内控', '2026-01-01 00:00:00', '', '', 'idle')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ic_nodes(project_id, code, node_type)
                 VALUES (1, 'ACT-1', 'ACT')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v12：canvas_w/h 列出现，存量行默认 0（内容自适应）
        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let (w, h): (f64, f64) = conn
            .query_row(
                "SELECT canvas_w, canvas_h FROM ic_nodes WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (w, h),
            (0.0, 0.0),
            "存量行尺寸应为 0（缺列时此查询直接报错）"
        );
        let version: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as u32);
    }

    #[test]
    fn migration_v11_adds_doc_blob_columns_keeps_rows() {
        // 模拟 v10 库：ic_documents 已有磁盘路径行（无 data 列）
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute_batch(INIT_SQL).unwrap();
            for (v, sql) in MIGRATIONS.iter().take(10) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, 'x')",
                    rusqlite::params![v],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                 VALUES ('内控', '2026-01-01 00:00:00', '', '', 'idle')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ic_documents(project_id, name, rel_path, file_path)
                 VALUES (1, 'a.pdf', 'a.pdf', '/old/path/a.pdf')",
                [],
            )
            .unwrap();
        }
        // 重新打开应只补 v11：data/size 列出现，旧行保留且 data 为 NULL（读时回落磁盘）
        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let (name, data, size): (String, Option<Vec<u8>>, i64) = conn
            .query_row(
                "SELECT name, data, size FROM ic_documents WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "a.pdf", "旧行应保留");
        assert!(
            data.is_none(),
            "旧行 data 应为 NULL（缺列时此查询直接报错）"
        );
        assert_eq!(size, 0);
        let version: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as u32);
    }

    #[test]
    fn migrate_is_idempotent_on_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("querydoc.db");
        {
            let db = Db::open(&path).unwrap();
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO projects(name, created_at, base_path, project_dir, status)
                     VALUES ('a', '2026-01-01 00:00:00', '/b', 'c', 'idle')",
                    [],
                )
                .unwrap();
        }
        // 重新打开：迁移不重复执行，数据保留
        let db = Db::open(&path).unwrap();
        let count: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let migrations: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(migrations, MIGRATIONS.len() as i64);
    }

    #[test]
    fn open_creates_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/deep/querydoc.db");
        Db::open(&path).unwrap();
        assert!(path.exists());
    }
}
