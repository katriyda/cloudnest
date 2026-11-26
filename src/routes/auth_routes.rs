use crate::assets::Assets;
use crate::auth::{self, CurrentUser};
use crate::error::AppError;
use crate::ratelimit::FailureLimit;
use crate::state::AppState;
use crate::util;
use crate::views::{
    AccountPage, AccountTemplate, LoginPage, LoginTemplate, SetupPage, SetupTemplate,
};
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

/// 同一用户名连续失败 10 次后锁定 15 分钟；记录的是“失败”，成功登录会清零。
const LOGIN_FAILURE_LIMIT: FailureLimit = FailureLimit::new(10, 900, 900);

fn login_key(username: &str) -> String {
    format!("login-user:{}", username.to_lowercase())
}

fn note_login_failure(state: &AppState, username: &str) -> Option<i64> {
    let blocked = state
        .limits
        .record_failure(&login_key(username), LOGIN_FAILURE_LIMIT);
    if let Some(seconds) = blocked {
        tracing::warn!("账号 {username} 登录失败次数过多，已临时锁定 {seconds} 秒");
    }
    blocked
}

#[derive(Deserialize, Default)]
pub struct LoginQuery {
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub struct SetupForm {
    pub username: String,
    pub password: String,
    pub password_confirm: String,
}

#[derive(Deserialize)]
pub struct CsrfForm {
    pub csrf: String,
}

#[derive(Deserialize)]
pub struct PasswordForm {
    pub csrf: String,
    pub current_password: String,
    pub new_password: String,
    pub new_password_confirm: String,
}

async fn user_count(state: &AppState) -> Result<i64, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await?;
    Ok(count)
}

