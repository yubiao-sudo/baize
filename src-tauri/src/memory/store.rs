use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::security::AuditEntry;

/// 持久化存储（SQLite）。
///
/// M1 承载：会话 / 消息（工作记忆）持久化 + 审计日志。
/// M3 将在此之上扩展 FTS5 全文 + 向量双路召回、焦点栈、知识图谱（见设计文档 4.4）。
pub struct MemoryStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRow {
    pub role: String,
    pub content: String,
    pub created_at: i64,
    /// 执行流 JSON（thoughts + todos），仅 assistant 消息可能有值
    pub trace: Option<String>,
    /// 附件路径 JSON 数组字符串（用户消息上传的图片/文件）
    pub attachments: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationRow {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    /// 所属项目 id（未归入任何项目时为 null）
    pub project_id: Option<String>,
}

/// 项目（侧边栏「项目」导航）：一个工作目录 + 其会话分组
#[derive(Debug, Clone, Serialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryRow {
    pub mem_id: String,
    pub content: String,
    pub kind: String,
    pub salience: i64,
    pub last_access: i64,
}

/// 情景记忆（events 表）：发生过的具体事件（对话、操作、决策）
#[derive(Debug, Clone, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub ts: i64,
    pub event_type: String,
    pub summary: String,
    pub detail: Option<String>,
    pub importance: f64,
    pub hit_count: i64,
    pub last_hit: Option<i64>,
}

/// 语义记忆（semantic_memories 表）：提炼出的画像 / 偏好 / 项目知识
#[derive(Debug, Clone, Serialize)]
pub struct SemanticMemoryRow {
    pub id: i64,
    pub category: String,
    pub content: String,
    pub confidence: f64,
    pub source_event_id: Option<i64>,
}

/// 记忆看板总览：各类记忆条数（供前端看板 / 命令返回）
#[derive(Debug, Clone, Serialize)]
pub struct MemoryOverview {
    pub memories: usize,
    pub events: usize,
    pub semantic: usize,
    pub scheduled: usize,
    pub watchdog: usize,
}

/// 记忆写入/召回结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RememberOutcome {
    Created,     // 新记忆
    Reinforced,  // 同话题，强化已有记忆
    Filtered,    // 噪音，已过滤
}

