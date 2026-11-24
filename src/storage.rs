use crate::error::AppError;
use crate::state::AppState;
use crate::util;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::path::Path;
use tokio::io::AsyncWriteExt;

pub async fn usage_bytes(pool: &SqlitePool, owner_id: i64) -> Result<i64, AppError> {
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size), 0) FROM files WHERE owner_id = ? AND is_dir = 0",
    )
    .bind(owner_id)
    .fetch_one(pool)
    .await?;
    Ok(used)
}

pub async fn ensure_quota(
    pool: &SqlitePool,
    owner_id: i64,
    quota_bytes: i64,
    additional: i64,
) -> Result<(), AppError> {
    if quota_bytes <= 0 {
        return Ok(());
    }
    let used = usage_bytes(pool, owner_id).await?;
    if used.saturating_add(additional) > quota_bytes {
        return Err(AppError::BadRequest(format!(
            "存储空间不足：已用 {}，本次需要 {}，配额 {}",
            util::human_size(used),
            util::human_size(additional),
            util::human_size(quota_bytes)
        )));
    }
    Ok(())
}

pub async fn active_names(
    pool: &SqlitePool,
    owner_id: i64,
    parent_id: i64,
) -> Result<HashSet<String>, AppError> {
    let names: Vec<String> = if parent_id == 0 {
        sqlx::query_scalar(
            "SELECT name FROM files WHERE owner_id = ? AND parent_id IS NULL AND deleted_at IS NULL",
        )
    } else {
        sqlx::query_scalar(
            "SELECT name FROM files WHERE owner_id = ? AND parent_id = ? AND deleted_at IS NULL",
        )
        .bind(parent_id)
    }
    .bind(owner_id)
    .fetch_all(pool)
    .await?;
    Ok(names.into_iter().collect())
}

pub async fn ensure_folder_path(
    state: &AppState,
    owner_id: i64,
    mut parent_id: i64,
    components: &[String],
) -> Result<i64, AppError> {
    let now = util::now_ts();
    for component in components {
        let existing: Option<(i64, i64)> = if parent_id == 0 {
            sqlx::query_as(
                "SELECT id, is_dir FROM files WHERE owner_id = ? AND parent_id IS NULL
                 AND name = ? AND deleted_at IS NULL",
            )
            .bind(owner_id)
            .bind(component)
            .fetch_optional(&state.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, is_dir FROM files WHERE owner_id = ? AND parent_id = ?
                 AND name = ? AND deleted_at IS NULL",
            )
            .bind(owner_id)
            .bind(parent_id)
            .bind(component)
            .fetch_optional(&state.pool)
            .await?
        };
        if let Some((id, is_dir)) = existing {
            if is_dir == 0 {
                return Err(AppError::BadRequest(format!(
                    "路径中的“{component}”已是一个文件"
                )));
            }
            parent_id = id;
            continue;
        }
        let parent_bind = if parent_id == 0 {
            None
        } else {
            Some(parent_id)
        };
        let result = sqlx::query(
            "INSERT INTO files (owner_id, parent_id, name, is_dir, size, created_at, updated_at)
             VALUES (?, ?, ?, 1, 0, ?, ?)",
        )
        .bind(owner_id)
        .bind(parent_bind)
        .bind(component)
        .bind(now)
        .bind(now)
        .execute(&state.pool)
        .await?;
        parent_id = result.last_insert_rowid();
    }
    Ok(parent_id)
}

/// Move a fully written temporary file into user storage and create its metadata row.
pub async fn store_temp_file(
    state: &AppState,
    owner_id: i64,
    parent_id: i64,
    components: &[String],
    filename: &str,
    temp_path: &Path,
    size: i64,
) -> Result<i64, AppError> {
    let parent_id = ensure_folder_path(state, owner_id, parent_id, components).await?;
    let names = active_names(&state.pool, owner_id, parent_id).await?;
    let final_name = util::unique_name(filename, &names);
    let storage_key = crate::auth::random_token(20);
    let user_dir = state.user_dir(owner_id);
    tokio::fs::create_dir_all(&user_dir).await?;
    let final_path = user_dir.join(&storage_key);
    tokio::fs::rename(temp_path, &final_path).await?;
    let now = util::now_ts();
    let parent_bind = if parent_id == 0 {
        None
    } else {
        Some(parent_id)
    };
    let mime = util::guess_mime(&final_name);
    let result = sqlx::query(
        "INSERT INTO files (owner_id, parent_id, name, is_dir, size, mime, storage_key, created_at, updated_at)
         VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?)",
    )
    .bind(owner_id)
    .bind(parent_bind)
    .bind(&final_name)
    .bind(size)
    .bind(&mime)
    .bind(&storage_key)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await;
    match result {
        Ok(row) => {
            if parent_id > 0 {
                let _ = sqlx::query("UPDATE files SET updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(parent_id)
                    .execute(&state.pool)
                    .await;
            }
            Ok(row.last_insert_rowid())
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&final_path).await;
            Err(AppError::from(err))
        }
    }
}

/// Extract every storage key within a trashed tree (root + descendants).
pub async fn trash_group_keys(
    pool: &SqlitePool,
    owner_id: i64,
    trash_group: &str,
) -> Result<Vec<String>, AppError> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT storage_key FROM files
         WHERE owner_id = ? AND trash_group = ? AND storage_key IS NOT NULL",
    )
    .bind(owner_id)
    .bind(trash_group)
    .fetch_all(pool)
    .await?;
    Ok(keys)
}

pub async fn delete_stored_keys(state: &AppState, owner_id: i64, keys: &[String]) {
    for key in keys {
        let path = state.stored_file_path(owner_id, key);
        if let Err(err) = tokio::fs::remove_file(&path).await {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("无法删除存储文件 {}: {err}", path.display());
            }
        }
    }
}

pub async fn write_all_and_sync(file: &mut tokio::fs::File, bytes: &[u8]) -> Result<(), AppError> {
    file.write_all(bytes).await?;
    Ok(())
}

pub async fn cleanup_stale_uploads(state: &AppState, older_than: i64) -> Result<(), AppError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT token, temp_path FROM uploads WHERE created_at < ?")
            .bind(older_than)
            .fetch_all(&state.pool)
            .await?;
    for (token, temp_path) in rows {
        let _ = tokio::fs::remove_file(&temp_path).await;
        let _ = sqlx::query("DELETE FROM uploads WHERE token = ?")
            .bind(&token)
            .execute(&state.pool)
            .await;
    }
    Ok(())
}
