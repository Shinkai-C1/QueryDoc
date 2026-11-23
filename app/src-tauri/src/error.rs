//! 统一错误类型：业务逻辑返回 [`AppError`]，Tauri command 层转为 `String`。

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("无效输入: {0}")]
    InvalidInput(String),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("非法路径（目录遍历）: {0}")]
    PathTraversal(String),

    #[error("文件校验失败: {0}")]
    Validation(String),

    #[error("回收站操作失败: {0}")]
    Trash(String),

    #[error("内部错误: {0}")]
    Internal(String),

    #[error("LLM 调用失败: {0}")]
    Llm(String),

    #[error("xlsx 生成失败: {0}")]
    Xlsx(String),
}

impl From<rust_xlsxwriter::XlsxError> for AppError {
    fn from(e: rust_xlsxwriter::XlsxError) -> Self {
        AppError::Xlsx(e.to_string())
    }
}

impl From<trash::Error> for AppError {
    fn from(e: trash::Error) -> Self {
        AppError::Trash(e.to_string())
    }
}

/// command 薄封装统一使用：把 AppError 转成前端可见的字符串。
pub fn to_string_err(e: AppError) -> String {
    e.to_string()
}
