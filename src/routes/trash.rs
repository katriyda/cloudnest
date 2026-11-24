use crate::auth::{self, CurrentUser};
use crate::error::AppError;
use crate::state::AppState;
use crate::storage;
use crate::util;
use crate::views::{TrashItem, TrashPage, TrashTemplate};
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sqlx::Row;

#[derive(Deserialize, Default)]
pub struct TrashQuery {
    #[serde(default)]
    pub ok: Option<String>,
    #[serde(default)]
    pub err: Option<String>,
}

#[derive(Deserialize)]
pub struct TrashActionForm {
    pub csrf: String,
    pub id: Option<i64>,
}

pub async fn index(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<TrashQuery>,
) -> Result<Response, AppError> {
    let common = crate::views::build_common(
        &state,
        &user.0,
        "trash",
        query.ok.as_deref(),
        query.err.as_deref(),
    )
    .await?;
    let rows = sqlx::query(
        "SELECT id, name, is_dir, size, deleted_at FROM files f
         WHERE owner_id = ? AND deleted_at IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM files p WHERE p.id = f.parent_id AND p.deleted_at IS NOT NULL
           )
         ORDER BY deleted_at DESC LIMIT 1000",
    )
    .bind(user.0.id)
    .fetch_all(&state.pool)
    .await?;
    let entries = rows
        .iter()
        .map(|row| {
            let is_dir = row.get::<i64, _>("is_dir") != 0;
            let size = row.get::<i64, _>("size");
            let name: String = row.get("name");
            TrashItem {
                id: row.get("id"),
                name: name.clone(),
                is_dir,
                size_text: if is_dir {
                    "文件夹".to_string()
                } else {
                    util::human_size(size)
                },
                deleted_text: util::format_ts(row.get("deleted_at")),
                ext_label: if is_dir {
                    String::new()
                } else {
                    util::extension_label(&name)
                },
                hue: util::hue_for(&name),
            }
        })
        .collect();
    let page = TrashPage { common, entries };
    super::render(&TrashTemplate { page }, StatusCode::OK).await
}

async fn load_trashed(
    state: &AppState,
    owner_id: i64,
    id: i64,
) -> Result<(String, Option<i64>, String), AppError> {
    type TrashRow = (String, Option<String>, Option<i64>, Option<i64>);
    let row: Option<TrashRow> = sqlx::query_as(
        "SELECT name, trash_group, parent_id, orig_parent_id FROM files
         WHERE id = ? AND owner_id = ? AND deleted_at IS NOT NULL",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&state.pool)
    .await?;
    match row {
        Some((name, group, parent, orig_parent)) => {
            let group = group.unwrap_or_else(|| format!("legacy:{id}"));
            Ok((group, orig_parent.or(parent), name))
        }
        None => Err(AppError::NotFound("回收站中找不到该项目".to_string())),
    }
}

pub async fn restore(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<TrashActionForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let id = form
        .id
        .ok_or_else(|| AppError::BadRequest("缺少项目".to_string()))?;
    let (group, parent_id, name) = load_trashed(&state, user.0.id, id).await?;

    let target_parent = if let Some(parent) = parent_id {
        let status: Option<(i64, Option<i64>)> =
            sqlx::query_as("SELECT is_dir, deleted_at FROM files WHERE id = ? AND owner_id = ?")
                .bind(parent)
                .bind(user.0.id)
                .fetch_optional(&state.pool)
                .await?;
        match status {
            Some((is_dir, deleted)) if is_dir != 0 && deleted.is_none() => Some(parent),
            _ => None,
        }
    } else {
        None
    };

    // Resolve a name collision at the destination.
    let names = storage::active_names(&state.pool, user.0.id, target_parent.unwrap_or(0)).await?;
    let final_name = if names.contains(&name) {
        util::unique_name(&name, &names)
    } else {
        name
    };
    let now = util::now_ts();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE files SET deleted_at = NULL, trash_group = NULL, orig_parent_id = NULL, orig_name = NULL
         WHERE owner_id = ? AND trash_group = ?",
    )
    .bind(user.0.id)
    .bind(&group)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE files SET name = ?, parent_id = ?, updated_at = ? WHERE id = ? AND owner_id = ?",
    )
    .bind(&final_name)
    .bind(target_parent)
    .bind(now)
    .bind(id)
    .bind(user.0.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(super::redirect_msg("/trash", "ok", "已恢复").into_response())
}

pub async fn delete_forever(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<TrashActionForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let id = form
        .id
        .ok_or_else(|| AppError::BadRequest("缺少项目".to_string()))?;
    let (group, _, _) = load_trashed(&state, user.0.id, id).await?;
    let keys = storage::trash_group_keys(&state.pool, user.0.id, &group).await?;
    sqlx::query("DELETE FROM files WHERE owner_id = ? AND trash_group = ?")
        .bind(user.0.id)
        .bind(&group)
        .execute(&state.pool)
        .await?;
    storage::delete_stored_keys(&state, user.0.id, &keys).await;
    Ok(super::redirect_msg("/trash", "ok", "已永久删除").into_response())
}

pub async fn empty(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<TrashActionForm>,
) -> Result<Response, AppError> {
    auth::require_csrf(&user.0, &form.csrf)?;
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT storage_key FROM files WHERE owner_id = ? AND deleted_at IS NOT NULL AND storage_key IS NOT NULL",
    )
    .bind(user.0.id)
    .fetch_all(&state.pool)
    .await?;
    sqlx::query("DELETE FROM files WHERE owner_id = ? AND deleted_at IS NOT NULL")
        .bind(user.0.id)
        .execute(&state.pool)
        .await?;
    storage::delete_stored_keys(&state, user.0.id, &keys).await;
    Ok(super::redirect_msg("/trash", "ok", "回收站已清空").into_response())
}
