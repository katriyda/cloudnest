use crate::auth::{self, CurrentUser};
use crate::error::AppError;
use crate::state::AppState;
use crate::util;
use crate::views::{
    PublicEntry, ShareCrumb, ShareDialog, ShareDialogTemplate, ShareFileOption, ShareItem,
    SharePage, SharePasswordPage, SharePasswordTemplate, ShareTemplate, SharesPage, SharesTemplate,
};
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use sqlx::FromRow;

const UNLOCK_LIFETIME: i64 = 12 * 60 * 60;

#[derive(FromRow, Clone)]
struct ShareRecord {
    id: i64,
    token: String,
    owner_id: i64,
    file_id: i64,
    password_hash: Option<String>,
    expires_at: Option<i64>,
    max_downloads: Option<i64>,
    download_count: i64,
    created_at: i64,
    file_name: String,
    is_dir: i64,
    size: i64,
    mime: Option<String>,
    file_deleted_at: Option<i64>,
    owner_name: String,
}

fn share_is_valid(record: &ShareRecord) -> bool {
    if record.file_deleted_at.is_some() {
        return false;
    }
    if record
        .expires_at
        .is_some_and(|expires| expires <= util::now_ts())
    {
        return false;
    }
    if record
        .max_downloads
        .is_some_and(|max| record.download_count >= max)
    {
        return false;
    }
    true
}

fn downloads_text(record: &ShareRecord) -> String {
    match record.max_downloads {
        Some(max) => format!("{} / {}", record.download_count, max),
        None => format!("{} 次", record.download_count),
    }
}

fn expires_text(record: &ShareRecord) -> String {
    match record.expires_at {
        Some(ts) => util::format_ts(ts),
        None => "永久".to_string(),
    }
}

async fn load_share(state: &AppState, token: &str) -> Result<ShareRecord, AppError> {
    let record = sqlx::query_as::<_, ShareRecord>(
        "SELECT s.id, s.token, s.owner_id, s.file_id, s.password_hash, s.expires_at,
                s.max_downloads, s.download_count, s.created_at, f.name AS file_name, f.is_dir, f.size,
                f.mime, f.deleted_at AS file_deleted_at, u.username AS owner_name
         FROM shares s
         JOIN files f ON f.id = s.file_id
         JOIN users u ON u.id = s.owner_id
         WHERE s.token = ?",
    )
    .bind(token)
    .fetch_optional(&state.pool)
    .await?;
    record.ok_or_else(|| AppError::NotFound("分享链接不存在或已被取消".to_string()))
}

fn share_url(state: &AppState, headers: &HeaderMap, token: &str) -> String {
    let config = state.config();
    if let Some(public_url) = &config.public_url {
        return format!("{public_url}/s/{token}");
    }
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}/s/{token}")
}

#[derive(Deserialize, Default)]
pub struct SharesQuery {
    #[serde(default)]
    pub ok: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<SharesQuery>,
) -> Result<Response, AppError> {
    let common = crate::views::build_common(
        &state,
        &user.0,
        "shares",
        query.ok.as_deref(),
        query.err.as_deref(),
    )
    .await?;
    let rows = sqlx::query_as::<_, ShareRecord>(
        "SELECT s.id, s.token, s.owner_id, s.file_id, s.password_hash, s.expires_at,
                s.max_downloads, s.download_count, s.created_at, f.name AS file_name, f.is_dir, f.size,
                f.mime, f.deleted_at AS file_deleted_at, u.username AS owner_name
         FROM shares s
         JOIN files f ON f.id = s.file_id
         JOIN users u ON u.id = s.owner_id
         WHERE s.owner_id = ?
         ORDER BY s.created_at DESC LIMIT 500",
    )
    .bind(user.0.id)
    .fetch_all(&state.pool)
    .await?;
    let shares = rows
        .iter()
        .map(|record| ShareItem {
            id: record.id,
            token: record.token.clone(),
            name: record.file_name.clone(),
            is_dir: record.is_dir != 0,
            url: share_url(&state, &headers, &record.token),
            has_password: record.password_hash.is_some(),
            expires_text: expires_text(record),
            downloads_text: downloads_text(record),
            created_text: util::format_date(record.created_at),
            active: share_is_valid(record),
            download_count: record.download_count,
        })
        .collect();
    let page = SharesPage {
        common,
        shares,
        new_share_url: "/shares/new".to_string(),
    };
    super::render(&SharesTemplate { page }, StatusCode::OK).await
}

