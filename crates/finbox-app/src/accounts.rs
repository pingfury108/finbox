//! 账户管理：发现/创建/删除独立账户。
//!
//! 目录结构：
//! ```
//! data/
//! ├── market.duckdb           # 共享行情库
//! └── accounts/
//!     └── <name>/account.duckdb  # 每账户独立状态库
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use finbox_store::{open_account_shared, open_market_shared, SharedDb};

/// 账户元信息。
#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub name: String,
    /// account.duckdb 路径
    pub db_path: PathBuf,
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
            out.push(AccountInfo { name, db_path: db });
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
    Ok(AccountInfo { name: safe, db_path })
}

/// 删除账户（目录）。
pub fn remove_account(data_dir: &str, name: &str) -> Result<()> {
    let dir = accounts_root(data_dir).join(sanitize_name(name));
    if !dir.exists() {
        anyhow::bail!("账户「{name}」不存在");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// 打开账户库（共享句柄）。
pub fn open_account(data_dir: &str, name: &str) -> Result<SharedDb> {
    let dir = accounts_root(data_dir).join(sanitize_name(name));
    let db = dir.join("account.duckdb");
    if !db.exists() {
        anyhow::bail!("账户「{name}」不存在");
    }
    Ok(open_account_shared(db)?)
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
