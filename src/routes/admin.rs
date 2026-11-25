use crate::auth::{self, AdminUser};
use crate::error::AppError;
use crate::state::AppState;
use crate::storage;
use crate::util;
use crate::views::{AdminPage, AdminTemplate, AdminUserRow};
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sqlx::Row;

#[derive(Deserialize, Default)]
pub struct AdminQuery {
    #[serde(default)]
    pub ok: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateUserForm {
    pub csrf: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub quota_gb: Option<i64>,
}

#[derive(Deserialize)]
pub struct UserIdForm {
    pub csrf: String,
    pub id: i64,
}

#[derive(Deserialize)]
pub struct ResetPasswordForm {
    pub csrf: String,
    pub id: i64,
    pub password: String,
}

#[derive(Deserialize)]
pub struct QuotaForm {
    pub csrf: String,
    pub id: i64,
    pub quota_gb: i64,
}

pub async fn index(
    State(state): State<AppState>,
    admin: AdminUser,
    Query(query): Query<AdminQuery>,
) -> Result<Response, AppError> {
    let common = crate::views::build_common(
        &state,
        &admin.0,
        "admin",
        query.ok.as_deref(),
        query.err.as_deref(),
    )
    .await?;
    let rows = sqlx::query(
        "SELECT u.id, u.username, u.role, u.disabled, u.quota_bytes, u.created_at,
                (SELECT COALESCE(SUM(size), 0) FROM files f WHERE f.owner_id = u.id AND f.is_dir = 0) AS used_bytes,
                (SELECT COUNT(*) FROM files f WHERE f.owner_id = u.id AND f.is_dir = 0) AS file_count
         FROM users u ORDER BY u.role = 'admin' DESC, u.id ASC",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut users = Vec::new();
    let mut total_files = 0i64;
    let mut total_size = 0i64;
    let mut total_quota = 0i64;
    for row in &rows {
        let quota_bytes: i64 = row.get("quota_bytes");
        let used: i64 = row.get("used_bytes");
        let count: i64 = row.get("file_count");
        total_files += count;
        total_size += used;
        if quota_bytes > 0 {
            total_quota += quota_bytes;
        }
        let quota_unlimited = quota_bytes <= 0;
        let percent = if quota_unlimited {
            0
        } else {
            ((used as f64 / quota_bytes as f64) * 100.0)
                .ceil()
                .clamp(0.0, 100.0) as u8
        };
        users.push(AdminUserRow {
            id: row.get("id"),
            username: row.get("username"),
            initial: row
                .get::<String, _>("username")
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string(),
            is_admin: row.get::<String, _>("role") == "admin",
            disabled: row.get::<i64, _>("disabled") != 0,
            used_text: util::human_size(used),
            quota_text: if quota_unlimited {
                "不限".to_string()
            } else {
                util::human_size(quota_bytes)
            },
            percent,
            quota_unlimited,
            quota_gb: if quota_unlimited {
                0
            } else {
                (quota_bytes / util::GIB).max(1)
            },
            file_count: count,
            created_text: util::format_date(row.get("created_at")),
            is_self: row.get::<i64, _>("id") == admin.0.id,
        });
    }
    let total_users = users.len() as i64;
    let page = AdminPage {
        common,
        users,
        total_users,
        total_files,
        total_size_text: util::human_size(total_size),
        total_quota_text: util::human_size(total_quota),
        default_quota_gb: state.config().default_quota_gb,
    };
    super::render(&AdminTemplate { page }, StatusCode::OK).await
}

pub async fn create_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<CreateUserForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&admin.0, &form.csrf)?;
    let username = form.username.trim().to_string();
    util::username_valid(&username)?;
    util::password_valid(&form.password)?;
    let quota_gb = form
        .quota_gb
        .unwrap_or(state.config().default_quota_gb)
        .max(0);
    let quota_bytes = quota_gb.saturating_mul(util::GIB);
    let hash = auth::hash_password(form.password).await?;
    let now = util::now_ts();
    let result = sqlx::query(
        "INSERT INTO users (username, password_hash, role, disabled, quota_bytes, created_at)
         VALUES (?, ?, 'user', 0, ?, ?)",
    )
    .bind(&username)
    .bind(&hash)
    .bind(quota_bytes)
    .bind(now)
    .execute(&state.pool)
    .await;
    if let Err(err) = result {
        tracing::warn!("创建用户失败: {err}");
        return Ok(
            super::redirect_msg("/admin", "err", "创建失败：用户名可能已存在").into_response(),
        );
    }
    Ok(super::redirect_msg("/admin", "ok", &format!("用户 {username} 已创建")).into_response())
}