#[derive(Deserialize, Default)]
pub struct NewShareQuery {
    #[serde(default)]
    pub file_id: Option<i64>,
}

pub async fn new_dialog(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<NewShareQuery>,
) -> Result<Response, AppError> {
    let mut files = Vec::new();
    let mut fixed_file = false;
    let mut selected_id = 0;
    let mut selected_name = String::new();
    let mut is_dir = false;
    if let Some(file_id) = query.file_id {
        let file = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT id, name, is_dir FROM files
             WHERE id = ? AND owner_id = ? AND deleted_at IS NULL",
        )
        .bind(file_id)
        .bind(user.0.id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("文件不存在".to_string()))?;
        fixed_file = true;
        selected_id = file.0;
        selected_name = file.1;
        is_dir = file.2 != 0;
    } else {
        let rows: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT id, name, is_dir FROM files
             WHERE owner_id = ? AND parent_id IS NULL AND deleted_at IS NULL
             ORDER BY is_dir DESC, name COLLATE NOCASE ASC LIMIT 200",
        )
        .bind(user.0.id)
        .fetch_all(&state.pool)
        .await?;
        if rows.is_empty() {
            return Err(AppError::BadRequest(
                "还没有可分享的文件或文件夹".to_string(),
            ));
        }
        for (id, name, dir) in rows {
            files.push(ShareFileOption {
                id,
                name,
                is_dir: dir != 0,
            });
        }
    }
    super::render(
        &ShareDialogTemplate {
            page: ShareDialog {
                action: "/shares/create".to_string(),
                csrf: user.0.csrf.clone(),
                files,
                fixed_file,
                selected_id,
                selected_name,
                is_dir,
                min_expiry: util::format_datetime_local(util::now_ts() + 300),
            },
        },
        StatusCode::OK,
    )
    .await
}

#[derive(Deserialize)]
pub struct CreateShareForm {
    pub csrf: String,
    pub file_id: i64,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub max_downloads: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<CreateShareForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let _file = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT id, name, is_dir FROM files
         WHERE id = ? AND owner_id = ? AND deleted_at IS NULL",
    )
    .bind(form.file_id)
    .bind(user.0.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("文件不存在".to_string()))?;
    let password_hash = match form
        .password
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(password) => {
            if password.chars().count() > 128 {
                return Err(AppError::BadRequest("提取密码过长".to_string()));
            }
            Some(auth::hash_password(password.to_string()).await?)
        }
        None => None,
    };
    let expires_at = match form.expires_at.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            let ts = util::parse_local_datetime(value)
                .ok_or_else(|| AppError::BadRequest("有效期格式不正确".to_string()))?;
            if ts <= util::now_ts() {
                return Err(AppError::BadRequest("有效期必须晚于当前时间".to_string()));
            }
            Some(ts)
        }
        _ => None,
    };
    let max_downloads = match form
        .max_downloads
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(value) => {
            let parsed: i64 = value
                .parse()
                .map_err(|_| AppError::BadRequest("下载次数上限必须是正整数".to_string()))?;
            if parsed <= 0 {
                return Err(AppError::BadRequest("下载次数上限必须大于 0".to_string()));
            }
            Some(parsed)
        }
        None => None,
    };
    let token = auth::random_token(32);
    let now = util::now_ts();
    sqlx::query(
        "INSERT INTO shares (token, owner_id, file_id, password_hash, expires_at, max_downloads, download_count, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(&token)
    .bind(user.0.id)
    .bind(form.file_id)
    .bind(&password_hash)
    .bind(expires_at)
    .bind(max_downloads)
    .bind(now)
    .execute(&state.pool)
    .await?;
    Ok(super::redirect_msg("/shares", "ok", "分享链接已创建").into_response())
}

#[derive(Deserialize)]
pub struct DeleteShareForm {
    pub csrf: String,
    pub id: i64,
}

pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<DeleteShareForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    sqlx::query("DELETE FROM shares WHERE id = ? AND owner_id = ?")
        .bind(form.id)
        .bind(user.0.id)
        .execute(&state.pool)
        .await?;
    Ok(super::redirect_msg("/shares", "ok", "分享已取消").into_response())
}

fn share_cookie_name(token: &str) -> String {
    format!("cn_s_{token}")
}

