use crate::auth::{self, CurrentUser};
use crate::error::AppError;
use crate::state::AppState;
use crate::storage;
use crate::util;
use crate::views::{
    Crumb, FileItem, FilesPage, FilesTemplate, FolderDialog, FolderOption, FolderTemplate,
    MoveDialog, MoveTemplate, PreviewPage, PreviewTemplate, RenameDialog, RenameTemplate,
};
use axum::Form;
use axum::body::{Body, Bytes};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use sqlx::Row;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

#[derive(Debug, Clone)]
pub(crate) struct OwnedFile {
    pub(crate) id: i64,
    pub(crate) parent_id: i64,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) size: i64,
    pub(crate) mime: Option<String>,
    pub(crate) updated_at: i64,
    pub(crate) storage_key: Option<String>,
}

impl OwnedFile {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        let parent: Option<i64> = row.get("parent_id");
        Self {
            id: row.get("id"),
            parent_id: parent.unwrap_or(0),
            name: row.get("name"),
            is_dir: row.get::<i64, _>("is_dir") != 0,
            size: row.get("size"),
            mime: row.get("mime"),
            updated_at: row.get("updated_at"),
            storage_key: row.get("storage_key"),
        }
    }
}

#[derive(Deserialize, Default)]
pub struct FilesQuery {
    #[serde(default)]
    pub path: Option<i64>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub ok: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    pub csrf: String,
}

#[derive(Deserialize)]
pub struct RenameForm {
    pub csrf: String,
    pub id: i64,
    pub name: String,
}

#[derive(Deserialize)]
pub struct MoveForm {
    pub csrf: String,
    pub id: i64,
    pub target_id: i64,
}

#[derive(Deserialize)]
pub struct DeleteForm {
    pub csrf: String,
    pub id: i64,
    #[serde(default)]
    pub return_path: Option<i64>,
}

#[derive(Deserialize)]
pub struct FolderForm {
    pub csrf: String,
    pub parent_id: i64,
    pub name: String,
}

#[derive(Deserialize)]
pub struct FolderDialogQuery {
    #[serde(default)]
    pub parent_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct UploadInitForm {
    pub csrf: String,
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub path: String,
    pub name: String,
    pub size: i64,
}

fn clamp_query(q: &str) -> String {
    q.trim().chars().take(100).collect()
}

fn normalize_sort(sort: Option<&str>) -> &'static str {
    match sort {
        Some("size") => "size",
        Some("modified") => "modified",
        _ => "name",
    }
}

fn normalize_dir(dir: Option<&str>) -> &'static str {
    if dir == Some("desc") { "desc" } else { "asc" }
}

fn normalize_view(view: Option<&str>) -> &'static str {
    if view == Some("grid") { "grid" } else { "list" }
}

fn files_url(parent_id: i64, q: &str, sort: &str, dir: &str, view: &str) -> String {
    format!(
        "/files?path={parent_id}&q={}&sort={sort}&dir={dir}&view={view}",
        urlencoding::encode(q)
    )
}

async fn resolve_folder(
    state: &AppState,
    owner_id: i64,
    id: i64,
) -> Result<Option<OwnedFile>, AppError> {
    if id == 0 {
        return Ok(Some(OwnedFile {
            id: 0,
            parent_id: 0,
            name: "全部文件".to_string(),
            is_dir: true,
            size: 0,
            mime: None,
            updated_at: 0,
            storage_key: None,
        }));
    }
    let row = sqlx::query(
        "SELECT id, parent_id, name, is_dir, size, mime, created_at, updated_at, storage_key
         FROM files WHERE id = ? AND owner_id = ? AND is_dir = 1 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.as_ref().map(OwnedFile::from_row))
}

async fn load_owned_file(state: &AppState, owner_id: i64, id: i64) -> Result<OwnedFile, AppError> {
    let row = sqlx::query(
        "SELECT id, parent_id, name, is_dir, size, mime, created_at, updated_at, storage_key
         FROM files WHERE id = ? AND owner_id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&state.pool)
    .await?;
    row.as_ref()
        .map(OwnedFile::from_row)
        .ok_or_else(|| AppError::NotFound("文件不存在或已删除".to_string()))
}

