//! finbox-store 决策留痕：LLM 决策的上下文、原始输出、解析动作全程记录。

use crate::{Db, Result};

/// 一条决策日志。
#[derive(Debug, Clone)]
pub struct DecisionLog {
    pub id: i64,
    pub ts_ms: i64,
    pub model: String,
    pub context: String,
    pub raw_response: String,
    pub actions: String,
    pub status: String,
    pub note: String,
}

impl Db {
    /// 插入一条决策日志，返回 id。
    pub fn insert_decision_log(&self, log: &DecisionLog) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO decision_logs (ts_ms, model, context, raw_response, actions, status, note)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                log.ts_ms, log.model, log.context, log.raw_response, log.actions, log.status, log.note
            ],
        )?;
        let id = self
            .conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get::<_, i64>(0))?;
        Ok(id)
    }

    /// 最近 N 条已执行决策（供 AI 复盘反馈）。
    pub fn recent_executed_decisions(&self, limit: u32) -> Result<Vec<DecisionLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, model, context, raw_response, actions, status, note
             FROM decision_logs WHERE status = 'executed'
             ORDER BY ts_ms DESC LIMIT ?",
        )?;
        let mut rows = stmt.query(duckdb::params![limit as i64])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(DecisionLog {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                model: r.get(2)?,
                context: r.get(3)?,
                raw_response: r.get(4)?,
                actions: r.get(5)?,
                status: r.get(6)?,
                note: r.get(7)?,
            });
        }
        Ok(out)
    }
}