async fn is_unlocked(state: &AppState, record: &ShareRecord, headers: &HeaderMap) -> bool {
    let Some(password_hash) = record.password_hash.as_deref() else {
        return true;
    };
    let Some(cookie) = auth::get_cookie(headers, &share_cookie_name(&record.token)) else {
        return false;
    };
    let expected = auth::share_unlock_value(&state.pool, &record.token, password_hash).await;
    auth::csrf_ok(&expected, &cookie)
}

fn require_access(_state: &AppState, record: &ShareRecord) -> Result<(), AppError> {
    if !share_is_valid(record) {
        return Err(AppError::NotFound("分享链接已失效".to_string()));
    }
    if record.file_deleted_at.is_some() {
        return Err(AppError::NotFound("分享的文件已被删除".to_string()));
    }
    Ok(())
}

async fn ensure_unlocked(
    state: &AppState,
    record: &ShareRecord,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    require_access(state, record)?;
    if !is_unlocked(state, record, headers).await {
        return Err(AppError::Forbidden("该分享需要提取密码".to_string()));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct PublicShareQuery {
    #[serde(default)]
    pub dir: Option<i64>,
    #[serde(default)]
    pub err: Option<String>,
}

fn share_password_page(record: &ShareRecord, error: Option<String>) -> SharePasswordPage {
    SharePasswordPage {
        token: record.token.clone(),
        owner_name: record.owner_name.clone(),
        root_name: record.file_name.clone(),
        error,
    }
}

pub async fn public_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<PublicShareQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    require_access(&state, &record)?;
    if !is_unlocked(&state, &record, &headers).await {
        let page = share_password_page(&record, query.err.clone());
        return super::render(&SharePasswordTemplate { page }, StatusCode::OK).await;
    }

    let mut entries = Vec::new();
    let mut crumbs = Vec::new();
    let mut is_empty = false;
    let is_file = record.is_dir == 0;
    let mut file_name = String::new();
    let mut raw_url = String::new();
    let mut download_url = String::new();
    let back_url = String::new();
    let mut text_content = String::new();
    let mut text_truncated = false;

    if record.is_dir != 0 {
        let dir_id = query.dir.unwrap_or(record.file_id).max(0);
        let current = if dir_id == 0 { record.file_id } else { dir_id };
        validate_descendant(&state, record.owner_id, current, record.file_id).await?;
        let mut chain = Vec::new();
        let mut cur = current;
        for _ in 0..64 {
            if cur == record.file_id {
                break;
            }
            let (id, parent, name) = share_folder_row(&state, record.owner_id, cur).await?;
            chain.push((id, name));
            cur = parent;
        }
        chain.reverse();
        crumbs.push(ShareCrumb {
            id: record.file_id,
            name: record.file_name.clone(),
            href: format!("/s/{}", record.token),
        });
        for (id, name) in &chain {
            crumbs.push(ShareCrumb {
                id: *id,
                name: name.clone(),
                href: format!("/s/{}?dir={id}", record.token),
            });
        }
        let rows = sqlx::query_as::<_, (i64, String, i64, i64, Option<String>, i64)>(
            "SELECT id, name, is_dir, size, mime, updated_at FROM files
             WHERE owner_id = ? AND parent_id = ? AND deleted_at IS NULL
             ORDER BY is_dir DESC, name COLLATE NOCASE ASC LIMIT 1000",
        )
        .bind(record.owner_id)
        .bind(current)
        .fetch_all(&state.pool)
        .await?;
        is_empty = rows.is_empty();
        for (id, name, dir, size, mime, updated) in rows {
            let preview = if dir != 0 {
                "none".to_string()
            } else {
                util::preview_kind(&name, mime.as_deref()).to_string()
            };
            entries.push(PublicEntry {
                id,
                name: name.clone(),
                is_dir: dir != 0,
                size_text: if dir != 0 {
                    "文件夹".to_string()
                } else {
                    util::human_size(size)
                },
                updated_text: util::format_ts(updated),
                ext_label: if dir != 0 {
                    String::new()
                } else {
                    util::extension_label(&name)
                },
                hue: util::hue_for(&name),
                open_href: if dir != 0 {
                    format!("/s/{}?dir={id}", record.token)
                } else {
                    format!("/s/{}/file/{id}", record.token)
                },
                download_href: if dir != 0 {
                    String::new()
                } else {
                    format!("/s/{}/download/{id}", record.token)
                },
                preview,
            });
        }
    } else {
        file_name = record.file_name.clone();
        raw_url = format!("/s/{}/raw", record.token);
        download_url = format!("/s/{}/download", record.token);
        let owned = load_public_file(&state, &record, record.file_id).await?;
        if util::preview_kind(&owned.name, owned.mime.as_deref()) == "text" {
            (text_content, text_truncated) =
                crate::routes::files::load_text_preview(&state, record.owner_id, &owned).await?;
        }
    }

    let page = SharePage {
        token: record.token.clone(),
        owner_name: record.owner_name.clone(),
        root_name: record.file_name.clone(),
        is_file,
        file_name: if is_file { file_name } else { String::new() },
        file_size_text: util::human_size(record.size),
        file_preview: if is_file {
            util::preview_kind(&record.file_name, record.mime.as_deref()).to_string()
        } else {
            "none".to_string()
        },
        file_ext: util::extension_label(&record.file_name),
        file_hue: util::hue_for(&record.file_name),
        raw_url,
        download_url,
        crumbs,
        entries,
        downloads_left: match record.max_downloads {
            Some(max) => format!("{} / {}", record.download_count, max),
            None => "不限".to_string(),
        },
        expires_text: expires_text(&record),
        is_empty,
        back_url,
        text_content,
        text_truncated,
    };
    super::render(&ShareTemplate { page }, StatusCode::OK).await
}

async fn share_folder_row(
    state: &AppState,
    owner_id: i64,
    id: i64,
) -> Result<(i64, i64, String), AppError> {
    let row: Option<(Option<i64>, String)> = sqlx::query_as(
        "SELECT parent_id, name FROM files WHERE id = ? AND owner_id = ? AND is_dir = 1 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&state.pool)
    .await?;
    let (parent, name) = row.ok_or_else(|| AppError::NotFound("分享文件夹不存在".to_string()))?;
    Ok((id, parent.unwrap_or(0), name))
}

async fn validate_descendant(
    state: &AppState,
    owner_id: i64,
    file_id: i64,
    root_id: i64,
) -> Result<(), AppError> {
    let mut cur = file_id;
    for _ in 0..64 {
        if cur == root_id {
            return Ok(());
        }
        let row: Option<(Option<i64>, Option<i64>)> =
            sqlx::query_as("SELECT parent_id, deleted_at FROM files WHERE id = ? AND owner_id = ?")
                .bind(cur)
                .bind(owner_id)
                .fetch_optional(&state.pool)
                .await?;
        let (parent, deleted) = row.ok_or_else(|| AppError::NotFound("文件不存在".to_string()))?;
        if deleted.is_some() {
            return Err(AppError::NotFound("文件已被删除".to_string()));
        }
        match parent {
            Some(parent) if parent != 0 => cur = parent,
            _ => return Err(AppError::Forbidden("文件不在分享范围内".to_string())),
        }
    }
    Err(AppError::Forbidden("文件不在分享范围内".to_string()))
}

pub async fn public_file_page(
    State(state): State<AppState>,
    Path((token, file_id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    ensure_unlocked(&state, &record, &headers).await?;
    let file = load_public_file(&state, &record, file_id).await?;
    let parent = file.parent_id;
    let back_url = if parent != 0 && parent != record.file_id {
        format!("/s/{}?dir={parent}", record.token)
    } else if record.is_dir != 0 {
        format!("/s/{}", record.token)
    } else {
        String::new()
    };
    let (text_content, text_truncated) =
        if util::preview_kind(&file.name, file.mime.as_deref()) == "text" {
            crate::routes::files::load_text_preview(&state, record.owner_id, &file).await?
        } else {
            (String::new(), false)
        };
    let page = SharePage {
        token: record.token.clone(),
        owner_name: record.owner_name.clone(),
        root_name: record.file_name.clone(),
        is_file: true,
        file_name: file.name.clone(),
        file_size_text: util::human_size(file.size),
        file_preview: util::preview_kind(&file.name, file.mime.as_deref()).to_string(),
        file_ext: util::extension_label(&file.name),
        file_hue: util::hue_for(&file.name),
        raw_url: format!("/s/{}/raw/{file_id}", record.token),
        download_url: format!("/s/{}/download/{file_id}", record.token),
        crumbs: Vec::new(),
        entries: Vec::new(),
        downloads_left: match record.max_downloads {
            Some(max) => format!("{} / {}", record.download_count, max),
            None => "不限".to_string(),
        },
        expires_text: expires_text(&record),
        is_empty: false,
        back_url,
        text_content,
        text_truncated,
    };
    super::render(&ShareTemplate { page }, StatusCode::OK).await
}

#[derive(Deserialize)]
pub struct UnlockForm {
    pub password: String,
}

pub async fn unlock(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Form(form): Form<UnlockForm>,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    require_access(&state, &record)?;
    let Some(password_hash) = record.password_hash.as_deref() else {
        return Ok(Redirect::to(&format!("/s/{token}")).into_response());
    };
    if !auth::verify_password(form.password, password_hash.to_string()).await? {
        let page = share_password_page(&record, Some("提取密码错误".to_string()));
        return super::render(&SharePasswordTemplate { page }, StatusCode::UNAUTHORIZED).await;
    }
    let value = auth::share_unlock_value(&state.pool, &token, password_hash).await;
    let cookie = auth::cookie_header(
        &share_cookie_name(&token),
        &value,
        UNLOCK_LIFETIME,
        true,
        state.config().secure_cookies,
        &format!("/s/{token}"),
    );
    let cookie = HeaderValue::from_str(&cookie)
        .map_err(|e| AppError::Internal(format!("Cookie 无效: {e}")))?;
    let mut response = Redirect::to(&format!("/s/{token}")).into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(response)
}

async fn load_public_file(
    state: &AppState,
    record: &ShareRecord,
    file_id: i64,
) -> Result<crate::routes::files::OwnedFile, AppError> {
    if record.is_dir != 0 {
        validate_descendant(state, record.owner_id, file_id, record.file_id).await?;
    } else if file_id != record.file_id {
        return Err(AppError::Forbidden("文件不在分享范围内".to_string()));
    }
    type PublicFileRow = (
        i64,
        Option<i64>,
        String,
        i64,
        i64,
        Option<String>,
        Option<String>,
    );
    let row: Option<PublicFileRow> = sqlx::query_as(
        "SELECT id, parent_id, name, is_dir, size, mime, storage_key FROM files
         WHERE id = ? AND owner_id = ? AND deleted_at IS NULL AND is_dir = 0",
    )
    .bind(file_id)
    .bind(record.owner_id)
    .fetch_optional(&state.pool)
    .await?;
    let (id, parent, name, is_dir, size, mime, storage_key) =
        row.ok_or_else(|| AppError::NotFound("文件不存在".to_string()))?;
    Ok(crate::routes::files::OwnedFile {
        id,
        parent_id: parent.unwrap_or(0),
        name,
        is_dir: is_dir != 0,
        size,
        mime,
        updated_at: 0,
        storage_key,
    })
}

async fn increment_download(state: &AppState, record: &ShareRecord) -> Result<(), AppError> {
    let updated = sqlx::query(
        "UPDATE shares SET download_count = download_count + 1
         WHERE id = ? AND (max_downloads IS NULL OR download_count < max_downloads)",
    )
    .bind(record.id)
    .execute(&state.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::BadRequest("该分享的下载次数已达上限".to_string()));
    }
    Ok(())
}

async fn public_download_file(
    state: &AppState,
    record: &ShareRecord,
    file_id: i64,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    ensure_unlocked(state, record, headers).await?;
    let file = load_public_file(state, record, file_id).await?;
    increment_download(state, record).await?;
    crate::routes::files::serve_owned_file(state, record.owner_id, &file, false, headers).await
}

async fn public_raw_file(
    state: &AppState,
    record: &ShareRecord,
    file_id: i64,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    ensure_unlocked(state, record, headers).await?;
    let file = load_public_file(state, record, file_id).await?;
    crate::routes::files::serve_owned_file(state, record.owner_id, &file, true, headers).await
}

pub async fn public_download_root(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    public_download_file(&state, &record, record.file_id, &headers).await
}

pub async fn public_download(
    State(state): State<AppState>,
    Path((token, file_id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    public_download_file(&state, &record, file_id, &headers).await
}

pub async fn public_raw_root(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    public_raw_file(&state, &record, record.file_id, &headers).await
}

pub async fn public_raw(
    State(state): State<AppState>,
    Path((token, file_id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let record = load_share(&state, &token).await?;
    public_raw_file(&state, &record, file_id, &headers).await
}