async fn breadcrumbs(
    state: &AppState,
    owner_id: i64,
    folder_id: i64,
) -> Result<Vec<Crumb>, AppError> {
    let mut crumbs = Vec::new();
    let mut current = folder_id;
    for _ in 0..64 {
        if current == 0 {
            break;
        }
        let row: Option<(i64, Option<i64>, String)> = sqlx::query_as(
            "SELECT id, parent_id, name FROM files
             WHERE id = ? AND owner_id = ? AND is_dir = 1 AND deleted_at IS NULL",
        )
        .bind(current)
        .bind(owner_id)
        .fetch_optional(&state.pool)
        .await?;
        let Some((id, parent, name)) = row else {
            return Err(AppError::NotFound("文件夹不存在".to_string()));
        };
        crumbs.push(Crumb { id, name });
        current = parent.unwrap_or(0);
    }
    crumbs.reverse();
    Ok(crumbs)
}

pub async fn index(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<FilesQuery>,
) -> Result<Response, AppError> {
    let parent_id = query.path.unwrap_or(0).max(0);
    let parent = resolve_folder(&state, user.0.id, parent_id)
        .await?
        .ok_or_else(|| AppError::NotFound("文件夹不存在".to_string()))?;
    let query_text = clamp_query(query.q.as_deref().unwrap_or(""));
    let sort = normalize_sort(query.sort.as_deref());
    let sort_dir = normalize_dir(query.dir.as_deref());
    let view = normalize_view(query.view.as_deref());
    let crumbs = breadcrumbs(&state, user.0.id, parent.id).await?;

    let entries: Vec<FileItem> = if !query_text.is_empty() {
        let pattern = format!(
            "%{}%",
            query_text
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let order = match (sort, sort_dir) {
            ("size", "desc") => "is_dir DESC, size DESC, name COLLATE NOCASE ASC",
            ("size", _) => "is_dir DESC, size ASC, name COLLATE NOCASE ASC",
            ("modified", "desc") => "is_dir DESC, updated_at DESC, name COLLATE NOCASE ASC",
            ("modified", _) => "is_dir DESC, updated_at ASC, name COLLATE NOCASE ASC",
            (_, "desc") => "is_dir DESC, name COLLATE NOCASE DESC",
            _ => "is_dir DESC, name COLLATE NOCASE ASC",
        };
        let sql = format!(
            "SELECT id, parent_id, name, is_dir, size, mime, created_at, updated_at, storage_key
             FROM files WHERE owner_id = ? AND deleted_at IS NULL AND name LIKE ? ESCAPE '\\'
             ORDER BY {order} LIMIT 500"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user.0.id)
            .bind(&pattern)
            .fetch_all(&state.pool)
            .await?;
        rows.iter()
            .map(OwnedFile::from_row)
            .map(file_item)
            .collect()
    } else {
        let order = match (sort, sort_dir) {
            ("size", "desc") => "is_dir DESC, size DESC, name COLLATE NOCASE ASC",
            ("size", _) => "is_dir DESC, size ASC, name COLLATE NOCASE ASC",
            ("modified", "desc") => "is_dir DESC, updated_at DESC, name COLLATE NOCASE ASC",
            ("modified", _) => "is_dir DESC, updated_at ASC, name COLLATE NOCASE ASC",
            (_, "desc") => "is_dir DESC, name COLLATE NOCASE DESC",
            _ => "is_dir DESC, name COLLATE NOCASE ASC",
        };
        let sql = format!(
            "SELECT id, parent_id, name, is_dir, size, mime, created_at, updated_at, storage_key
             FROM files WHERE owner_id = ? AND deleted_at IS NULL AND parent_id IS ?
             ORDER BY {order} LIMIT 1000"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user.0.id)
            .bind(if parent.id == 0 {
                None
            } else {
                Some(parent.id)
            })
            .fetch_all(&state.pool)
            .await?;
        rows.iter()
            .map(OwnedFile::from_row)
            .map(file_item)
            .collect()
    };

    let dir_label = if sort_dir == "desc" {
        "↓".to_string()
    } else {
        "↑".to_string()
    };
    let common = crate::views::build_common(
        &state,
        &user.0,
        "files",
        query.ok.as_deref(),
        query.err.as_deref(),
    )
    .await?;
    let sort_name_href = files_url(
        parent.id,
        &query_text,
        "name",
        if sort == "name" && sort_dir == "asc" {
            "desc"
        } else {
            "asc"
        },
        view,
    );
    let sort_size_href = files_url(
        parent.id,
        &query_text,
        "size",
        if sort == "size" && sort_dir == "asc" {
            "desc"
        } else {
            "asc"
        },
        view,
    );
    let sort_modified_href = files_url(
        parent.id,
        &query_text,
        "modified",
        if sort == "modified" && sort_dir == "asc" {
            "desc"
        } else {
            "asc"
        },
        view,
    );
    let view_list_href = files_url(parent.id, &query_text, sort, sort_dir, "list");
    let view_grid_href = files_url(parent.id, &query_text, sort, sort_dir, "grid");
    let count = entries.len();
    let page = FilesPage {
        common,
        crumbs,
        entries,
        count,
        dir_label,
        is_search: !query_text.is_empty(),
        query: query_text.clone(),
        query_encoded: urlencoding::encode(&query_text).to_string(),
        parent_id: parent.id,
        sort: sort.to_string(),
        dir: sort_dir.to_string(),
        view: view.to_string(),
        sort_name_href,
        sort_size_href,
        sort_modified_href,
        view_list_href,
        view_grid_href,
        folder_dialog_url: format!("/files/dialog/folder?parent_id={}", parent.id),
        upload_url: "/files/upload".to_string(),
    };
    super::render(&FilesTemplate { page }, StatusCode::OK).await
}

fn file_item(file: OwnedFile) -> FileItem {
    FileItem::new(
        file.id,
        file.name,
        file.is_dir,
        file.size,
        file.mime,
        file.updated_at,
    )
}

pub async fn folder_dialog(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<FolderDialogQuery>,
) -> Result<Response, AppError> {
    let parent_id = query.parent_id.unwrap_or(0).max(0);
    let parent = resolve_folder(&state, user.0.id, parent_id)
        .await?
        .ok_or_else(|| AppError::NotFound("文件夹不存在".to_string()))?;
    let current_crumb = if parent.id == 0 {
        "全部文件".to_string()
    } else {
        format_parent_label(&state, user.0.id, parent.id).await?
    };
    super::render(
        &FolderTemplate {
            page: FolderDialog {
                parent_id: parent.id,
                action: "/files/create-folder".to_string(),
                csrf: user.0.csrf.clone(),
                current_crumb,
            },
        },
        StatusCode::OK,
    )
    .await
}

async fn format_parent_label(state: &AppState, owner_id: i64, id: i64) -> Result<String, AppError> {
    let mut names = Vec::new();
    let mut cur = id;
    for _ in 0..64 {
        if cur == 0 {
            break;
        }
        let row: Option<(Option<i64>, String)> = sqlx::query_as(
            "SELECT parent_id, name FROM files WHERE id = ? AND owner_id = ? AND deleted_at IS NULL",
        )
        .bind(cur)
        .bind(owner_id)
        .fetch_optional(&state.pool)
        .await?;
        let Some((next, name)) = row else { break };
        names.push(name);
        cur = next.unwrap_or(0);
    }
    names.reverse();
    Ok(format!("全部文件 / {}", names.join(" / ")))
}

pub async fn rename_dialog(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let file = load_owned_file(&state, user.0.id, id).await?;
    super::render(
        &RenameTemplate {
            page: RenameDialog {
                file_id: file.id,
                name: file.name,
                is_dir: file.is_dir,
                action: "/files/rename".to_string(),
                csrf: user.0.csrf.clone(),
            },
        },
        StatusCode::OK,
    )
    .await
}

pub async fn move_dialog(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let file = load_owned_file(&state, user.0.id, id).await?;
    let dirs: Vec<(i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT id, parent_id, name FROM files
         WHERE owner_id = ? AND is_dir = 1 AND deleted_at IS NULL",
    )
    .bind(user.0.id)
    .fetch_all(&state.pool)
    .await?;
    let mut excluded = Vec::new();
    if file.is_dir {
        let ids: Vec<i64> = sqlx::query_scalar(
            "WITH RECURSIVE subtree(id) AS (
                SELECT id FROM files WHERE id = ? AND owner_id = ?
                UNION ALL
                SELECT f.id FROM files f JOIN subtree s ON f.parent_id = s.id
                WHERE f.owner_id = ?
             ) SELECT id FROM subtree",
        )
        .bind(file.id)
        .bind(user.0.id)
        .bind(user.0.id)
        .fetch_all(&state.pool)
        .await?;
        excluded = ids;
    }
    let map: std::collections::HashMap<i64, (Option<i64>, String)> = dirs
        .iter()
        .map(|(id, parent, name)| (*id, (*parent, name.clone())))
        .collect();
    let mut folders = vec![FolderOption {
        id: 0,
        label: "全部文件（根目录）".to_string(),
        selected: file.parent_id == 0,
    }];
    for (id, _, _) in &dirs {
        if excluded.contains(id) {
            continue;
        }
        let label = folder_label(*id, &map);
        folders.push(FolderOption {
            id: *id,
            label,
            selected: file.parent_id == *id,
        });
    }
    folders.sort_by(|a, b| a.label.cmp(&b.label));
    super::render(
        &MoveTemplate {
            page: MoveDialog {
                file_id: file.id,
                name: file.name,
                action: "/files/move".to_string(),
                csrf: user.0.csrf.clone(),
                folders,
            },
        },
        StatusCode::OK,
    )
    .await
}

fn folder_label(id: i64, map: &std::collections::HashMap<i64, (Option<i64>, String)>) -> String {
    let mut names = Vec::new();
    let mut cur = id;
    for _ in 0..64 {
        if cur == 0 {
            break;
        }
        let Some((parent, name)) = map.get(&cur) else {
            break;
        };
        names.push(name.clone());
        cur = parent.unwrap_or(0);
    }
    names.reverse();
    format!("全部文件 / {}", names.join(" / "))
}

pub async fn create_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<FolderForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let parent = resolve_folder(&state, user.0.id, form.parent_id)
        .await?
        .ok_or_else(|| AppError::NotFound("文件夹不存在".to_string()))?;
    let name = util::sanitize_filename(&form.name)?;
    let names = storage::active_names(&state.pool, user.0.id, parent.id).await?;
    if names.contains(&name) {
        return Err(AppError::BadRequest("同名文件或文件夹已存在".to_string()));
    }
    let now = util::now_ts();
    sqlx::query(
        "INSERT INTO files (owner_id, parent_id, name, is_dir, size, created_at, updated_at)
         VALUES (?, ?, ?, 1, 0, ?, ?)",
    )
    .bind(user.0.id)
    .bind(if parent.id == 0 {
        None
    } else {
        Some(parent.id)
    })
    .bind(&name)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    Ok(
        super::redirect_msg(&format!("/files?path={}", parent.id), "ok", "文件夹已创建")
            .into_response(),
    )
}

pub async fn rename(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<RenameForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let file = load_owned_file(&state, user.0.id, form.id).await?;
    let name = util::sanitize_filename(&form.name)?;
    let conflict: Option<i64> = if file.parent_id == 0 {
        sqlx::query_scalar(
            "SELECT id FROM files WHERE owner_id = ? AND parent_id IS NULL
             AND deleted_at IS NULL AND name = ? AND id != ?",
        )
        .bind(user.0.id)
        .bind(&name)
        .bind(file.id)
        .fetch_optional(&state.pool)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT id FROM files WHERE owner_id = ? AND parent_id = ?
             AND deleted_at IS NULL AND name = ? AND id != ?",
        )
        .bind(user.0.id)
        .bind(file.parent_id)
        .bind(&name)
        .bind(file.id)
        .fetch_optional(&state.pool)
        .await?
    };
    if conflict.is_some() {
        return Err(AppError::BadRequest("同名文件或文件夹已存在".to_string()));
    }
    let now = util::now_ts();
    sqlx::query("UPDATE files SET name = ?, updated_at = ? WHERE id = ? AND owner_id = ?")
        .bind(&name)
        .bind(now)
        .bind(file.id)
        .bind(user.0.id)
        .execute(&state.pool)
        .await?;
    Ok(
        super::redirect_msg(&format!("/files?path={}", file.parent_id), "ok", "已重命名")
            .into_response(),
    )
}

