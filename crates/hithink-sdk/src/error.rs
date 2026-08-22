use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    /// 缺少 API Key
    #[error("缺少 API Key，请设置 HITHINK_FINANCE_API_KEY")]
    MissingApiKey,
    /// 网络层错误（连接失败、超时等）
    #[error("网络错误: {0}")]
    Http(#[from] reqwest::Error),
    /// HTTP 状态码非 2xx
    #[error("HTTP 状态错误 {status}: {url}")]
    Status { status: u16, url: String },
    /// 响应体反序列化失败
    #[error("响应解析失败: {0}")]
    Parse(#[from] serde_json::Error),
    /// 本地文件 IO 错误（下载落盘等）
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 大文件下载停滞（长时间无数据）
    #[error("下载停滞: 超过 {0} 秒未收到数据")]
    Stalled(u64),
    /// 业务错误：HTTP 200 但信封 code != 0
    #[error("API 错误 code={code}: {message} (request_id={rid})")]
    Api { code: i32, message: String, rid: String },
}

impl Error {
    /// 业务错误码。
    pub fn api_code(&self) -> Option<i32> {
        match self {
            Error::Api { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// 是否可安全重试：限流(4001)、服务端异常(5xxx)、网络错误、HTTP 5xx。
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Http(_) | Error::Stalled(_) | Error::Api { code: 4001, .. } => true,
            Error::Api { code, .. } => *code >= 5000,
            Error::Status { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

pub(crate) fn api_error(code: i32, message: impl Into<String>, rid: impl Into<String>) -> Error {
    Error::Api { code, message: message.into(), rid: rid.into() }
}