// ---------- 记忆参数（可调） ----------
/// 纯寒暄/噪音词，不记录
const NOISE_STOPLIST: &[&str] = &[
    "你好", "您好", "谢谢", "好的", "嗯", "在吗", "收到", "知道了", "再见", "拜拜",
    "好的谢谢", "明白了", "没问题", "hi", "hello", "ok", "okay", "thanks", "bye",
];
/// 少于该字数不记录
const MIN_MEMORY_LEN: usize = 4;
/// 共享 n-gram >= 该值视为同话题
const SIMILAR_THRESHOLD: usize = 2;
/// 新记忆初始显著性
const INITIAL_SALIENCE: i64 = 5;
/// 同话题强化增量
const REINFORCE_DELTA: i64 = 2;
/// 显著性上限
const MAX_SALIENCE: i64 = 100;
/// 遗忘半衰期（天）
const HALF_LIFE_DAYS: f64 = 3.0;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS conversations (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    path       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages(conversation_id, id);
CREATE TABLE IF NOT EXISTS audit_log (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       INTEGER NOT NULL,
    subject  TEXT,
    tool     TEXT,
    args     TEXT,
    decision TEXT,
    result   TEXT
);
CREATE TABLE IF NOT EXISTS memories (
    mem_id      TEXT PRIMARY KEY,
    content     TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'semantic',
    salience    INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    last_access INTEGER NOT NULL,
    embedding   TEXT
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS rag_chunks (
    id         TEXT PRIMARY KEY,
    path       TEXT NOT NULL,
    content    TEXT NOT NULL,
    embedding  TEXT
);
CREATE TABLE IF NOT EXISTS vault (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS scheduled_jobs (
    id   TEXT PRIMARY KEY,
    data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS job_runs (
    id         TEXT PRIMARY KEY,
    job_id     TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_job_runs_job ON job_runs(job_id, started_at);
CREATE TABLE IF NOT EXISTS workflows (
    id   TEXT PRIMARY KEY,
    data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS workflow_runs (
    id          TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    data        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_wf ON workflow_runs(workflow_id, started_at);
CREATE TABLE IF NOT EXISTS plaza_items (
    id   TEXT PRIMARY KEY,
    data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    type         TEXT NOT NULL DEFAULT 'event',
    summary      TEXT NOT NULL,
    detail       TEXT,
    importance   REAL NOT NULL DEFAULT 0.5,
    hit_count    INTEGER NOT NULL DEFAULT 0,
    last_hit     INTEGER,
    consolidated INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE TABLE IF NOT EXISTS semantic_memories (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    category        TEXT NOT NULL,
    content         TEXT NOT NULL,
    embedding       TEXT,
    confidence      REAL NOT NULL DEFAULT 0.5,
    source_event_id INTEGER
);
CREATE INDEX IF NOT EXISTS idx_semantic_cat ON semantic_memories(category);
CREATE TABLE IF NOT EXISTS forget_policy (
    category     TEXT PRIMARY KEY,
    max_age_days INTEGER NOT NULL DEFAULT 30,
    min_hits     INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS watchdog_tasks (
    id   TEXT PRIMARY KEY,
    data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS watchdog_runs (
    id         TEXT PRIMARY KEY,
    task_id    TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_watchdog_runs_task ON watchdog_runs(task_id, started_at);
CREATE TABLE IF NOT EXISTS skills (
    name TEXT PRIMARY KEY,
    data TEXT NOT NULL
);
"#;

impl MemoryStore {
    pub fn open(path: &str) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("初始化表结构失败: {e}"))?;
        // 迁移：给旧库补 embedding 列（已存在则忽略）
        let _ = conn.execute("ALTER TABLE memories ADD COLUMN embedding TEXT", []);
        // 迁移：给消息表补 trace 列（存储执行流 JSON，已存在则忽略）
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN trace TEXT", []);
        // 迁移：给消息表补 attachments 列（存储附件路径 JSON，已存在则忽略）
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN attachments TEXT", []);
        // 迁移：给事件表补 consolidated 列（是否已归纳为语义记忆，已存在则忽略）
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN consolidated INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // 迁移：给会话表补 project_id 列（项目归属，已存在则忽略）
        let _ = conn.execute("ALTER TABLE conversations ADD COLUMN project_id TEXT", []);
        // 迁移：历史会话话题命名回填——仍是默认标题（「新会话」/「默认会话」/空）且已有用户消息的会话，
        // 用首条用户消息（折叠空白、取前 20 字）作为话题命名；已命名过的会话不动。
        // COALESCE 兜底：无消息可取时保留原标题，避免触发 title 的 NOT NULL 约束
        let _ = conn.execute(
            "UPDATE conversations SET title = COALESCE((
                SELECT substr(rtrim(ltrim(replace(replace(content, char(13), ' '), char(10), ' '),
                                        ' ' || char(9) || char(10) || char(13)),
                                     ' ' || char(9) || char(10) || char(13)), 1, 20)
                FROM messages
                WHERE messages.conversation_id = conversations.id
                  AND role = 'user' AND trim(content) <> ''
                ORDER BY id ASC LIMIT 1
             ), title)
             WHERE title IN ('新会话', '默认会话', '')
               AND EXISTS (SELECT 1 FROM messages
                           WHERE conversation_id = conversations.id
                             AND role = 'user' AND trim(content) <> '')",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    // ---------- 会话与消息（工作记忆） ----------

    pub fn ensure_conversation(
        &self,
        conv_id: &str,
        title: &str,
        project_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO conversations(id, title, project_id, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![conv_id, title, project_id, Self::now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 会话话题命名：仅当会话仍是默认标题（「新会话」/「默认会话」/空）时改为 title，
    /// 保证标题始终来自该会话的首条用户消息；已被命名过的会话不动
    pub fn rename_conversation_if_default(&self, conv_id: &str, title: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET title = ?2 WHERE id = ?1 AND title IN ('新会话', '默认会话', '')",
            params![conv_id, title],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---------- 项目（侧边栏「项目」导航） ----------

    /// 新建项目（同 id 则覆盖标题与路径）
    pub fn ensure_project(&self, id: &str, name: &str, path: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO projects(id, name, path, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![id, name, path, Self::now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, path, created_at FROM projects ORDER BY created_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ProjectRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    path: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 删除项目：其会话自动回到「未分组」（project_id 置 NULL），消息不受影响
    pub fn delete_project(&self, project_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET project_id = NULL WHERE project_id = ?1",
            params![project_id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM projects WHERE id = ?1", params![project_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 把会话归入项目（project_id 传 None 表示移出项目）
    pub fn set_conversation_project(
        &self,
        conv_id: &str,
        project_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET project_id = ?2 WHERE id = ?1",
            params![conv_id, project_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 查询会话所属项目（未归入任何项目返回 None）。
    /// 供 chat 命令注入【当前项目】上下文——白泽由此知道自己在为哪个项目服务。
    pub fn project_of_conversation(&self, conv_id: &str) -> Result<Option<ProjectRow>, String> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT p.id, p.name, p.path, p.created_at
                 FROM projects p JOIN conversations c ON c.project_id = p.id
                 WHERE c.id = ?1",
                params![conv_id],
                |r| {
                    Ok(ProjectRow {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        path: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    pub fn add_message(
        &self,
        conv_id: &str,
        role: &str,
        content: &str,
        attachments: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(conversation_id, role, content, attachments, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![conv_id, role, content, attachments, Self::now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn messages(&self, conv_id: &str, limit: usize) -> Result<Vec<MessageRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT role, content, created_at, trace, attachments FROM messages
                 WHERE conversation_id = ?1 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![conv_id, limit as i64], |r| {
                Ok(MessageRow {
                    role: r.get(0)?,
                    content: r.get(1)?,
                    created_at: r.get(2)?,
                    trace: r.get(3)?,
                    attachments: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        out.reverse();
        Ok(out)
    }

    /// 把执行流（JSON 字符串）挂到该会话最新一条 assistant 消息上
    pub fn attach_trace(&self, conv_id: &str, trace: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET trace = ?1
             WHERE id = (SELECT id FROM messages
                         WHERE conversation_id = ?2 AND role = 'assistant'
                         ORDER BY id DESC LIMIT 1)",
            params![trace, conv_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 写入一条「多模型对比」assistant 消息：content 为空，各模型分支结果存 trace.branches
    /// （对比结果原本只在前端内存里，重启即丢；落库后重新打开会话仍可回看）
    pub fn add_compare_message(&self, conv_id: &str, trace_json: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(conversation_id, role, content, trace, created_at) VALUES(?1, 'assistant', '', ?2, ?3)",
            params![conv_id, trace_json, Self::now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 最近一条用户消息的时间戳（毫秒），用于计算「用户多久没互动」（主动心跳）。
    pub fn last_user_message_time(&self) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT created_at FROM messages WHERE role = 'user' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(row)
    }

    pub fn list_conversations(&self) -> Result<Vec<ConversationRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, title, project_id, created_at FROM conversations ORDER BY created_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ConversationRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    project_id: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn delete_conversation(&self, conv_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM messages WHERE conversation_id = ?1",
            params![conv_id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM conversations WHERE id = ?1", params![conv_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---------- RAG 知识库 chunk 持久化 ----------

    /// 保存 RAG chunks（清空重建）
    pub fn save_rag_chunks(
        &self,
        chunks: &[(String, String, Option<Vec<f32>>)],
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM rag_chunks", [])
            .map_err(|e| e.to_string())?;
        for (path, content, embedding) in chunks {
            let id = format!("{:016x}", stable_hash(&format!("{path}\n{content}")));
            let emb = embedding
                .as_ref()
                .and_then(|e| serde_json::to_string(e).ok());
            conn.execute(
                "INSERT INTO rag_chunks(id, path, content, embedding) VALUES(?1, ?2, ?3, ?4)",
                params![id, path, content, emb],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// 加载 RAG chunks
    pub fn load_rag_chunks(&self) -> Result<Vec<(String, String, Option<Vec<f32>>)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT path, content, embedding FROM rag_chunks")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            let (path, content, emb) = r.map_err(|e| e.to_string())?;
            let embedding = emb.and_then(|s| serde_json::from_str::<Vec<f32>>(&s).ok());
            out.push((path, content, embedding));
        }
        Ok(out)
    }

    /// 清空知识库（rag_chunks 表）
    pub fn clear_rag_chunks(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM rag_chunks", [])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---------- 审计日志 ----------

    pub fn add_audit(&self, entry: &AuditEntry) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO audit_log(ts, subject, tool, args, decision, result)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.ts as i64,
                entry.subject,
                entry.tool,
                entry.args.to_string(),
                entry.decision,
                entry.result,
            ],
        )
        .map_err(|e| e.to_string())?;
        // 审计表瘦身：目前只写不读，无限增长会让 SQLite 文件持续膨胀。
        // 每 256 次写入裁剪一次，只保留最近 5000 条。
        use std::sync::atomic::{AtomicU32, Ordering};
        static AUDIT_WRITES: AtomicU32 = AtomicU32::new(0);
        if AUDIT_WRITES.fetch_add(1, Ordering::Relaxed) % 256 == 255 {
            let _ = self.prune_audit(5000);
        }
        Ok(())
    }

    /// 自维护：审计日志裁剪到最近 keep 条 + WAL 压缩，返回裁剪条数。
    /// 由 maintenance 模块周期性调用（写入端 256 次一裁是兜底，这里是主清理路径）。
    pub fn prune_audit(&self, keep: usize) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let pruned = conn
            .execute(
                "DELETE FROM audit_log WHERE rowid NOT IN (
                     SELECT rowid FROM audit_log ORDER BY ts DESC LIMIT ?1
                 )",
                params![keep as i64],
            )
            .map_err(|e| e.to_string())?;
        // WAL 截断归档：把 -wal 文件收回到主库，释放磁盘
        let _ = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", []);
        Ok(pruned)
    }

    // ---------- 记忆（M1 基础；M3 升级召回） ----------

    /// 底层写入（按内容哈希去重），保留供直接使用
    pub fn upsert_memory(&self, content: &str, kind: &str) -> Result<(), String> {
        self.insert_with_salience(content, kind, 1)
    }

    fn insert_with_salience(&self, content: &str, kind: &str, salience: i64) -> Result<(), String> {
        // 尝试生成 embedding（失败存 NULL，检索时回退 n-gram）
        let embedding = crate::embedding::embed(content)
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok());
        let conn = self.conn.lock().unwrap();
        let id = format!("{:016x}", stable_hash(content));
        conn.execute(
            "INSERT INTO memories(mem_id, content, kind, salience, created_at, last_access, embedding)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6)
             ON CONFLICT(mem_id) DO UPDATE SET last_access = excluded.last_access",
            params![id, content, kind, salience, Self::now(), embedding],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 智能记录：过滤噪音 + 同话题合并强化（M3 v3）
    pub fn smart_remember(&self, text: &str, kind: &str) -> Result<RememberOutcome, String> {
        let t = text.trim();
        // 过滤：过短 / 纯寒暄
        if t.chars().count() < MIN_MEMORY_LEN
            || NOISE_STOPLIST.contains(&t.to_lowercase().as_str())
        {
            return Ok(RememberOutcome::Filtered);
        }
        let grams = ngrams(t, 2, 3);
        if grams.is_empty() {
            return Ok(RememberOutcome::Filtered);
        }

        // 找同话题的已有记忆（n-gram 重叠最高者）
        let candidates = self.recent_memories(50)?;
        let mut best: Option<(usize, String)> = None;
        for m in &candidates {
            let overlap = grams.iter().filter(|g| m.content.contains(g.as_str())).count();
            if overlap >= SIMILAR_THRESHOLD
                && overlap > best.as_ref().map(|(s, _)| *s).unwrap_or(0)
            {
                best = Some((overlap, m.mem_id.clone()));
            }
        }

        match best {
            Some((_, mem_id)) => {
                // 同话题：强化已有记忆（显著性提升、时间刷新），不新建
                let conn = self.conn.lock().unwrap();
                conn.execute(
                    "UPDATE memories SET salience = MIN(salience + ?1, ?2), last_access = ?3
                     WHERE mem_id = ?4",
                    params![REINFORCE_DELTA, MAX_SALIENCE, Self::now(), mem_id],
                )
                .map_err(|e| e.to_string())?;
                Ok(RememberOutcome::Reinforced)
            }
            None => {
                self.insert_with_salience(t, kind, INITIAL_SALIENCE)?;
                Ok(RememberOutcome::Created)
            }
        }
    }

    pub fn recall(&self, keyword: &str, top_k: usize) -> Result<Vec<MemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT mem_id, content, kind, salience, last_access FROM memories
                 WHERE content LIKE ?1
                 ORDER BY salience DESC, last_access DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let like = format!("%{keyword}%");
        let rows = stmt
            .query_map(params![like, top_k as i64], |r| {
                Ok(MemoryRow {
                    mem_id: r.get(0)?,
                    content: r.get(1)?,
                    kind: r.get(2)?,
                    salience: r.get(3)?,
                    last_access: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 最近记忆（按最近访问排序），用于召回注入与前端展示
    pub fn recent_memories(&self, limit: usize) -> Result<Vec<MemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT mem_id, content, kind, salience, last_access FROM memories
                 ORDER BY last_access DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(MemoryRow {
                    mem_id: r.get(0)?,
                    content: r.get(1)?,
                    kind: r.get(2)?,
                    salience: r.get(3)?,
                    last_access: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 按类型列出记忆（记忆看板明细）：kind=None 列全部；置顶（salience 降序）优先，再按最近访问
    pub fn list_memories_by_kind(
        &self,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT mem_id, content, kind, salience, last_access FROM memories
                 WHERE (?1 IS NULL OR kind = ?1)
                 ORDER BY salience DESC, last_access DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![kind, limit as i64], |r| {
                Ok(MemoryRow {
                    mem_id: r.get(0)?,
                    content: r.get(1)?,
                    kind: r.get(2)?,
                    salience: r.get(3)?,
                    last_access: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 置顶记忆：salience +10（上限 100）并刷新访问时间，让召回排序优先。返回是否存在
    pub fn pin_memory(&self, mem_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute(
                "UPDATE memories SET salience = MIN(100, salience + 10), last_access = ?2
                 WHERE mem_id = ?1",
                params![mem_id, Self::now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    /// 相关记忆召回：语义（embedding 余弦相似度）优先，回退 n-gram 相关度
    pub fn recall_related(&self, text: &str, limit: usize) -> Result<Vec<MemoryRow>, String> {
        // 1) 语义召回：embedding 相似度（若 Ollama embedding 可用）
        if let Ok(query_emb) = crate::embedding::embed(text) {
            if let Ok(sem) = self.semantic_recall(&query_emb, limit) {
                if !sem.is_empty() {
                    for m in &sem {
                        let _ = self.bump_salience(&m.mem_id);
                    }
                    return Ok(sem);
                }
            }
        }

        // 2) 回退：n-gram 相关度 × salience × 时间衰减（遗忘曲线）
        let grams = ngrams(text, 2, 3);
        if grams.is_empty() {
            return self.recent_memories(limit);
        }
        // 候选：最近 50 条（避免全表扫描；大规模时升级 FTS5+向量）
        let candidates = self.recent_memories(50)?;
        let now = Self::now();
        let mut scored: Vec<(f64, MemoryRow)> = candidates
            .into_iter()
            .map(|m| {
                let overlap = grams.iter().filter(|g| m.content.contains(g.as_str())).count();
                let age = (now - m.last_access).max(0);
                let score = (overlap as f64) * (m.salience as f64 + 1.0) * decay(age);
                (score, m)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let result: Vec<MemoryRow> = scored
            .into_iter()
            .filter(|(s, _)| *s > 0.0)
            .take(limit)
            .map(|(_, m)| m)
            .collect();

        // 命中记忆提升显著性（常用常新，减缓遗忘）
        for m in &result {
            let _ = self.bump_salience(&m.mem_id);
        }
        Ok(result)
    }

    /// 召回「用户画像」：高 salience 且含偏好/身份/习惯关键词的记忆。
    /// 供每轮对话注入 system prompt，让白泽越来越懂用户。
    pub fn recall_profile(&self, limit: usize) -> Result<Vec<MemoryRow>, String> {
        const PROFILE_HINTS: &[&str] = &[
            "喜欢", "不喜欢", "偏好", "希望", "不要", "以后", "记得", "称呼", "我是", "名字", "常用", "习惯",
        ];
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT mem_id, content, kind, salience, last_access FROM memories
                 WHERE salience >= 2 ORDER BY salience DESC, last_access DESC LIMIT 50",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(MemoryRow {
                    mem_id: r.get(0)?,
                    content: r.get(1)?,
                    kind: r.get(2)?,
                    salience: r.get(3)?,
                    last_access: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut out = Vec::new();
        for r in rows.flatten() {
            if PROFILE_HINTS.iter().any(|h| r.content.contains(h)) {
                out.push(r);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// 召回「经验教训」（kind='lesson'）：白泽自己的经验知识库。
    /// 任务复盘（工具失败→解决）写入的经验由此召回，注入 system prompt，
    /// 下次遇到同类问题时优先采用过往验证过的解决办法（程序性经验）。
    pub fn recall_lessons(&self, text: &str, limit: usize) -> Result<Vec<MemoryRow>, String> {
        self.recall_by_kind(text, "lesson", limit)
    }

    /// 召回「成功操作配方」（kind='recipe'）：GUI 任务成功后沉淀的操作链。
    /// 下次操作同类应用时注入 system prompt，直接照用已验证的步骤序列。
    pub fn recall_recipes(&self, text: &str, limit: usize) -> Result<Vec<MemoryRow>, String> {
        self.recall_by_kind(text, "recipe", limit)
    }

    /// 按 kind 召回（embedding 余弦优先 × salience × 遗忘衰减，n-gram 兜底）
    pub fn recall_by_kind(
        &self,
        text: &str,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT mem_id, content, kind, salience, last_access, embedding
                 FROM memories WHERE kind = ?1
                 ORDER BY salience DESC, last_access DESC LIMIT 100",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([kind], |r| {
                Ok((
                    MemoryRow {
                        mem_id: r.get(0)?,
                        content: r.get(1)?,
                        kind: r.get(2)?,
                        salience: r.get(3)?,
                        last_access: r.get(4)?,
                    },
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);
        drop(conn);
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // 语义相似优先（embedding 余弦 × salience × 遗忘衰减）
        if let Ok(q) = crate::embedding::embed(text) {
            let now = Self::now();
            let mut scored: Vec<(f64, MemoryRow)> = Vec::new();
            for (m, emb_str) in &rows {
                let Some(s) = emb_str else { continue };
                let Ok(emb) = serde_json::from_str::<Vec<f32>>(s) else { continue };
                let sim = crate::embedding::cosine(&q, &emb);
                if sim > 0.3 {
                    let age = (now - m.last_access).max(0);
                    scored.push((sim * (m.salience as f64 + 1.0) * decay(age), m.clone()));
                }
            }
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let result: Vec<MemoryRow> =
                scored.into_iter().take(limit).map(|(_, m)| m).collect();
            if !result.is_empty() {
                for m in &result {
                    let _ = self.bump_salience(&m.mem_id);
                }
                return Ok(result);
            }
        }

        // 回退：n-gram 相关度（与 recall_related 同口径，只在 lesson 子集内）
        let grams = ngrams(text, 2, 3);
        if grams.is_empty() {
            return Ok(Vec::new());
        }
        let now = Self::now();
        let mut scored: Vec<(f64, MemoryRow)> = rows
            .into_iter()
            .map(|(m, _)| {
                let overlap = grams.iter().filter(|g| m.content.contains(g.as_str())).count();
                let age = (now - m.last_access).max(0);
                let score = (overlap as f64) * (m.salience as f64 + 1.0) * decay(age);
                (score, m)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let result: Vec<MemoryRow> = scored
            .into_iter()
            .filter(|(s, _)| *s > 0.0)
            .take(limit)
            .map(|(_, m)| m)
            .collect();
        for m in &result {
            let _ = self.bump_salience(&m.mem_id);
        }
        Ok(result)
    }

    /// 语义召回：全量记忆做「语义相似度 × 显著性 × 遗忘衰减」联合排序
    fn semantic_recall(&self, query_emb: &[f32], limit: usize) -> Result<Vec<MemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT mem_id, content, kind, salience, last_access, embedding
                 FROM memories",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    MemoryRow {
                        mem_id: r.get(0)?,
                        content: r.get(1)?,
                        kind: r.get(2)?,
                        salience: r.get(3)?,
                        last_access: r.get(4)?,
                    },
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        let now = Self::now();
        let mut scored: Vec<(f64, MemoryRow)> = Vec::new();
        for r in rows {
            let (m, emb_str) = r.map_err(|e| e.to_string())?;
            if let Some(emb_str) = emb_str {
                if let Ok(emb) = serde_json::from_str::<Vec<f32>>(&emb_str) {
                    let sim = crate::embedding::cosine(query_emb, &emb);
                    if sim > 0.3 {
                        let age = (now - m.last_access).max(0);
                        // 联合排序：语义相似度为主导，显著性放大，遗忘曲线衰减
                        let score = sim * (m.salience as f64 + 1.0) * decay(age);
                        scored.push((score, m));
                    }
                }
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, m)| m).collect())
    }

    fn bump_salience(&self, mem_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE memories SET salience = salience + 1, last_access = ?2 WHERE mem_id = ?1",
            params![mem_id, Self::now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 记忆衰减：超过 7 天未访问的记忆降权（salience -1），salience 归零的删除。
    /// 返回被降权的条数。供「主动意识」空闲整理调用。
    pub fn decay_memories(&self) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let week_ms = 7 * 24 * 3600 * 1000i64;
        let cutoff = Self::now() - week_ms;
        let changed = conn
            .execute(
                "UPDATE memories SET salience = salience - 1 WHERE last_access < ?1 AND salience > 0",
                params![cutoff],
            )
            .map_err(|e| e.to_string())?;
        let _ = conn
            .execute("DELETE FROM memories WHERE salience <= 0", [])
            .map_err(|e| e.to_string())?;
        Ok(changed)
    }

    /// 记忆去重合并：内容高度相似（embedding 余弦 > 0.92 或字符 n-gram 重叠 ≥ 6）
    /// 的记忆视为重复，保留 salience 更高的一条并 +1 强化，删除另一条。
    /// 返回被合并删除的条数。
    pub fn merge_duplicate_memories(&self) -> Result<usize, String> {
        // 读段（短锁）：一次性取出比对所需数据后立刻释放 DB 锁。
        // 切勿在 O(n²) 比对期间持锁——后台治理线程长期持锁会让主线程上的
        // 同步命令（如设置保存）阻塞，进而冻住 WebView2 导致界面崩溃。
        let items: Vec<(String, String, i64, Option<Vec<f32>>)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT mem_id, content, salience, embedding FROM memories")
                .map_err(|e| e.to_string())?;
            let rows: Vec<(String, String, i64, Option<String>)> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .filter_map(|r| r.ok())
                .collect();
            rows.into_iter()
                .map(|(id, content, sal, emb)| {
                    let emb = emb.and_then(|s| serde_json::from_str::<Vec<f32>>(&s).ok());
                    (id, content, sal, emb)
                })
                .collect()
        }; // conn 锁在此释放

        // 比对段（无锁）
        let mut removed: Vec<String> = Vec::new();
        let mut boosted: Vec<String> = Vec::new();
        for i in 0..items.len() {
            if removed.contains(&items[i].0) {
                continue;
            }
            for j in (i + 1)..items.len() {
                if removed.contains(&items[j].0) {
                    continue;
                }
                let a = &items[i];
                let b = &items[j];
                let dup = match (&a.3, &b.3) {
                    (Some(ea), Some(eb)) => crate::embedding::cosine(ea, eb) > 0.92,
                    _ => ngram_overlap(&a.1, &b.1) >= 6,
                };
                if dup {
                    // 保留 salience 高（同值保留先写入的 i）
                    let (keep, drop) = if a.2 >= b.2 { (&items[i], &items[j]) } else { (&items[j], &items[i]) };
                    boosted.push(keep.0.clone());
                    removed.push(drop.0.clone());
                }
            }
        }
        // 写段（短锁）：重新拿锁集中落库，避免与主线程命令长时间互斥
        {
            let conn = self.conn.lock().unwrap();
            for id in &removed {
                let _ = conn.execute("DELETE FROM memories WHERE mem_id = ?1", params![id]);
            }
            for id in &boosted {
                let _ = conn.execute(
                    "UPDATE memories SET salience = salience + 1 WHERE mem_id = ?1",
                    params![id],
                );
            }
        }
        Ok(removed.len())
    }

    /// 记忆治理（一键整理）：去重合并 + 衰减清理。返回 (合并删除数, 衰减降权数)。
    pub fn consolidate_memories(&self) -> Result<(usize, usize), String> {
        let merged = self.merge_duplicate_memories()?;
        let decayed = self.decay_memories()?;
        Ok((merged, decayed))
    }

    /// 记忆图谱：节点（记忆）+ 边（相似关系），供前端意识网络渲染
    pub fn memory_graph(&self) -> Result<MemoryGraph, String> {
        let nodes = self.recent_memories(20)?;
        let mut edges = Vec::new();
        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                let overlap = ngram_overlap(&nodes[i].content, &nodes[j].content);
                if overlap >= 3 {
                    edges.push(MemoryEdge {
                        from: nodes[i].mem_id.clone(),
                        to: nodes[j].mem_id.clone(),
                        weight: overlap as f64,
                    });
                }
            }
        }
        Ok(MemoryGraph { nodes, edges })
    }

    // ---------- 通用设置（键值，用于持久化模型配置等） ----------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT value FROM settings WHERE key = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![key], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e.to_string()),
            None => Ok(None),
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---------- 凭据 Vault（值已由调用方加密为密文文本） ----------

    pub fn vault_get(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT value FROM vault WHERE key = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![key], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e.to_string()),
            None => Ok(None),
        }
    }

    pub fn vault_set(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO vault(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn vault_list(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT key FROM vault ORDER BY key")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn vault_delete(&self, key: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM vault WHERE key = ?1", params![key])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    // ---------- 定时任务（data 为调度器序列化的 JSON） ----------

    pub fn list_scheduled_jobs(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM scheduled_jobs ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn upsert_scheduled_job(&self, id: &str, data: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO scheduled_jobs(id, data) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![id, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_scheduled_job(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM scheduled_jobs WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    // ---------- 定时任务执行日志（job_runs） ----------

    /// 写入/更新一条执行日志（data 为调度器序列化的 JobRun JSON）
    pub fn upsert_job_run(&self, run_id: &str, job_id: &str, started_at: i64, data: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO job_runs(id, job_id, started_at, data) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![run_id, job_id, started_at, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 查询某个任务的执行日志（按开始时间倒序），job_id 传空串则查询全部
    pub fn list_job_runs(&self, job_id: &str, limit: usize) -> Result<Vec<(String, String, i64, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = if job_id.is_empty() {
            "SELECT id, job_id, started_at, data FROM job_runs ORDER BY started_at DESC LIMIT ?1"
        } else {
            "SELECT id, job_id, started_at, data FROM job_runs WHERE job_id = ?1 ORDER BY started_at DESC LIMIT ?2"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, i64, String)> = if job_id.is_empty() {
            stmt.query_map(params![limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        } else {
            stmt.query_map(params![job_id, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        };
        Ok(rows)
    }

    /// 清空某任务的全部执行日志，返回删除条数
    pub fn clear_job_runs(&self, job_id: &str) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM job_runs WHERE job_id = ?1", params![job_id])
            .map_err(|e| e.to_string())?;
        Ok(affected)
    }

    // ---------- 可编排工作流（workflows） ----------

    pub fn list_workflows(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM workflows ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn upsert_workflow(&self, id: &str, data: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflows(id, data) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![id, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_workflow(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM workflows WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    // ---------- 工作流执行日志（workflow_runs） ----------

    pub fn upsert_workflow_run(
        &self,
        run_id: &str,
        workflow_id: &str,
        started_at: i64,
        data: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflow_runs(id, workflow_id, started_at, data) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![run_id, workflow_id, started_at, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 查询工作流执行日志（按开始时间倒序），workflow_id 传空串则查询全部
    pub fn list_workflow_runs(
        &self,
        workflow_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, i64, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = if workflow_id.is_empty() {
            "SELECT id, workflow_id, started_at, data FROM workflow_runs ORDER BY started_at DESC LIMIT ?1"
        } else {
            "SELECT id, workflow_id, started_at, data FROM workflow_runs WHERE workflow_id = ?1 ORDER BY started_at DESC LIMIT ?2"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, i64, String)> = if workflow_id.is_empty() {
            stmt.query_map(params![limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        } else {
            stmt.query_map(params![workflow_id, limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        };
        Ok(rows)
    }

    /// 清空某工作流的全部执行日志，返回删除条数
    pub fn clear_workflow_runs(&self, workflow_id: &str) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute(
                "DELETE FROM workflow_runs WHERE workflow_id = ?1",
                params![workflow_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(affected)
    }

    // ---------- 任务广场（plaza_items） ----------

    pub fn list_plaza_items(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM plaza_items ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn upsert_plaza_item(&self, id: &str, data: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO plaza_items(id, data) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![id, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_plaza_item(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM plaza_items WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    // ---------- 记忆管理：删除 / 列出（补全 M3 记忆管理面） ----------

    /// 删除一条记忆（按 mem_id），返回是否删除成功
    pub fn delete_memory(&self, mem_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM memories WHERE mem_id = ?1", params![mem_id])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    /// 清空全部记忆，返回删除条数
    pub fn clear_memories(&self) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM memories", [])
            .map_err(|e| e.to_string())?;
        Ok(affected)
    }

    /// 按关键词遗忘：删除工作记忆、语义记忆与情景事件里命中 keyword 的条目，
    /// 让「忘记我说过 XX」之后相关内容不再出现在简报/检索里。返回删除总条数。
    pub fn forget_matching(&self, keyword: &str) -> Result<usize, String> {
        let kw = keyword.trim();
        if kw.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().unwrap();
        let like = format!("%{kw}%");
        let a = conn
            .execute("DELETE FROM memories WHERE content LIKE ?1", params![like])
            .map_err(|e| e.to_string())?;
        let b = conn
            .execute(
                "DELETE FROM semantic_memories WHERE content LIKE ?1",
                params![like],
            )
            .map_err(|e| e.to_string())?;
        let c = conn
            .execute("DELETE FROM events WHERE summary LIKE ?1", params![like])
            .map_err(|e| e.to_string())?;
        Ok(a + b + c)
    }

    // ---------- 情景记忆（events） ----------

    /// 记录一条情景事件（对话/操作/决策自动打点）
    pub fn record_event(
        &self,
        event_type: &str,
        summary: &str,
        detail: &str,
        importance: f64,
    ) -> Result<i64, String> {
        if summary.trim().is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events(ts, type, summary, detail, importance) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                Self::now(),
                event_type,
                summary,
                if detail.is_empty() { None } else { Some(detail) },
                importance.clamp(0.0, 1.0),
            ],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// 最近情景事件（按时间倒序）
    pub fn list_events(&self, limit: usize) -> Result<Vec<EventRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, type, summary, detail, importance, hit_count, last_hit
                 FROM events ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    event_type: r.get(2)?,
                    summary: r.get(3)?,
                    detail: r.get(4)?,
                    importance: r.get(5)?,
                    hit_count: r.get(6)?,
                    last_hit: r.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 命中的情景事件增加 hit 计数与 last_hit 时间戳
    pub fn bump_event_hit(&self, event_id: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE events SET hit_count = hit_count + 1, last_hit = ?1 WHERE id = ?2",
            params![Self::now(), event_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 关键词召回情景事件（summary/detail 模糊匹配），用于「按上下文回忆做过什么」
    pub fn recall_events(&self, keyword: &str, limit: usize) -> Result<Vec<EventRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, type, summary, detail, importance, hit_count, last_hit
                 FROM events WHERE summary LIKE ?1 OR detail LIKE ?1
                 ORDER BY importance DESC, id DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let like = format!("%{keyword}%");
        let rows = stmt
            .query_map(params![like, limit as i64], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    event_type: r.get(2)?,
                    summary: r.get(3)?,
                    detail: r.get(4)?,
                    importance: r.get(5)?,
                    hit_count: r.get(6)?,
                    last_hit: r.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 按遗忘策略清理事件：超过 max_age_days 且 hit_count 低于 min_hits 的事件删除。
    /// 返回删除条数。category 用于查 forget_policy。
    pub fn consolidate_events(&self) -> Result<usize, String> {
        let policy = self.get_forget_policy("events")?;
        let (max_age_days, min_hits) = policy;
        let cutoff = Self::now() - max_age_days * 24 * 3600 * 1000;
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute(
                "DELETE FROM events WHERE ts < ?1 AND hit_count < ?2",
                params![cutoff, min_hits],
            )
            .map_err(|e| e.to_string())?;
        Ok(affected)
    }

    // ---------- 语义记忆（semantic_memories） ----------

    /// 情景→语义巩固：把尚未巩固且重要性较高的情景事件归纳为语义记忆，
    /// 并按事件类型映射语义分类；成功后标记 consolidated=1。返回本次巩固的事件条数。
    pub fn consolidate_events_to_semantic(&self) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, type, summary, importance FROM events
                 WHERE consolidated = 0 AND importance >= 0.6
                 ORDER BY id ASC LIMIT 100",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String, f64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);

        let mut count = 0usize;
        for (id, event_type, summary, importance) in rows {
            if summary.trim().is_empty() {
                let _ = conn.execute("UPDATE events SET consolidated = 1 WHERE id = ?1", params![id]);
                continue;
            }
            let category = match event_type.as_str() {
                "decision" => "project",
                "task" => "project",
                "lesson" => "lesson",
                "preference" => "preference",
                _ => "event",
            };
            let embedding = crate::embedding::embed(&summary)
                .ok()
                .and_then(|v| serde_json::to_string(&v).ok());
            // 同 category + 同内容时提升置信度，否则新增（与 upsert_semantic 一致）
            let existing: Option<i64> = conn
                .query_row(
                    "SELECT id FROM semantic_memories WHERE category = ?1 AND content = ?2 LIMIT 1",
                    params![category, summary],
                    |r| r.get(0),
                )
                .ok();
            let conf = importance.clamp(0.0, 1.0);
            match existing {
                Some(sid) => {
                    let _ = conn.execute(
                        "UPDATE semantic_memories
                         SET confidence = MIN(confidence + ?1, 1.0),
                             source_event_id = COALESCE(source_event_id, ?2)
                         WHERE id = ?3",
                        params![conf * 0.5, id, sid],
                    );
                }
                None => {
                    let _ = conn.execute(
                        "INSERT INTO semantic_memories(category, content, embedding, confidence, source_event_id)
                         VALUES(?1, ?2, ?3, ?4, ?5)",
                        params![category, summary, embedding, conf, id],
                    );
                }
            }
            let _ = conn.execute("UPDATE events SET consolidated = 1 WHERE id = ?1", params![id]);
            count += 1;
        }
        Ok(count)
    }

    /// 写入/更新一条语义记忆（按内容哈希去重，同内容提升置信度）
    pub fn upsert_semantic(
        &self,
        category: &str,
        content: &str,
        confidence: f64,
        source_event_id: Option<i64>,
    ) -> Result<(), String> {
        if content.trim().is_empty() {
            return Ok(());
        }
        let embedding = crate::embedding::embed(content)
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok());
        let conn = self.conn.lock().unwrap();
        // 同 category + 同内容时提升置信度，否则新增
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM semantic_memories WHERE category = ?1 AND content = ?2 LIMIT 1",
                params![category, content],
                |r| r.get(0),
            )
            .ok();
        match existing {
            Some(id) => {
                conn.execute(
                    "UPDATE semantic_memories SET confidence = MIN(confidence + ?1, 1.0)
                     WHERE id = ?2",
                    params![(confidence * 0.5).clamp(0.05, 0.5), id],
                )
                .map_err(|e| e.to_string())?;
            }
            None => {
                conn.execute(
                    "INSERT INTO semantic_memories(category, content, embedding, confidence, source_event_id)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        category,
                        content,
                        embedding,
                        confidence.clamp(0.0, 1.0),
                        source_event_id,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// 列出语义记忆（category 传空串列出全部）
    pub fn list_semantic(&self, category: &str) -> Result<Vec<SemanticMemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = if category.is_empty() {
            "SELECT id, category, content, confidence, source_event_id FROM semantic_memories ORDER BY confidence DESC, id DESC"
        } else {
            "SELECT id, category, content, confidence, source_event_id FROM semantic_memories WHERE category = ?1 ORDER BY confidence DESC, id DESC"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let collect = |r: &rusqlite::Row<'_>| -> rusqlite::Result<SemanticMemoryRow> {
            Ok(SemanticMemoryRow {
                id: r.get(0)?,
                category: r.get(1)?,
                content: r.get(2)?,
                confidence: r.get(3)?,
                source_event_id: r.get(4)?,
            })
        };
        let rows = if category.is_empty() {
            stmt.query_map([], collect)
        } else {
            stmt.query_map(params![category], collect)
        }
        .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// 语义记忆按语义相似度召回（embedding 余弦相似度，回退关键词/全量）
    pub fn recall_semantic(&self, text: &str, limit: usize) -> Result<Vec<SemanticMemoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, category, content, confidence, source_event_id, embedding
                 FROM semantic_memories",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    SemanticMemoryRow {
                        id: r.get(0)?,
                        category: r.get(1)?,
                        content: r.get(2)?,
                        confidence: r.get(3)?,
                        source_event_id: r.get(4)?,
                    },
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        let query_emb = crate::embedding::embed(text).ok();
        let mut scored: Vec<(f64, SemanticMemoryRow)> = Vec::new();
        for r in rows {
            let (m, emb_str) = r.map_err(|e| e.to_string())?;
            let score = match &query_emb {
                Some(q) => {
                    let sim = emb_str
                        .as_ref()
                        .and_then(|s| serde_json::from_str::<Vec<f32>>(s).ok())
                        .map(|e| crate::embedding::cosine(q, &e))
                        .unwrap_or(0.0);
                    sim * m.confidence
                }
                None => {
                    if m.content.contains(text) {
                        m.confidence
                    } else {
                        0.0
                    }
                }
            };
            if score > 0.0 {
                scored.push((score, m));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, m)| m).collect())
    }

    // ---------- 遗忘策略（forget_policy） ----------

    /// 读取某类记忆的遗忘策略，返回 (max_age_days, min_hits)，缺省 (30, 0)
    pub fn get_forget_policy(&self, category: &str) -> Result<(i64, i64), String> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT max_age_days, min_hits FROM forget_policy WHERE category = ?1",
                params![category],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        Ok(row.unwrap_or((30, 0)))
    }

    /// 设置某类记忆的遗忘策略
    pub fn set_forget_policy(
        &self,
        category: &str,
        max_age_days: i64,
        min_hits: i64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO forget_policy(category, max_age_days, min_hits) VALUES(?1, ?2, ?3)
             ON CONFLICT(category) DO UPDATE SET max_age_days = excluded.max_age_days, min_hits = excluded.min_hits",
            params![category, max_age_days.max(1), min_hits.max(0)],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---------- 看护任务持久化（watchdog_tasks / watchdog_runs） ----------

    pub fn list_watchdog_tasks(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM watchdog_tasks ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn upsert_watchdog_task(&self, id: &str, data: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO watchdog_tasks(id, data) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![id, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_watchdog_task(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM watchdog_tasks WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    pub fn upsert_watchdog_run(
        &self,
        run_id: &str,
        task_id: &str,
        started_at: i64,
        data: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO watchdog_runs(id, task_id, started_at, data) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![run_id, task_id, started_at, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_watchdog_runs(
        &self,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, i64, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = if task_id.is_empty() {
            "SELECT id, task_id, started_at, data FROM watchdog_runs ORDER BY started_at DESC LIMIT ?1"
        } else {
            "SELECT id, task_id, started_at, data FROM watchdog_runs WHERE task_id = ?1 ORDER BY started_at DESC LIMIT ?2"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String, i64, String)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        };
        let rows = if task_id.is_empty() {
            stmt.query_map(params![limit as i64], map)
        } else {
            stmt.query_map(params![task_id, limit as i64], map)
        }
        .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn clear_watchdog_runs(&self, task_id: &str) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM watchdog_runs WHERE task_id = ?1", params![task_id])
            .map_err(|e| e.to_string())?;
        Ok(affected)
    }

    // ---------- 技能库持久化（skills） ----------

    pub fn list_skills(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name, data FROM skills ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn upsert_skill(&self, name: &str, data: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO skills(name, data) VALUES(?1, ?2)
             ON CONFLICT(name) DO UPDATE SET data = excluded.data",
            params![name, data],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_skill(&self, name: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM skills WHERE name = ?1", params![name])
            .map_err(|e| e.to_string())?;
        Ok(affected > 0)
    }

    // ---------- 记忆看板总览 ----------

    /// 统计各类记忆条数，供前端记忆看板 / get_memory_overview 命令展示。
    pub fn memory_overview(&self) -> Result<MemoryOverview, String> {
        let conn = self.conn.lock().unwrap();
        let count = |sql: &str| -> usize {
            conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or(0)
                .max(0) as usize
        };
        Ok(MemoryOverview {
            memories: count("SELECT COUNT(*) FROM memories"),
            events: count("SELECT COUNT(*) FROM events"),
            semantic: count("SELECT COUNT(*) FROM semantic_memories"),
            scheduled: count("SELECT COUNT(*) FROM scheduled_jobs"),
            watchdog: count("SELECT COUNT(*) FROM watchdog_tasks"),
        })
    }
}

/// 两个文本的 n-gram 重叠数（话题相似度 / 知识图谱关系）
pub fn ngram_overlap(a: &str, b: &str) -> usize {
    ngrams(a, 2, 3)
        .iter()
        .filter(|g| b.contains(g.as_str()))
        .count()
}

/// 记忆图谱：节点 + 关系边
#[derive(Debug, Clone, Serialize)]
pub struct MemoryGraph {
    pub nodes: Vec<MemoryRow>,
    pub edges: Vec<MemoryEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryEdge {
    pub from: String,
    pub to: String,
    pub weight: f64,
}

/// 时间衰减（遗忘曲线）：指数半衰期，越久越弱
fn decay(age_ms: i64) -> f64 {
    let days = age_ms as f64 / (24.0 * 3600.0 * 1000.0);
    (0.5f64).powf(days / HALF_LIFE_DAYS)
}

/// 生成字符 n-gram（中文无空格分词，用 n-gram 近似关键词匹配）
fn ngrams(text: &str, min_len: usize, max_len: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    for n in min_len..=max_len {
        if chars.len() >= n {
            for i in 0..=(chars.len() - n) {
                let gram: String = chars[i..i + n].iter().collect();
                // 跳过纯空白/标点
                if !gram.chars().all(|c| c.is_whitespace() || c.is_ascii_punctuation()) {
                    out.push(gram);
                }
            }
        }
    }
    out
}

/// 稳定的 64 位哈希（仅用于去重 id，非安全用途）
fn stable_hash(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
