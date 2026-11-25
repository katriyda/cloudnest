use crate::views::ErrorPage;
use askama::Template;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::fmt;

#[derive(Debug)]
pub enum AppError {
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    BadRequest(String),
    Internal(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "未登录"),
            Self::Forbidden(m) | Self::NotFound(m) | Self::BadRequest(m) | Self::Internal(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for AppError {}

impl AppError {
    pub fn internal<E: fmt::Display>(err: E) -> Self {
        Self::Internal(format!("{err}"))
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        tracing::error!("数据库错误: {value}");
        Self::Internal("数据库操作失败".to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        tracing::error!("文件系统错误: {value}");
        Self::Internal("文件操作失败".to_string())
    }
}

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(value: axum::extract::multipart::MultipartError) -> Self {
        Self::BadRequest(format!("上传没有完成，请重试。详情: {value}"))
    }
}

impl From<askama::Error> for AppError {
    fn from(value: askama::Error) -> Self {
        Self::Internal(format!("模板渲染失败: {value}"))
    }
}

impl From<axum::http::Error> for AppError {
    fn from(value: axum::http::Error) -> Self {
        Self::Internal(format!("响应构建失败: {value}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, title, message) = match self {
            Self::Unauthorized => {
                return (StatusCode::SEE_OTHER, [(header::LOCATION, "/login")]).into_response();
            }
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, "没有访问权限", message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, "内容不可用", message),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "操作没有完成", message),
            Self::Internal(message) => {
                tracing::error!("内部错误: {message}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "出了点问题",
                    "请稍后重试。".to_string(),
                )
            }
        };
        let page = ErrorPage {
            status: status.as_u16(),
            title: title.to_string(),
            message,
        };
        match page.render() {
            Ok(html) => (
                status,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html,
            )
                .into_response(),
            Err(err) => {
                tracing::error!("错误页渲染失败: {err}");
                (status, "出了点问题").into_response()
            }
        }
    }
}
