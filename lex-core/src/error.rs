use thiserror::Error;

#[derive(Debug, Error)]
pub enum LexError {
    #[error("配置错误: {0}")]
    Config(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("网络错误: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Provider 协议错误: {0}")]
    Provider(String),
    #[error("工具执行失败: {0}")]
    Tool(String),
    #[error("权限被拒绝: {0}")]
    PermissionDenied(String),
}

pub type Result<T> = std::result::Result<T, LexError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_displays_chinese_prefix() {
        let e = LexError::Config("缺少 base_url".into());
        assert_eq!(e.to_string(), "配置错误: 缺少 base_url");
    }
}