pub async fn move_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<MoveForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let file = load_owned_file(&state, user.0.id, form.id).await?;
    let target = resolve_folder(&state, user.0.id, form.target_id)
        .await?
        .ok_or_else(|| AppError::NotFound("目标文件夹不存在".to_string()))?;
    if form.target_id == file.id {
        return Err(AppError::BadRequest("不能移动到自身".to_string()));
    }
    if file.is_dir && form.target_id != 0 {
        let is_descendant: i64 = sqlx::query_scalar(
            "WITH RECURSIVE subtree(id) AS (
                SELECT id FROM files WHERE id = ? AND owner_id = ?
                UNION ALL
                SELECT f.id FROM files f JOIN subtree s ON f.parent_id = s.id
                WHERE f.owner_id = ?
             ) SELECT COUNT(*) FROM subtree WHERE id = ?",
        )
        .bind(file.id)
        .bind(user.0.id)
        .bind(user.0.id)
        .bind(form.target_id)
        .fetch_one(&state.pool)
        .await?;
        if is_descendant > 0 {
            return Err(AppError::BadRequest("不能移动到文件夹自身内部".to_string()));
        }
    }
    let names = storage::active_names(&state.pool, user.0.id, target.id).await?;
    if names.contains(&file.name) {
        return Err(AppError::BadRequest(
            "目标文件夹中已存在同名项目".to_string(),
        ));
    }
    let now = util::now_ts();
    sqlx::query("UPDATE files SET parent_id = ?, updated_at = ? WHERE id = ? AND owner_id = ?")
        .bind(if target.id == 0 {
            None
        } else {
            Some(target.id)
        })
        .bind(now)
        .bind(file.id)
        .bind(user.0.id)
        .execute(&state.pool)
        .await?;
    Ok(
        super::redirect_msg(&format!("/files?path={}", file.parent_id), "ok", "已移动")
            .into_response(),
    )
}

