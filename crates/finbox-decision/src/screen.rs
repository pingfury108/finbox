//! 全市场初筛：从本地快照/日K取候选（涨幅 Top N、量比 Top N 去重）。
//!
//! 初筛只读本地 DuckDB，不依赖远端。

use finbox_store::Db;

/// 初筛候选。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub thscode: String,
    pub name: String,
    pub price: f64,
    pub pct: f64,
    pub volume_ratio: Option<f64>,
    pub reason: String,
}

/// 初筛：涨幅 Top N + 量比 Top N（去重）。量比由单条 SQL 计算，避免逐只查询。
pub fn screen(db: &Db, top_n: u32) -> finbox_store::Result<Vec<Candidate>> {
    let snaps = db.latest_snapshots()?;
    let ratios = db.market_volume_ratios()?;
    let ratio_map: std::collections::HashMap<String, f64> =
        ratios.into_iter().map(|r| (r.thscode, r.ratio)).collect();

    let mut by_pct: Vec<&finbox_store::SnapshotView> = snaps
        .iter()
        .filter(|s| s.turnover > 0.0)
        .collect();
    by_pct.sort_by(|a, b| b.pct.partial_cmp(&a.pct).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<Candidate> = Vec::new();
    for s in by_pct.iter().take(top_n as usize) {
        out.push(Candidate {
            thscode: s.thscode.clone(),
            name: ticker_name(db, &s.thscode),
            price: s.last_price,
            pct: s.pct,
            volume_ratio: ratio_map.get(&s.thscode).copied(),
            reason: "涨幅Top".into(),
        });
    }

    // 量比 Top（排除已在涨幅 Top 的）
    let mut scored: Vec<(&finbox_store::SnapshotView, f64)> = Vec::new();
    for s in by_pct.iter() {
        if let Some(r) = ratio_map.get(&s.thscode) {
            scored.push((s, *r));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (s, ratio) in scored.iter().take(top_n as usize) {
        if out.iter().any(|c| c.thscode == s.thscode) {
            continue;
        }
        out.push(Candidate {
            thscode: s.thscode.clone(),
            name: ticker_name(db, &s.thscode),
            price: s.last_price,
            pct: s.pct,
            volume_ratio: Some(*ratio),
            reason: "量比Top".into(),
        });
    }
    Ok(out)
}

/// 近 5 日均成交额与当日成交额之比（近似量比）。
#[allow(dead_code)]
fn volume_ratio(db: &Db, thscode: &str) -> Option<f64> {
    let bars = db.recent_bars(thscode, 5).ok()?;
    if bars.len() < 2 {
        return None;
    }
    let total: f64 = bars.iter().take(4).map(|b| b.close * b.volume).sum();
    let avg = total / (bars.len().saturating_sub(1) as f64);
    if avg <= 0.0 {
        return None;
    }
    let last = bars.last()?;
    Some((last.close * last.volume) / avg)
}

fn ticker_name(db: &Db, thscode: &str) -> String {
    db.ticker_name(thscode).unwrap_or_else(|_| thscode.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use finbox_store::{DailyBarRow, SnapshotRow};

    #[test]
    fn screen_returns_candidates() {
        let db = Db::open(":memory:").unwrap();
        for i in 1..=3 {
            let code = format!("60000{i}.SH");
            db.insert_snapshots(
                i * 1000,
                &[SnapshotRow {
                    thscode: code.clone(),
                    last_price: 10.0 + i as f64,
                    price_change: i as f64,
                    price_change_ratio_pct: i as f64,
                    open_price: 10.0,
                    high_price: 11.0,
                    low_price: 9.0,
                    prev_price: 10.0,
                    volume: 1000.0,
                    turnover: 10000.0 * i as f64,
                }],
            )
            .unwrap();
        }
        let c = screen(&db, 10).unwrap();
        // 3 只都有快照，都进涨幅Top
        assert_eq!(c.len(), 3);
        // 涨幅排序：600003 最高
        assert!(c[0].pct >= c[1].pct);
    }
}
