use crate::auth::{self, AdminUser};
use crate::config::{Config, ConfigError};
use crate::error::AppError;
use crate::state::AppState;
use crate::update::{self, UpdateService};
use crate::views::{SettingsPage, SettingsTemplate};
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
pub struct SettingsForm {
    pub csrf: String,
    pub listen: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default)]
    pub secure_cookies: Option<String>,
    pub max_upload_size_mb: String,
    pub default_quota_gb: String,
    pub session_days: String,
}

#[derive(Deserialize)]
pub struct CsrfForm {
    pub csrf: String,
}

impl From<ConfigError> for AppError {
    fn from(value: ConfigError) -> Self {
        AppError::BadRequest(value.0)
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "ok": false, "message": message.into() })),
    )
        .into_response()
}

fn env_names() -> Vec<String> {
    Config::env_overrides()
        .into_iter()
        .map(|name| {
            let var = format!("CLOUDNEST_{}", name.to_uppercase());
            format!("{name}（{var}）")
        })
        .collect()
}

pub async fn index(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(query): Query<crate::routes::admin::AdminQuery>,
) -> Result<Response, AppError> {
    let common = crate::views::build_common(
        &state,
        &admin.0,
        "settings",
        query.ok.as_deref(),
        query.err.as_deref(),
    )
    .await?;
    let running = state.config();
    let (file_config, config_path) = match state.config_path() {
        Some(path) => {
            let file = Config::load_raw(path).unwrap_or_else(|_| (*running).clone());
            (file, path.display().to_string())
        }
        None => ((*running).clone(), String::new()),
    };
    let listen_pending = file_config.listen != running.listen;
    let page = SettingsPage {
        common,
        current_version: crate::cli::VERSION.to_string(),
        update_supported: update::web_update_supported(),
        platform_label: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        config_path,
        listen: file_config.listen.clone(),
        public_url: file_config.public_url.clone().unwrap_or_default(),
        secure_cookies: file_config.secure_cookies,
        max_upload_size_mb: file_config.max_upload_size_mb,
        default_quota_gb: file_config.default_quota_gb,
        session_days: file_config.session_days,
        running_listen: running.listen.clone(),
        listen_pending,
        data_dir: running.data_dir.display().to_string(),
        env_overrides: env_names(),
    };
    super::render(&SettingsTemplate { page }, StatusCode::OK).await
}

fn parse_number<T>(value: &str, label: &str) -> Result<T, AppError>
where
    T: std::str::FromStr,
{
    value
        .trim()
        .parse::<T>()
        .map_err(|_| AppError::BadRequest(format!("{label}格式不正确")))
}

pub async fn save(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&admin.0, &form.csrf)?;
    let path = state.config_path().ok_or_else(|| {
        AppError::BadRequest("当前运行方式没有配置文件，无法保存设置".to_string())
    })?;
    let mut file_config = Config::load_raw(path)?;
    file_config.listen = form.listen.trim().to_string();
    let public_url = form.public_url.trim().trim_end_matches('/').to_string();
    file_config.public_url = if public_url.is_empty() {
        None
    } else {
        Some(public_url)
    };
    file_config.secure_cookies = form.secure_cookies.is_some();
    file_config.max_upload_size_mb = parse_number(&form.max_upload_size_mb, "单次请求体上限")?;
    file_config.default_quota_gb = parse_number(&form.default_quota_gb, "默认存储配额")?;
    file_config.session_days = parse_number(&form.session_days, "会话有效期")?;
    file_config.normalize();
    file_config.validate()?;
    file_config.save(path)?;

    // 让新配置立即生效；监听地址与数据目录的变更需要重启进程才能应用。
    let startup = state.config();
    let mut runtime = file_config.clone();
    runtime.apply_env();
    runtime.normalize();
    runtime.listen = startup.listen.clone();
    runtime.data_dir = startup.data_dir.clone();
    state.replace_config(runtime);

    let mut notes = Vec::new();
    let overridden = Config::env_overrides();
    if !overridden.is_empty() {
        notes.push(format!(
            "{} 由环境变量覆盖，网页保存的值不会生效",
            overridden.join("、")
        ));
    }
    if file_config.listen != startup.listen && !overridden.contains(&"listen") {
        notes.push("监听地址将在重启服务后生效".to_string());
    }
    let message = if notes.is_empty() {
        "设置已保存并立即生效".to_string()
    } else {
        format!("设置已保存；{}", notes.join("；"))
    };
    Ok(super::redirect_msg("/admin/settings", "ok", &message).into_response())
}