pub async fn delete_file(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<DeleteForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let file = load_owned_file(&state, user.0.id, form.id).await?;
    let group = auth::random_token(16);
    let now = util::now_ts();
    sqlx::query(
        "WITH RECURSIVE subtree(id) AS (
            SELECT id FROM files WHERE id = ? AND owner_id = ?
            UNION ALL
            SELECT f.id FROM files f JOIN subtree s ON f.parent_id = s.id
            WHERE f.owner_id = ?
         )
         UPDATE files SET deleted_at = ?, trash_group = ?,
             orig_parent_id = CASE WHEN id = ? THEN parent_id ELSE orig_parent_id END,
             orig_name = CASE WHEN id = ? THEN name ELSE orig_name END
         WHERE id IN (SELECT id FROM subtree) AND deleted_at IS NULL",
    )
    .bind(file.id)
    .bind(user.0.id)
    .bind(user.0.id)
    .bind(now)
    .bind(&group)
    .bind(file.id)
    .bind(file.id)
    .execute(&state.pool)
    .await?;
    let back = form.return_path.unwrap_or(file.parent_id).max(0);
    Ok(super::redirect_msg(&format!("/files?path={back}"), "ok", "已移入回收站").into_response())
}

pub(crate) const TEXT_PREVIEW_LIMIT: usize = 1024 * 1024;

