//! 除权除息处理：持仓过除权日时调整数量/成本，现金分红入账（含红利税）。
//!
//! 真实规则：
//! - 现金分红：每股派息 × 持仓数入账，红利税按持有期（<1月 20% / 1月-1年 10% / >1年 免）
//! - 送股/转增：数量按比例增加，成本同比例摊薄（10送1: 数量×1.1, 成本÷1.1）
//! - 配股：需掏钱认购；模拟盘自动放弃（不调整，记日志）

use finbox_store::Db;

/// 红利税率：按持有期（除权日 - 最早买入日）。
fn dividend_tax_rate(hold_ms: i64) -> f64 {
    const MONTH: i64 = 30 * 86_400_000;
    const YEAR: i64 = 365 * 86_400_000;
    if hold_ms >= YEAR {
        0.0
    } else if hold_ms >= MONTH {
        0.10
    } else {
        0.20
    }
}

/// 应用所有持仓的待处理除权事件。返回处理的事件数。
/// `today_ms` 为今日（Asia/Shanghai 零点）毫秒戳；ex_date <= today 的事件生效。
pub fn apply_pending_adjustments(acct: &Db, market: &Db, today_ms: i64) -> finbox_store::Result<u32> {
    let positions = acct.positions()?;
    let mut applied = 0u32;
    for p in positions {
        let events = market.adjustments_for(&p.thscode, today_ms)?;
        for (ex_date, dividend, bonus, allot_ratio, _allot_price) in events {
            if acct.is_adjustment_processed(&p.thscode, ex_date)? {
                continue;
            }
            let mut qty = p.quantity;
            let mut cost = p.avg_cost;

            // 送股/转增：数量增加、成本摊薄
            if bonus > 0.0 {
                qty = (qty as f64 * (1.0 + bonus)).round() as u32;
                cost = cost / (1.0 + bonus);
                log::info!(
                    "[除权] {} 送股 {:.0}%：{}股→{}股，成本 {:.3}→{:.3}",
                    p.thscode, bonus * 100.0, p.quantity, qty, p.avg_cost, cost
                );
            }
            if qty != p.quantity || (cost - p.avg_cost).abs() > 1e-9 {
                acct.upsert_position(&finbox_core::Position {
                    thscode: p.thscode.clone(),
                    name: p.name.clone(),
                    quantity: qty,
                    avg_cost: (cost * 1000.0).round() / 1000.0,
                })?;
            }

            // 现金分红：税后入账（持有期按最早买入到除权日）
            if dividend > 0.0 {
                let hold_ms = acct.first_buy_ms(&p.thscode)?.map(|b| ex_date - b).unwrap_or(0);
                let tax = dividend_tax_rate(hold_ms);
                let gross = dividend * qty as f64;
                let net = (gross * (1.0 - tax) * 100.0).round() / 100.0;
                acct.add_cash(net)?;
                log::info!(
                    "[除权] {} 分红每股 {:.3}元 × {}股 = {:.2}元（红利税 {:.0}% 后 {:.2}元入账）",
                    p.thscode, dividend, qty, gross, tax * 100.0, net
                );
            }

            // 配股：自动放弃（需掏钱认购，模拟盘不参与，接受摊薄失真）
            if let Some(ratio) = allot_ratio {
                if ratio > 0.0 {
                    log::warn!("[除权] {} 有配股（比例 {:.2}），模拟盘自动放弃", p.thscode, ratio);
                }
            }

            acct.mark_adjustment_processed(&p.thscode, ex_date)?;
            applied += 1;
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use finbox_store::{open_account_shared, open_market_shared};

    #[test]
    fn dividend_tax_by_holding() {
        assert_eq!(dividend_tax_rate(10 * 86_400_000), 0.20); // <1月
        assert_eq!(dividend_tax_rate(60 * 86_400_000), 0.10); // 1月-1年
        assert_eq!(dividend_tax_rate(400 * 86_400_000), 0.0); // >1年
    }

    #[test]
    fn bonus_split_adjusts_position() {
        let market = open_market_shared(":memory:").unwrap();
        let acct = open_account_shared(":memory:").unwrap();
        // 造一只持仓 + 一条送股事件（10送1）
        {
            let a = acct.lock().unwrap();
            a.get_or_init_account(100_000.0).unwrap();
            a.upsert_position(&finbox_core::Position {
                thscode: "600519.SH".into(),
                name: "贵州茅台".into(),
                quantity: 100,
                avg_cost: 1000.0,
            })
            .unwrap();
        }
        let today = 1_800_000_000_000i64;
        {
            let m = market.lock().unwrap();
            m.conn().execute(
                "INSERT INTO adjustment_events VALUES ('600519.SH', ?, 0.0, 0.1, NULL, NULL)",
                duckdb::params![today - 86_400_000],
            ).unwrap();
        }
        let n = {
            let a = acct.lock().unwrap();
            let m = market.lock().unwrap();
            apply_pending_adjustments(&a, &m, today).unwrap()
        };
        assert_eq!(n, 1);
        let p = acct.lock().unwrap().position("600519.SH").unwrap().unwrap();
        assert_eq!(p.quantity, 110);
        assert!((p.avg_cost - 909.091).abs() < 0.01);
        // 重复应用应跳过
        let n2 = {
            let a = acct.lock().unwrap();
            let m = market.lock().unwrap();
            apply_pending_adjustments(&a, &m, today).unwrap()
        };
        assert_eq!(n2, 0);
    }
}