pub async fn check_update(State(state): State<AppState>, _admin: AdminUser) -> Response {
    let service = state.updates.clone();
    match tokio::task::spawn_blocking(move || service.check()).await {
        Ok(Ok(status)) => axum::Json(serde_json::json!({
            "ok": true,
            "current": status.current,
            "latest": status.latest,
            "has_update": status.has_update,
            "supported": update::web_update_supported(),
        }))
        .into_response(),
        Ok(Err(err)) => json_error(StatusCode::BAD_GATEWAY, err.0),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("检查更新任务失败: {err}"),
        ),
    }
}

struct BusyGuard(UpdateService);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

pub async fn apply_update(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<CsrfForm>,
) -> Response {
    if let Err(err) = auth::require_csrf(&admin.0, &form.csrf) {
        return json_error(StatusCode::BAD_REQUEST, err.to_string());
    }
    if !update::web_update_supported() {
        return json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "当前平台（{}）不支持在网页上直接安装，请下载发布包或使用命令行更新",
                std::env::consts::OS
            ),
        );
    }
    if !state.updates.begin() {
        return json_error(StatusCode::CONFLICT, "已有更新正在进行，请稍后再试");
    }
    let _guard = BusyGuard(state.updates.clone());

    let status = match tokio::task::spawn_blocking({
        let service = state.updates.clone();
        move || service.check()
    })
    .await
    {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => return json_error(StatusCode::BAD_GATEWAY, err.0),
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("检查更新任务失败: {err}"),
            );
        }
    };
    if !status.has_update {
        return axum::Json(serde_json::json!({
            "ok": false,
            "message": format!("已经是最新版本（{}）", status.current),
        }))
        .into_response();
    }

    let stage_dir = state.data_dir.join("updates");
    if stage_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&stage_dir).await;
    }
    let staged = match tokio::task::spawn_blocking({
        let service = state.updates.clone();
        let stage_dir = stage_dir.clone();
        move || service.stage_latest(&stage_dir)
    })
    .await
    {
        Ok(Ok(staged)) => staged,
        Ok(Err(err)) => return json_error(StatusCode::BAD_GATEWAY, err.0),
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("下载新版本任务失败: {err}"),
            );
        }
    };

    let backup = match update::backup_database(&state.data_dir).await {
        Ok(path) => path,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.0),
    };
    if let Some(path) = &backup {
        tracing::info!("在线更新前已备份数据库到 {}", path.display());
    }

    let previous = match tokio::task::spawn_blocking({
        let staged_path = staged.path.clone();
        move || update::replace_exe(&staged_path)
    })
    .await
    {
        Ok(Ok(previous)) => previous,
        Ok(Err(err)) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.0);
        }
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("替换程序任务失败: {err}"),
            );
        }
    };
    tracing::info!(
        "已安装 {}（旧版本备份于 {}），服务即将原地重启",
        staged.version,
        previous.display()
    );
    let _ = tokio::fs::remove_dir_all(&stage_dir).await;
    update::schedule_restart(Duration::from_millis(1500));

    axum::Json(serde_json::json!({
        "ok": true,
        "version": staged.version,
        "current": status.current,
        "message": "更新已安装，服务将在几秒后重启",
        "restarting": true,
    }))
    .into_response()
}
