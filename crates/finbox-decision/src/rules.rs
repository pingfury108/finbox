//! 参数进化的回测裁决（P4 规则闭环核心）。
//!
//! 原则：**数据当法官**。在历史样本上评估每个候选参数值的期望收益，
//! 只有改善超过阈值才采纳——避免在噪声上反复横跳。
//!
//! 四个可调维度（每个都是小范围离散候选，防止过拟合）：
//!  1. 回调型配额 pullback_share ∈ {0.25, 0.5, 0.75}
//!  2. 候选最低评分 min_score ∈ {0.0, 0.5, 0.6, 0.7}
//!  3. 量比偏好 volume_pref ∈ {shrink（缩量低吸）, neutral, expand（放量突破）}
//!  4. risk-off 仓位上限 riskoff_position ∈ {0.15, 0.30, 0.45}

/// 一条已回填的样本。
#[derive(Debug, Clone)]
pub struct Outcome {
    pub regime: String,
    pub candidate_type: String,
    pub score: Option<f64>,
    pub volume_ratio: Option<f64>,
    pub day_pct: Option<f64>,
    pub ret_d5: f64,
}

/// 单维裁决结果。
#[derive(Debug, Clone)]
pub struct Verdict {
    pub key: &'static str,
    pub current: f64,
    pub best: f64,
    /// 当前参数下的期望 D5（%）
    pub cur_exp: f64,
    /// 最优参数下的期望 D5（%）
    pub best_exp: f64,
    /// 参与裁决的样本数
    pub samples: usize,
    /// 是否建议采纳（改善超阈值）
    pub accept: bool,
}

/// 采纳阈值（D5 均值改善，百分点）
const ACCEPT_DELTA: f64 = 0.3;
/// 单维裁决的最小样本数（低于此不下结论）
const MIN_SAMPLES: usize = 20;

