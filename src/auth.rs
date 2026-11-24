use crate::error::AppError;
use crate::state::AppState;
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

pub const SESSION_COOKIE: &str = "cn_session";

#[derive(Debug, Clone)]
pub struct SessionUser {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub is_admin: bool,
    pub quota_bytes: i64,
    pub csrf: String,
    pub session_token: String,
}

pub struct CurrentUser(pub SessionUser);

#[derive(Debug)]
pub struct AdminUser(pub SessionUser);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = get_cookie(&parts.headers, SESSION_COOKIE).ok_or(AppError::Unauthorized)?;
        load_session(&state.pool, &token).await
    }
}

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?.0;
        if !user.is_admin {
            return Err(AppError::Forbidden("需要管理员权限".to_string()));
        }
        Ok(AdminUser(user))
    }
}

async fn load_session(pool: &SqlitePool, token: &str) -> Result<CurrentUser, AppError> {
    let now = crate::util::now_ts();
    let row: Option<(i64, String, String, i64, i64, String, i64)> = sqlx::query_as(
        "SELECT u.id, u.username, u.role, u.disabled, u.quota_bytes, s.csrf, s.expires_at
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = ?",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    let (id, username, role, disabled, quota_bytes, csrf, expires_at) =
        row.ok_or(AppError::Unauthorized)?;
    if expires_at <= now || disabled != 0 {
        let _ = sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(pool)
            .await;
        return Err(AppError::Unauthorized);
    }
    let _ = sqlx::query("UPDATE sessions SET last_seen = ? WHERE token = ?")
        .bind(now)
        .bind(token)
        .execute(pool)
        .await;
    Ok(CurrentUser(SessionUser {
        id,
        username,
        role: role.clone(),
        is_admin: role == "admin",
        quota_bytes,
        csrf,
        session_token: token.to_string(),
    }))
}

pub async fn create_session(
    pool: &SqlitePool,
    user_id: i64,
    secure: bool,
    lifetime_seconds: i64,
) -> Result<(String, String, String), AppError> {
    let now = crate::util::now_ts();
    let token = random_token(32);
    let csrf = random_token(24);
    let expires = now + lifetime_seconds.clamp(3600, 365 * 24 * 60 * 60);
    let _ = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(now)
        .execute(pool)
        .await;
    sqlx::query(
        "INSERT INTO sessions (token, user_id, csrf, created_at, expires_at, last_seen)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(user_id)
    .bind(&csrf)
    .bind(now)
    .bind(expires)
    .bind(now)
    .execute(pool)
    .await?;
    let cookie = cookie_header(SESSION_COOKIE, &token, expires - now, true, secure, "/");
    Ok((token, csrf, cookie))
}

pub async fn destroy_session(pool: &SqlitePool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub fn clear_session_cookie(secure: bool) -> String {
    cookie_header(SESSION_COOKIE, "", 0, true, secure, "/")
}

pub fn cookie_header(
    name: &str,
    value: &str,
    max_age: i64,
    http_only: bool,
    secure: bool,
    path: &str,
) -> String {
    let mut cookie = format!("{name}={value}; Path={path}; SameSite=Lax");
    if max_age > 0 {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    } else {
        cookie.push_str("; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
    }
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=') {
            if key == name {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    hex::encode(buf)
}

pub async fn hash_password(password: String) -> Result<String, AppError> {
    tokio::task::spawn_blocking(move || {
        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(password.as_bytes())
            .map_err(|e| AppError::Internal(format!("密码哈希失败: {e}")))?;
        Ok::<String, AppError>(hash.to_string())
    })
    .await
    .map_err(|e| AppError::Internal(format!("密码哈希任务失败: {e}")))?
}

pub async fn verify_password(password: String, hash: String) -> Result<bool, AppError> {
    tokio::task::spawn_blocking(move || {
        let parsed = match PasswordHash::new(&hash) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("存储的密码哈希无效: {e}");
                return false;
            }
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .map_err(|e| AppError::Internal(format!("密码校验任务失败: {e}")))
}

pub fn csrf_ok(expected: &str, supplied: &str) -> bool {
    if expected.len() != supplied.len() || expected.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(supplied.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub fn require_csrf(user: &SessionUser, token: &str) -> Result<(), AppError> {
    if !csrf_ok(&user.csrf, token.trim()) {
        return Err(AppError::BadRequest(
            "安全校验失败，请刷新页面后重试".to_string(),
        ));
    }
    Ok(())
}

/// Signed value proving the visitor entered the share password.
pub async fn share_unlock_value(pool: &SqlitePool, token: &str, password_hash: &str) -> String {
    let secret = crate::db::server_secret(pool).await.unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":share:");
    hasher.update(token.as_bytes());
    hasher.update(b":");
    hasher.update(password_hash.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique() {
        let a = random_token(16);
        let b = random_token(16);
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn csrf_comparison() {
        assert!(csrf_ok("abc", "abc"));
        assert!(!csrf_ok("abc", "abd"));
        assert!(!csrf_ok("", ""));
    }
}