pub(crate) async fn load_text_preview(
    state: &AppState,
    owner_id: i64,
    file: &OwnedFile,
) -> Result<(String, bool), AppError> {
    let key = file
        .storage_key
        .as_deref()
        .ok_or_else(|| AppError::Internal("文件存储信息缺失".to_string()))?;
    let path = state.stored_file_path(owner_id, key);
    let metadata = tokio::fs::metadata(&path).await?;
    let len = metadata.len();
    let limit = TEXT_PREVIEW_LIMIT as u64;
    let reader = tokio::fs::File::open(&path).await?;
    let mut buf = Vec::with_capacity(len.min(limit) as usize);
    let mut reader = reader.take(len.min(limit));
    reader.read_to_end(&mut buf).await?;
    let truncated = len > limit;
    Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
}

pub async fn preview(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let file = load_owned_file(&state, user.0.id, id).await?;
    if file.is_dir {
        return Err(AppError::NotFound("文件夹不支持预览".to_string()));
    }
    let kind = util::preview_kind(&file.name, file.mime.as_deref()).to_string();
    if kind == "none" {
        return Ok(Redirect::to(&format!("/files/{}/download", file.id)).into_response());
    }
    let common = crate::views::build_common(&state, &user.0, "files", None, None).await?;
    let (text_content, text_truncated) = if kind == "text" {
        load_text_preview(&state, user.0.id, &file).await?
    } else {
        (String::new(), false)
    };
    let page = PreviewPage {
        common,
        file_id: file.id,
        name: file.name.clone(),
        size_text: util::human_size(file.size),
        kind,
        mime: file
            .mime
            .clone()
            .unwrap_or_else(|| util::guess_mime(&file.name)),
        raw_url: format!("/files/{}/raw", file.id),
        download_url: format!("/files/{}/download", file.id),
        back_url: format!("/files?path={}", file.parent_id),
        parent_id: file.parent_id,
        text_content,
        text_truncated,
    };
    super::render(&PreviewTemplate { page }, StatusCode::OK).await
}