pub async fn login_page(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Result<Response, AppError> {
    if user_count(&state).await? == 0 {
        return Ok(Redirect::to("/setup").into_response());
    }
    let page = LoginPage {
        error: None,
        next: util::safe_next(query.next.as_deref()),
        username: String::new(),
        assets: Assets::new(),
    };
    super::render(&LoginTemplate { page }, StatusCode::OK).await
}

pub async fn login_submit(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    if user_count(&state).await? == 0 {
        return Ok(Redirect::to("/setup").into_response());
    }
    let next = util::safe_next(form.next.as_deref());
    let username = form.username.trim().to_string();
    let generic_error = "用户名或密码错误".to_string();

    // 超出正常长度的输入直接拒绝，同时保持同样的耗时与文案。
    if username.chars().count() > 64 || form.password.chars().count() > 128 {
        auth::simulate_password_check(form.password.clone()).await?;
        note_login_failure(&state, &username);
        let page = LoginPage {
            error: Some(generic_error),
            next,
            username,
            assets: Assets::new(),
        };
        return super::render(&LoginTemplate { page }, StatusCode::UNAUTHORIZED).await;
    }

    if let Some(seconds) = state.limits.blocked_seconds(&login_key(&username)) {
        tracing::warn!("登录请求被限流: 用户 {username}");
        return Err(AppError::rate_limited("尝试过于频繁，请稍后再试", seconds));
    }
    let Some(_slot) = state.password_slot().await else {
        return Err(AppError::rate_limited("登录请求过多，请稍后再试", 30));
    };

    let row: Option<(i64, String, i64)> =
        sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
            .bind(&username)
            .fetch_optional(&state.pool)
            .await?;

    let error = match row {
        // 不存在的账号也执行一次等价 Argon2 计算，避免通过响应时间枚举用户名。
        None => {
            auth::simulate_password_check(form.password.clone()).await?;
            note_login_failure(&state, &username);
            tracing::warn!("登录失败: 用户 {username}（账号不存在）");
            Some(generic_error)
        }
        Some((_id, hash, disabled)) if disabled != 0 => {
            let matched = auth::verify_password(form.password.clone(), hash).await?;
            if matched {
                Some("该账户已被禁用，请联系管理员".to_string())
            } else {
                note_login_failure(&state, &username);
                tracing::warn!("登录失败: 用户 {username}（密码错误）");
                Some(generic_error)
            }
        }
        Some((id, hash, _)) => {
            if auth::verify_password(form.password.clone(), hash).await? {
                state.limits.reset(&login_key(&username));
                let config = state.config();
                let (_, _, cookie) = auth::create_session(
                    &state.pool,
                    id,
                    config.secure_cookies,
                    config.session_seconds(),
                )
                .await?;
                tracing::info!("用户 {username} 登录成功");
                return super::redirect_with_cookie(&next, &cookie);
            }
            note_login_failure(&state, &username);
            tracing::warn!("登录失败: 用户 {username}（密码错误）");
            Some(generic_error)
        }
    };

    let page = LoginPage {
        error,
        next,
        username,
        assets: Assets::new(),
    };
    super::render(&LoginTemplate { page }, StatusCode::UNAUTHORIZED).await
}

pub async fn setup_page(State(state): State<AppState>) -> Result<Response, AppError> {
    if user_count(&state).await? > 0 {
        return Ok(Redirect::to("/login").into_response());
    }
    super::render(
        &SetupTemplate {
            page: SetupPage {
                error: None,
                assets: Assets::new(),
            },
        },
        StatusCode::OK,
    )
    .await
}

pub async fn setup_submit(
    State(state): State<AppState>,
    Form(form): Form<SetupForm>,
) -> Result<Response, AppError> {
    if user_count(&state).await? > 0 {
        return Ok(Redirect::to("/login").into_response());
    }
    let username = form.username.trim().to_string();
    let error = if let Err(err) = util::username_valid(&username) {
        Some(err.to_string())
    } else if let Err(err) = util::password_valid(&form.password) {
        Some(err.to_string())
    } else if form.password != form.password_confirm {
        Some("两次输入的密码不一致".to_string())
    } else {
        None
    };
    if let Some(error) = error {
        return super::render(
            &SetupTemplate {
                page: SetupPage {
                    error: Some(error),
                    assets: Assets::new(),
                },
            },
            StatusCode::BAD_REQUEST,
        )
        .await;
    }

    let hash = auth::hash_password(form.password.clone()).await?;
    let now = util::now_ts();
    // 用单条语句完成“仅当还没有任何用户时创建管理员”，避免并发初始化产生多个管理员。
    let result = sqlx::query(
        "INSERT INTO users (username, password_hash, role, disabled, quota_bytes, created_at)
         SELECT ?, ?, 'admin', 0, 0, ?
         WHERE NOT EXISTS (SELECT 1 FROM users)",
    )
    .bind(&username)
    .bind(&hash)
    .bind(now)
    .execute(&state.pool)
    .await;
    let user_id = match result {
        Ok(row) if row.rows_affected() == 0 => {
            tracing::info!("初始化页面被重复提交，已有管理员，跳转到登录");
            return Ok(Redirect::to("/login").into_response());
        }
        Ok(row) => row.last_insert_rowid(),
        Err(err) => {
            tracing::error!("初始化管理员失败: {err}");
            return super::render(
                &SetupTemplate {
                    page: SetupPage {
                        error: Some("创建管理员失败，请重试".to_string()),
                        assets: Assets::new(),
                    },
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .await;
        }
    };
    let config = state.config();
    let (_, _, cookie) = auth::create_session(
        &state.pool,
        user_id,
        config.secure_cookies,
        config.session_seconds(),
    )
    .await?;
    tracing::info!("初始化管理员账户 {username}");
    super::redirect_with_cookie(
        &format!(
            "/files?ok={}",
            urlencoding::encode("初始化完成，欢迎使用 CloudNest")
        ),
        &cookie,
    )
}

pub async fn logout(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    auth::destroy_session(&state.pool, &user.0.session_token).await?;
    let mut response = Redirect::to("/login?ok=已安全退出").into_response();
    let cookie = auth::clear_session_cookie(state.config().secure_cookies);
    if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
        response
            .headers_mut()
            .insert(axum::http::header::SET_COOKIE, value);
    }
    Ok(response)
}

pub async fn account_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<crate::routes::admin::AdminQuery>,
) -> Result<Response, AppError> {
    let common = crate::views::build_common(
        &state,
        &user.0,
        "account",
        query.ok.as_deref(),
        query.err.as_deref(),
    )
    .await?;
    let created_at: i64 = sqlx::query_scalar("SELECT created_at FROM users WHERE id = ?")
        .bind(user.0.id)
        .fetch_one(&state.pool)
        .await?;
    let user_file_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE owner_id = ? AND is_dir = 0")
            .bind(user.0.id)
            .fetch_one(&state.pool)
            .await?;
    let page = AccountPage {
        common,
        created_text: util::format_date(created_at),
        user_file_count,
    };
    super::render(&AccountTemplate { page }, StatusCode::OK).await
}

pub async fn account_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    util::password_valid(&form.new_password)?;
    if form.new_password != form.new_password_confirm {
        return Err(AppError::BadRequest("两次输入的新密码不一致".to_string()));
    }
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(user.0.id)
        .fetch_one(&state.pool)
        .await?;
    if !auth::verify_password(form.current_password, hash).await? {
        return Err(AppError::BadRequest("当前密码不正确".to_string()));
    }
    let new_hash = auth::hash_password(form.new_password).await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(user.0.id)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token != ?")
        .bind(user.0.id)
        .bind(&user.0.session_token)
        .execute(&state.pool)
        .await?;
    Ok(super::redirect_msg("/account", "ok", "密码已更新，其他设备已退出登录").into_response())
}
