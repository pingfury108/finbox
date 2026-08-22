use std::time::Duration;

use serde::de::{DeserializeOwned, Error as _};

use crate::error::api_error;
use crate::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://fuyao.aicubes.cn";
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// 大文件下载：单次整体超时与空闲看门狗。
const DOWNLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(900);
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 业务响应信封。HTTP 200 不代表成功，必须判 `code == 0`。
#[derive(Debug, serde::Deserialize)]
struct Envelope<T> {
    code: i32,
    #[serde(default)]
    message: Option<String>,
    #[serde(default, rename = "request_id")]
    request_id: Option<String>,
    data: Option<T>,
}

/// SDK 客户端：认证、信封解析、限流/服务端错误退避重试。
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        // 同花顺为国内服务，绕过系统代理（HTTPS_PROXY 等）直连，避免代理转发拖慢大文件下载
        // 强制 HTTP/1.1：实测 CDN 的 h2 链路下大文件响应无 content-length 且流不关闭，会永久挂起
        let http = reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { http, base_url: DEFAULT_BASE_URL.to_string(), api_key: api_key.into() })
    }

    /// 从环境变量 `HITHINK_FINANCE_API_KEY` 构造。
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("HITHINK_FINANCE_API_KEY")
            .map_err(|_| Error::MissingApiKey)?
            .trim()
            .to_string();
        Self::new(key)
    }

    /// 覆盖 base URL（测试用）。
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// GET 并解析信封。网络错误与 `4001`/`5xxx`/HTTP 5xx 指数退避重试。
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let mut last_err: Option<Error> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = INITIAL_BACKOFF * (1_u32 << (attempt - 1)).min(8);
                tokio::time::sleep(backoff).await;
            }

            let resp = match self.http.get(&url).header("X-api-key", &self.api_key).query(query).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(Error::from(e));
                    continue; // 网络错误，退避重试
                }
            };

            let status = resp.status();
            let body = resp.text().await?;
            if !status.is_success() {
                let err = Error::Status { status: status.as_u16(), url: url.clone() };
                if err.is_retryable() {
                    last_err = Some(err);
                    continue;
                }
                return Err(err);
            }

            let env: Envelope<T> = serde_json::from_str(&body)?;

            return match (env.code, env.data) {
                (0, Some(data)) => Ok(data),
                (0, None) => Err(Error::Parse(serde_json::Error::custom("code=0 但 data 缺失"))),
                (code, _) => {
                    let err = api_error(
                        code,
                        env.message.unwrap_or_default(),
                        env.request_id.unwrap_or_default(),
                    );
                    if err.is_retryable() && attempt < MAX_RETRIES {
                        last_err = Some(err);
                        continue;
                    }
                    Err(err)
                }
            };
        }

        Err(last_err.unwrap_or_else(|| api_error(-1, "重试次数耗尽", "")))
    }

    /// 流式下载大文件到本地路径（用于 Market Dumps Parquet）。
    ///
    /// 优先按 `Content-Length` 判断收尾（实测 CDN 在 body 结束后可能不关连接，
    /// 若只等流结束会永久挂起）；无 `Content-Length` 时退化为等流结束。
    /// 空闲看门狗：超过 60 秒无数据即中断返回 [`Error::Stalled`]，
    /// 由上层（`download_dump`）重新签 URL 重试。
    pub async fn download_to_file(&self, url: &str, dest: &std::path::Path) -> Result<()> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let resp = self
            .http
            .get(url)
            .timeout(DOWNLOAD_TOTAL_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        let expected = resp.content_length();
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = resp.bytes_stream();
        let mut received: u64 = 0;
        loop {
            match tokio::time::timeout(DOWNLOAD_IDLE_TIMEOUT, stream.next()).await {
                Ok(Some(chunk)) => {
                    let chunk = chunk?;
                    file.write_all(&chunk).await?;
                    received += chunk.len() as u64;
                    if let Some(total) = expected {
                        if received >= total {
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => return Err(Error::Stalled(DOWNLOAD_IDLE_TIMEOUT.as_secs())),
            }
        }
        file.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Data {
        item: Vec<String>,
    }

    #[test]
    fn envelope_success() {
        let body = json!({"code": 0, "message": null, "request_id": "r1", "data": {"item": ["a"]}});
        let env: Envelope<Data> = serde_json::from_value(body).unwrap();
        assert_eq!(env.code, 0);
        assert_eq!(env.data.unwrap().item, vec!["a"]);
    }

    #[test]
    fn envelope_business_error() {
        let body = json!({"code": 3001, "message": "标的不存在", "request_id": "r2", "data": null});
        let env: Envelope<Data> = serde_json::from_value(body).unwrap();
        assert_eq!(env.code, 3001);
        assert!(env.data.is_none());
    }

    #[test]
    fn retryable_classification() {
        assert!(api_error(4001, "", "").is_retryable());
        assert!(api_error(5002, "", "").is_retryable());
        assert!(!api_error(1001, "", "").is_retryable());
        assert!(!api_error(2003, "", "").is_retryable());
        assert!(!api_error(3002, "", "").is_retryable());
        assert!(Error::Status { status: 502, url: String::new() }.is_retryable());
        assert!(!Error::Status { status: 404, url: String::new() }.is_retryable());
    }
}