pub async fn raw(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let file = load_owned_file(&state, user.0.id, id).await?;
    if file.is_dir {
        return Err(AppError::NotFound("文件夹没有内容预览".to_string()));
    }
    if util::preview_kind(&file.name, file.mime.as_deref()) == "none" {
        return Ok(Redirect::to(&format!("/files/{}/download", file.id)).into_response());
    }
    serve_owned_file(&state, user.0.id, &file, true, &headers).await
}

pub async fn download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let file = load_owned_file(&state, user.0.id, id).await?;
    if file.is_dir {
        return Err(AppError::BadRequest("不能直接下载文件夹".to_string()));
    }
    serve_owned_file(&state, user.0.id, &file, false, &headers).await
}

fn safe_image_type(mime: &str) -> &'static str {
    match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/bmp" => "image/bmp",
        "image/avif" => "image/avif",
        _ => "application/octet-stream",
    }
}

pub(crate) async fn serve_owned_file(
    state: &AppState,
    owner_id: i64,
    file: &OwnedFile,
    inline: bool,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let key = file
        .storage_key
        .as_deref()
        .ok_or_else(|| AppError::Internal("文件存储信息缺失".to_string()))?;
    let path = state.stored_file_path(owner_id, key);
    let metadata = tokio::fs::metadata(&path).await.map_err(|e| {
        tracing::error!("读取文件元数据失败 {}: {e}", path.display());
        AppError::NotFound("文件内容已丢失".to_string())
    })?;
    let size = metadata.len();
    let kind = util::preview_kind(&file.name, file.mime.as_deref());
    let guessed = util::guess_mime(&file.name);
    let content_type: String = if !inline {
        "application/octet-stream".to_string()
    } else {
        match kind {
            "image" => safe_image_type(&guessed).to_string(),
            "video" if guessed.starts_with("video/") => guessed,
            "audio" if guessed.starts_with("audio/") => guessed,
            "video" => "application/octet-stream".to_string(),
            "audio" => "application/octet-stream".to_string(),
            "pdf" => "application/pdf".to_string(),
            "text" => "text/plain; charset=utf-8".to_string(),
            _ => "application/octet-stream".to_string(),
        }
    };
    let disposition =
        util::content_disposition(if inline { "inline" } else { "attachment" }, &file.name);
    let range_header = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());
    let range = range_header
        .as_deref()
        .and_then(|value| util::parse_range(value, size));

    if range_header.is_some() && range.is_none() {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{size}"))
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::empty())
            .map_err(AppError::from);
    }

    let mut reader = tokio::fs::File::open(&path).await?;
    let (status, start, length) = match range {
        Some((start, end)) => {
            reader.seek(std::io::SeekFrom::Start(start)).await?;
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
        None => (StatusCode::OK, 0, size),
    };
    let reader = reader.take(length);
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "private, no-store");
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{size}", start + length - 1),
        );
    }
    let body = Body::from_stream(ReaderStream::new(reader));
    builder.body(body).map_err(AppError::from)
}

struct TempFileRec {
    name: String,
    path: PathBuf,
    size: i64,
}

