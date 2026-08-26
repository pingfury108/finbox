//! 账户管理：发现/创建/删除独立账户。
//!
//! 目录结构：
//! ```
//! data/
//! ├── market.duckdb           # 共享行情库
//! └── accounts/
//!     └── <name>/account.duckdb  # 每账户独立状态库
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use finbox_store::{open_account_shared, open_market_shared, SharedDb};

/// 账户库连接缓存：同进程内同账户共享单一 DuckDB 连接。
///
/// 背景：DuckDB 同进程两次 open 同一文件会得到互不可见的独立实例——
/// 账户任务写入的成交，Web 另开连接读不到（今天数据"停在昨天"的根因）。
/// 因此账户任务与 Web 必须共享同一连接句柄。
static ACCOUNT_DB_CACHE: Mutex<Option<HashMap<String, SharedDb>>> = Mutex::new(None);

/// 账户元信息。
#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub name: String,
}

/// 账户目录根。
pub fn accounts_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("accounts")
}

/// 列出所有账户。
pub fn list_accounts(data_dir: &str) -> Result<Vec<AccountInfo>> {
    let root = accounts_root(data_dir);
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let db = entry.path().join("account.duckdb");
        if db.exists() {
            out.push(AccountInfo { name });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// 创建账户（目录 + 账户库 + 初始资金）。
pub fn create_account(data_dir: &str, name: &str, initial_capital: f64) -> Result<AccountInfo> {
    let safe = sanitize_name(name);
    let dir = accounts_root(data_dir).join(&safe);
    std::fs::create_dir_all(&dir).with_context(|| format!("创建账户目录 {dir:?}"))?;
    let db_path = dir.join("account.duckdb");
    if db_path.exists() {
        anyhow::bail!("账户「{safe}」已存在");
    }
    let acct = open_account_shared(&db_path)?;
    acct.lock()
        .unwrap()
        .get_or_init_account(initial_capital)?;
    Ok(AccountInfo { name: safe })
}

/// 删除账户（目录）。
pub fn remove_account(data_dir: &str, name: &str) -> Result<()> {
    let dir = accounts_root(data_dir).join(sanitize_name(name));
    if !dir.exists() {
        anyhow::bail!("账户「{name}」不存在");
    }
    // 先清连接缓存（释放文件句柄），再删目录
    if let Some(cache) = ACCOUNT_DB_CACHE.lock().unwrap().as_mut() {
        cache.remove(&sanitize_name(name));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// 打开账户库（共享句柄，进程内缓存：同账户始终同一连接）。
pub fn open_account(data_dir: &str, name: &str) -> Result<SharedDb> {
    let safe = sanitize_name(name);
    let dir = accounts_root(data_dir).join(&safe);
    let db = dir.join("account.duckdb");
    if !db.exists() {
        anyhow::bail!("账户「{name}」不存在");
    }
    let mut guard = ACCOUNT_DB_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(db) = cache.get(&safe) {
        return Ok(db.clone());
    }
    let shared = open_account_shared(&db)?;
    cache.insert(safe, shared.clone());
    Ok(shared)
}

/// 打开行情库（共享句柄）。
pub fn open_market(data_dir: &str) -> Result<SharedDb> {
    let path = Path::new(data_dir).join("market.duckdb");
    Ok(open_market_shared(path)?)
}

/// 账户名安全化（仅保留字母数字中文下划线）。
fn sanitize_name(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if s.is_empty() {
        "account".into()
    } else {
        s
    }
}