impl Verdict {
    pub fn log(&self) -> String {
        format!(
            "[规则闭环] {}: {} → {}（期望 {:+.2}% → {:+.2}%，{} 样本）{}",
            self.key,
            self.current,
            self.best,
            self.cur_exp,
            self.best_exp,
            self.samples,
            if self.accept { " 已采纳" } else { " 否决（改善不足）" }
        )
    }
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

fn filter_mean(samples: &[&Outcome], pred: impl Fn(&Outcome) -> bool) -> (usize, f64) {
    let hits: Vec<f64> = samples.iter().filter(|o| pred(o)).map(|o| o.ret_d5).collect();
    (hits.len(), mean(&hits))
}

/// ① 回调型配额：组合期望 = share×回调均值 + (1-share)×强势均值
pub fn search_pullback_share(samples: &[Outcome], current: f64) -> Option<Verdict> {
    let pb: Vec<&Outcome> = samples.iter().filter(|o| o.candidate_type == "回调型").collect();
    let mo: Vec<&Outcome> = samples.iter().filter(|o| o.candidate_type == "强势型").collect();
    if pb.len() < MIN_SAMPLES || mo.len() < MIN_SAMPLES {
        return None;
    }
    let (pn, pa) = (pb.len(), mean(&pb.iter().map(|o| o.ret_d5).collect::<Vec<_>>()));
    let (mn, ma) = (mo.len(), mean(&mo.iter().map(|o| o.ret_d5).collect::<Vec<_>>()));
    let exp = |share: f64| share * pa + (1.0 - share) * ma;
    let mut best = current;
    let mut best_exp = exp(current);
    for share in [0.25, 0.5, 0.75] {
        let e = exp(share);
        if e > best_exp {
            best = share;
            best_exp = e;
        }
    }
    Some(Verdict {
        key: "pullback_share",
        current,
        best,
        cur_exp: exp(current),
        best_exp,
        samples: pn + mn,
        accept: (best - current).abs() > 1e-9 && best_exp > exp(current) + ACCEPT_DELTA,
    })
}

/// ② 候选最低评分门槛：只保留 score ≥ threshold 的样本，看均值是否更好
pub fn search_min_score(samples: &[Outcome], current: f64) -> Option<Verdict> {
    let exp = |th: f64| -> (usize, f64) {
        filter_mean(&samples.iter().collect::<Vec<_>>(), |o| {
            o.score.map(|s| s >= th).unwrap_or(false)
        })
    };
    let (cur_n, cur_exp) = exp(current);
    if cur_n < MIN_SAMPLES {
        return None;
    }
    let mut best = current;
    let mut best_exp = cur_exp;
    let mut best_n = cur_n;
    for th in [0.0, 0.5, 0.6, 0.7] {
        let (n, e) = exp(th);
        if n < MIN_SAMPLES {
            continue; // 门槛太高导致样本不足，不选
        }
        if e > best_exp {
            best = th;
            best_exp = e;
            best_n = n;
        }
    }
    Some(Verdict {
        key: "min_score",
        current,
        best,
        cur_exp,
        best_exp,
        samples: best_n,
        accept: (best - current).abs() > 1e-9 && best_exp > cur_exp + ACCEPT_DELTA,
    })
}

/// ③ 量比偏好：缩量低吸 / 中性 / 放量突破
pub fn search_volume_pref(samples: &[Outcome], current: f64) -> Option<Verdict> {
    // current 编码：0=shrink, 1=neutral, 2=expand
    let bucket = |o: &Outcome| -> Option<f64> {
        o.volume_ratio.map(|v| if v < 1.2 { 0.0 } else if v <= 1.8 { 1.0 } else { 2.0 })
    };
    let exp = |b: f64| -> (usize, f64) {
        filter_mean(&samples.iter().collect::<Vec<_>>(), |o| bucket(o) == Some(b))
    };
    let (cur_n, cur_exp) = exp(current);
    let mut best = current;
    let mut best_exp = if cur_n >= MIN_SAMPLES { cur_exp } else { f64::NEG_INFINITY };
    let mut best_n = cur_n;
    for b in [0.0, 1.0, 2.0] {
        let (n, e) = exp(b);
        if n < MIN_SAMPLES {
            continue;
        }
        if e > best_exp {
            best = b;
            best_exp = e;
            best_n = n;
        }
    }
    if best_exp == f64::NEG_INFINITY {
        return None; // 三个档位样本全不足
    }
    Some(Verdict {
        key: "volume_pref",
        current,
        best,
        cur_exp,
        best_exp,
        samples: best_n,
        accept: (best - current).abs() > 1e-9 && best_exp > cur_exp + ACCEPT_DELTA,
    })
}

/// ④ risk-off 仓位上限：**保守优先**。
/// risk-off 本意就是降风险，不能因为历史样本偶然为正就放宽仓位——
/// 那与“不亏太多”的第一目标冲突。
/// 规则：仅当 risk-off 样本明显为负（<-1%）时收紧到 0.15；其余保持 0.30。
pub fn search_riskoff_position(samples: &[Outcome], current: f64) -> Option<Verdict> {
    let (n, e) = filter_mean(&samples.iter().collect::<Vec<_>>(), |o| o.regime == "risk-off");
    if n < MIN_SAMPLES {
        return None;
    }
    let best = if e < -1.0 { 0.15 } else { 0.30 };
    Some(Verdict {
        key: "riskoff_position",
        current,
        best,
        cur_exp: e,
        best_exp: e,
        samples: n,
        accept: (best - current).abs() > 1e-9,
    })
}

/// 全部四维裁决。
pub fn search_all(samples: &[Outcome], current: [f64; 4]) -> Vec<Verdict> {
    [
        search_pullback_share(samples, current[0]),
        search_min_score(samples, current[1]),
        search_volume_pref(samples, current[2]),
        search_riskoff_position(samples, current[3]),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(ctype: &str, regime: &str, score: f64, vr: f64, ret: f64) -> Outcome {
        Outcome {
            regime: regime.into(),
            candidate_type: ctype.into(),
            score: Some(score),
            volume_ratio: Some(vr),
            day_pct: Some(1.0),
            ret_d5: ret,
        }
    }

    #[test]
    fn pullback_share_prefers_better_type() {
        let mut s = Vec::new();
        for _ in 0..25 { s.push(mk("回调型", "risk-on", 0.8, 1.0, 1.2)); }
        for _ in 0..25 { s.push(mk("强势型", "risk-on", 0.8, 2.5, -3.2)); }
        let v = search_pullback_share(&s, 0.5).unwrap();
        assert!(v.accept);
        assert!((v.best - 0.75).abs() < 1e-9, "应选 0.75，实际 {}", v.best);
    }

    #[test]
    fn pullback_share_rejects_thin_samples() {
        let s = vec![mk("回调型", "risk-on", 0.8, 1.0, 1.0); 5];
        assert!(search_pullback_share(&s, 0.5).is_none());
    }

    #[test]
    fn min_score_raises_threshold_when_high_scores_better() {
        let mut s = Vec::new();
        for _ in 0..25 { s.push(mk("回调型", "risk-on", 0.85, 1.0, 2.0)); } // 高分好
        for _ in 0..25 { s.push(mk("回调型", "risk-on", 0.45, 1.0, -1.5)); } // 低分差
        let v = search_min_score(&s, 0.0).unwrap();
        assert!(v.accept, "应提高门槛: {:?}", v);
        assert!(v.best >= 0.5, "best={}", v.best);
    }

    #[test]
    fn volume_pref_detects_shrink_advantage() {
        let mut s = Vec::new();
        for _ in 0..25 { s.push(mk("回调型", "neutral", 0.7, 0.8, 1.5)); } // 缩量好
        for _ in 0..25 { s.push(mk("强势型", "neutral", 0.7, 2.5, -2.0)); } // 放量差
        let v = search_volume_pref(&s, 1.0).unwrap();
        assert!(v.accept);
        assert!((v.best - 0.0).abs() < 1e-9, "应选缩量，实际 {}", v.best);
    }

    #[test]
    fn riskoff_tightens_when_negative() {
        let s: Vec<Outcome> = (0..25).map(|_| mk("回调型", "risk-off", 0.7, 1.0, -2.0)).collect();
        let v = search_riskoff_position(&s, 0.30).unwrap();
        assert!(v.accept);
        assert!((v.best - 0.15).abs() < 1e-9, "负期望应收紧到 0.15");
    }

    #[test]
    fn riskoff_never_loosens_on_positive_samples() {
        // 历史样本偶然为正也不能放宽 risk-off 仓位（与“不亏太多”冲突）
        let s: Vec<Outcome> = (0..25).map(|_| mk("回调型", "risk-off", 0.7, 1.0, 2.0)).collect();
        let v = search_riskoff_position(&s, 0.30).unwrap();
        assert!(!v.accept, "正期望不应放宽仓位: best={}", v.best);
        assert!((v.best - 0.30).abs() < 1e-9);
    }
}