struct TempGuard {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl TempGuard {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            armed: true,
        }
    }
    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
    fn forget(&mut self, path: &std::path::Path) {
        self.paths.retain(|p| p != path);
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub async fn upload(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let mut guard = TempGuard::new();
    let mut temps: Vec<TempFileRec> = Vec::new();
    let mut csrf = String::new();
    let mut parent_id = 0i64;
    let mut rel_path = String::new();
    let max_total = state.config.max_upload_bytes() as i64;
    let mut total: i64 = 0;

    while let Some(mut field) = multipart.next_field().await? {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name == "files" {
            let filename = field.file_name().unwrap_or("").to_string();
            if filename.is_empty() {
                continue;
            }
            let token = auth::random_token(16);
            let temp_path = state.temp_path(&format!("up-{token}"));
            let mut output = tokio::fs::File::create(&temp_path).await?;
            guard.track(temp_path.clone());
            let mut size: i64 = 0;
            while let Some(chunk) = field.chunk().await? {
                size = size.saturating_add(chunk.len() as i64);
                total = total.saturating_add(chunk.len() as i64);
                if size > max_total || total > max_total {
                    output.shutdown().await.ok();
                    return Err(AppError::BadRequest(format!(
                        "文件太大，单次最多上传 {}",
                        util::human_size(max_total)
                    )));
                }
                output.write_all(&chunk).await?;
            }
            output.sync_all().await?;
            drop(output);
            temps.push(TempFileRec {
                name: filename,
                path: temp_path,
                size,
            });
        } else {
            let value = field.text().await?;
            match field_name.as_str() {
                "csrf" => csrf = value,
                "parent_id" => {
                    parent_id = value.trim().parse::<i64>().unwrap_or(0).max(0);
                }
                "path" => rel_path = value,
                _ => {}
            }
        }
    }

    auth::require_csrf(&user.0, &csrf)?;
    if temps.is_empty() {
        return Err(AppError::BadRequest("没有收到文件".to_string()));
    }
    let parent = resolve_folder(&state, user.0.id, parent_id)
        .await?
        .ok_or_else(|| AppError::NotFound("目标文件夹不存在".to_string()))?;
    let components = util::sanitize_relative_components(&rel_path)?;
    storage::ensure_quota(&state.pool, user.0.id, user.0.quota_bytes, total).await?;

    let mut uploaded = 0usize;
    for temp in &temps {
        let filename = util::sanitize_filename(&temp.name)?;
        match storage::store_temp_file(
            &state,
            user.0.id,
            parent.id,
            &components,
            &filename,
            &temp.path,
            temp.size,
        )
        .await
        {
            Ok(_) => {
                guard.forget(&temp.path);
                uploaded += 1;
            }
            Err(err) => {
                tracing::warn!("保存上传文件失败: {err}");
                return Err(err);
            }
        }
    }
    guard.disarm();
    Ok(super::redirect_msg(
        &format!("/files?path={}", parent.id),
        "ok",
        &format!("已上传 {uploaded} 个文件"),
    )
    .into_response())
}

#[derive(sqlx::FromRow)]
struct UploadRow {
    user_id: i64,
    parent_id: Option<i64>,
    dir_path: String,
    filename: String,
    total_size: i64,
    received: i64,
    temp_path: String,
}

async fn load_upload(state: &AppState, token: &str) -> Result<UploadRow, AppError> {
    sqlx::query_as::<_, UploadRow>(
        "SELECT user_id, parent_id, dir_path, filename, total_size, received, temp_path
         FROM uploads WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("上传已中断，请重新上传".to_string()))
}

pub async fn upload_init(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<UploadInitForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    if form.size < 0 {
        return Err(AppError::BadRequest("文件大小不正确".to_string()));
    }
    let parent_id = form.parent_id.unwrap_or(0).max(0);
    let parent = resolve_folder(&state, user.0.id, parent_id)
        .await?
        .ok_or_else(|| AppError::NotFound("目标文件夹不存在".to_string()))?;
    let components = util::sanitize_relative_components(&form.path)?;
    let filename = util::sanitize_filename(&form.name)?;
    storage::ensure_quota(&state.pool, user.0.id, user.0.quota_bytes, form.size).await?;
    let token = auth::random_token(24);
    let temp_path = state.temp_path(&format!("chunk-{token}"));
    tokio::fs::File::create(&temp_path).await?;
    let now = util::now_ts();
    let result = sqlx::query(
        "INSERT INTO uploads (token, user_id, parent_id, dir_path, filename, total_size, received, temp_path, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(&token)
    .bind(user.0.id)
    .bind(if parent.id == 0 { None } else { Some(parent.id) })
    .bind(components.join("/"))
    .bind(&filename)
    .bind(form.size)
    .bind(temp_path.to_string_lossy().to_string())
    .bind(now)
    .execute(&state.pool)
    .await;
    if let Err(err) = result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(AppError::from(err));
    }
    Ok(axum::Json(serde_json::json!({
        "token": token,
        "received": 0,
        "size": form.size
    }))
    .into_response())
}

pub async fn upload_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let upload = load_upload(&state, &token).await?;
    if upload.user_id != user.0.id {
        return Err(AppError::NotFound("上传已中断，请重新上传".to_string()));
    }
    if !std::path::Path::new(&upload.temp_path).exists() {
        return Err(AppError::NotFound("上传已中断，请重新上传".to_string()));
    }
    Ok(axum::Json(serde_json::json!({
        "token": token,
        "received": upload.received,
        "size": upload.total_size
    }))
    .into_response())
}