async fn target_user(state: &AppState, id: i64) -> Result<(String, String, i64), AppError> {
    let row: Option<(String, String, i64)> =
        sqlx::query_as("SELECT username, role, disabled FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    row.ok_or_else(|| AppError::NotFound("用户不存在".to_string()))
}

pub async fn toggle_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<UserIdForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&admin.0, &form.csrf)?;
    if form.id == admin.0.id {
        return Err(AppError::BadRequest(
            "不能禁用当前登录的管理员账户".to_string(),
        ));
    }
    let (username, _role, disabled) = target_user(&state, form.id).await?;
    let new_disabled = if disabled != 0 { 0 } else { 1 };
    sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
        .bind(new_disabled)
        .bind(form.id)
        .execute(&state.pool)
        .await?;
    if new_disabled == 1 {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(form.id)
            .execute(&state.pool)
            .await?;
    }
    let message = if new_disabled == 1 {
        format!("用户 {username} 已禁用")
    } else {
        format!("用户 {username} 已启用")
    };
    Ok(super::redirect_msg("/admin", "ok", &message).into_response())
}

pub async fn reset_password(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<ResetPasswordForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&admin.0, &form.csrf)?;
    util::password_valid(&form.password)?;
    let (username, _role, _disabled) = target_user(&state, form.id).await?;
    let hash = auth::hash_password(form.password).await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(&hash)
        .bind(form.id)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(form.id)
        .execute(&state.pool)
        .await?;
    Ok(super::redirect_msg("/admin", "ok", &format!("已重置 {username} 的密码")).into_response())
}

pub async fn set_quota(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<QuotaForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&admin.0, &form.csrf)?;
    if !(0..=1_048_576).contains(&form.quota_gb) {
        return Err(AppError::BadRequest("存储空间设置无效".to_string()));
    }
    let (username, _role, _disabled) = target_user(&state, form.id).await?;
    let quota_bytes = form.quota_gb.saturating_mul(util::GIB);
    sqlx::query("UPDATE users SET quota_bytes = ? WHERE id = ?")
        .bind(quota_bytes)
        .bind(form.id)
        .execute(&state.pool)
        .await?;
    let message = if quota_bytes == 0 {
        format!("{username} 的存储空间已设为不限制")
    } else {
        format!("{username} 的存储空间已设为 {} GB", form.quota_gb)
    };
    Ok(super::redirect_msg("/admin", "ok", &message).into_response())
}

pub async fn delete_user(
    State(state): State<AppState>,
    admin: AdminUser,
    Form(form): Form<UserIdForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&admin.0, &form.csrf)?;
    if form.id == admin.0.id {
        return Err(AppError::BadRequest("不能删除当前登录的账户".to_string()));
    }
    let (username, role, _disabled) = target_user(&state, form.id).await?;
    if role == "admin" {
        let admin_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
                .fetch_one(&state.pool)
                .await?;
        if admin_count <= 1 {
            return Err(AppError::BadRequest("不能删除最后一个管理员".to_string()));
        }
    }
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT storage_key FROM files WHERE owner_id = ? AND storage_key IS NOT NULL",
    )
    .bind(form.id)
    .fetch_all(&state.pool)
    .await?;
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(form.id)
        .execute(&state.pool)
        .await?;
    storage::delete_stored_keys(&state, form.id, &keys).await;
    Ok(super::redirect_msg("/admin", "ok", &format!("用户 {username} 已删除")).into_response())
}
