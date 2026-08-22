//! 全市场数据导出（Market Dumps）：S3 预签名 URL 签发与 Parquet 下载。
//!
//! 全市场建库/增量同步必须走本模块，**不要**逐股请求 `price_historical`
//! （全市场 5000+ 只票，逐只需数千次请求；本模块 3 次请求即可）。
//!
//! 预签名 URL 有效期约 5 分钟，签发后立即下载，不要缓存 URL。

use std::path::Path;

use crate::{Client, Result};

/// Dump 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpKind {
    /// 全市场 10 年日 K（未复权），~945 万行，首次全量
    DailyK,
    /// 全市场最近 10 个交易日日 K，~25 万行，日常增量
    DailyK10d,
    /// 全市场复权事件（分红/送股/配股），~5.2 万行
    AdjustmentFactors,
}

impl DumpKind {
    fn path(self) -> &'static str {
        match self {
            DumpKind::DailyK => "/api/dump/market-dumps/daily-k/download-url",
            DumpKind::DailyK10d => "/api/dump/market-dumps/daily-k-10d/download-url",
            DumpKind::AdjustmentFactors => {
                "/api/dump/market-dumps/adjustment-factors/download-url"
            }
        }
    }

    /// 建议的落盘文件名。
    pub fn default_filename(self) -> &'static str {
        match self {
            DumpKind::DailyK => "daily-k-full.parquet",
            DumpKind::DailyK10d => "daily-k-10d.parquet",
            DumpKind::AdjustmentFactors => "adjustment-factors.parquet",
        }
    }
}

/// 预签名下载 URL。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DownloadUrl {
    /// S3 预签名下载链接，有效期约 5 分钟
    pub presigned_url: String,
    /// URL 过期时间（ISO 8601 UTC）
    pub presigned_url_expires_at: String,
}

impl Client {
    /// 签发 dump 下载 URL（有效期约 5 分钟，勿缓存）。
    pub async fn dump_download_url(&self, kind: DumpKind) -> Result<DownloadUrl> {
        self.get(kind.path(), &[]).await
    }

    /// 签发并立即下载 Parquet 到 `dest`。
    ///
    /// 本地数据落后超过 7 个交易日时，`DailyK10d` 无法覆盖缺口，应改用 `DailyK` 全量重拉。
    /// 签发或下载失败时自动重签 URL 重试（共 3 次）。
    pub async fn download_dump(&self, kind: DumpKind, dest: &Path) -> Result<DownloadUrl> {
        let mut last_err: Option<crate::Error> = None;
        for attempt in 0..=2u32 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500 * (1 << (attempt - 1)))).await;
            }
            let url = match self.dump_download_url(kind).await {
                Ok(u) => u,
                Err(e) => {
                    if e.is_retryable() && attempt < 2 {
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            };
            match self.download_to_file(&url.presigned_url, dest).await {
                Ok(()) => return Ok(url),
                Err(e) => {
                    if e.is_retryable() && attempt < 2 {
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| crate::error::api_error(-1, "下载重试次数耗尽", "")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_url_parses() {
        let json = r#"{
            "presigned_url": "https://s3.example/bucket/x.parquet?sig=abc",
            "presigned_url_expires_at": "2025-08-22T00:05:00Z"
        }"#;
        let url: DownloadUrl = serde_json::from_str(json).unwrap();
        assert!(url.presigned_url.starts_with("https://s3.example"));
    }

    #[test]
    fn kind_paths() {
        assert!(DumpKind::DailyK.path().contains("daily-k/download-url"));
        assert_eq!(DumpKind::DailyK10d.default_filename(), "daily-k-10d.parquet");
    }
}