pub async fn upload_chunk(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    auth::require_csrf(&user.0, csrf)?;
    let upload = load_upload(&state, &token).await?;
    if upload.user_id != user.0.id {
        return Err(AppError::NotFound("上传已中断，请重新上传".to_string()));
    }
    let offset: i64 = headers
        .get("x-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| AppError::BadRequest("上传进度异常，请重新上传".to_string()))?;
    if offset != upload.received {
        return Err(AppError::BadRequest("上传没有对上进度，请重新上传".into()));
    }
    if body.len() > 16 * 1024 * 1024 {
        return Err(AppError::BadRequest("上传数据过大".to_string()));
    }
    if upload.received.saturating_add(body.len() as i64) > upload.total_size {
        return Err(AppError::BadRequest(
            "上传数据超过文件大小，请重新上传".to_string(),
        ));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&upload.temp_path)
        .await?;
    file.write_all(&body).await?;
    drop(file);
    let new_received = upload.received + body.len() as i64;
    let updated = sqlx::query("UPDATE uploads SET received = ? WHERE token = ? AND received = ?")
        .bind(new_received)
        .bind(&token)
        .bind(upload.received)
        .execute(&state.pool)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "多个上传同时进行，请稍后重试".to_string(),
        ));
    }
    Ok(axum::Json(serde_json::json!({
        "received": new_received,
        "size": upload.total_size
    }))
    .into_response())
}

pub async fn upload_complete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    auth::require_csrf(&user.0, csrf)?;
    let upload = load_upload(&state, &token).await?;
    if upload.user_id != user.0.id {
        return Err(AppError::NotFound("上传已中断，请重新上传".to_string()));
    }
    if upload.received != upload.total_size {
        return Err(AppError::BadRequest(format!(
            "上传还未完成：{} / {}",
            upload.received, upload.total_size
        )));
    }
    let parent_id = upload.parent_id.unwrap_or(0);
    let parent = resolve_folder(&state, user.0.id, parent_id)
        .await?
        .ok_or_else(|| AppError::NotFound("目标文件夹不存在".to_string()))?;
    let components = util::sanitize_relative_components(&upload.dir_path)?;
    storage::ensure_quota(
        &state.pool,
        user.0.id,
        user.0.quota_bytes,
        upload.total_size,
    )
    .await?;
    let temp_path = PathBuf::from(&upload.temp_path);
    if let Ok(file) = tokio::fs::OpenOptions::new()
        .read(true)
        .open(&temp_path)
        .await
    {
        let _ = file.sync_all().await;
    }
    let filename = util::sanitize_filename(&upload.filename)?;
    let id = storage::store_temp_file(
        &state,
        user.0.id,
        parent.id,
        &components,
        &filename,
        &temp_path,
        upload.total_size,
    )
    .await?;
    sqlx::query("DELETE FROM uploads WHERE token = ?")
        .bind(&token)
        .execute(&state.pool)
        .await?;
    Ok(axum::Json(serde_json::json!({ "id": id })).into_response())
}
