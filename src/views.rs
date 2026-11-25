use crate::error::AppError;
use crate::state::AppState;
use crate::util;
use askama::Template;

pub struct Flash {
    pub kind: String,
    pub message: String,
}

pub struct Common {
    pub user_id: i64,
    pub username: String,
    pub initial: String,
    pub is_admin: bool,
    pub csrf: String,
    pub active: String,
    pub flash: Option<Flash>,
    pub used_text: String,
    pub quota_text: String,
    pub used_percent: u8,
    pub quota_unlimited: bool,
    pub trash_count: i64,
    pub used_bytes: i64,
    pub quota_bytes: i64,
    pub file_count: i64,
}

/// 提示信息来自 URL 查询参数，只保留可见字符并限制长度，避免被拼接成长篇欺骗文案。
fn flash_message(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

pub async fn build_common(
    state: &AppState,
    user: &crate::auth::SessionUser,
    active: &str,
    ok: Option<&str>,
    err: Option<&str>,
) -> Result<Common, AppError> {
    let used_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size), 0) FROM files WHERE owner_id = ? AND is_dir = 0",
    )
    .bind(user.id)
    .fetch_one(&state.pool)
    .await?;
    let file_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE owner_id = ? AND is_dir = 0")
            .bind(user.id)
            .fetch_one(&state.pool)
            .await?;
    let trash_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM files f
         WHERE f.owner_id = ? AND f.deleted_at IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM files p WHERE p.id = f.parent_id AND p.deleted_at IS NOT NULL
           )",
    )
    .bind(user.id)
    .fetch_one(&state.pool)
    .await?;
    let quota_unlimited = user.quota_bytes <= 0;
    let percent = if quota_unlimited || user.quota_bytes <= 0 {
        0
    } else {
        ((used_bytes as f64 / user.quota_bytes as f64) * 100.0)
            .ceil()
            .clamp(0.0, 100.0) as u8
    };
    let flash = match (ok, err) {
        (Some(message), _) if !message.is_empty() => Some(Flash {
            kind: "ok".to_string(),
            message: flash_message(message),
        }),
        (_, Some(message)) if !message.is_empty() => Some(Flash {
            kind: "err".to_string(),
            message: flash_message(message),
        }),
        _ => None,
    };
    Ok(Common {
        user_id: user.id,
        username: user.username.clone(),
        initial: user
            .username
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string(),
        is_admin: user.is_admin,
        csrf: user.csrf.clone(),
        active: active.to_string(),
        flash,
        used_text: util::human_size(used_bytes),
        quota_text: if quota_unlimited {
            "不限".to_string()
        } else {
            util::human_size(user.quota_bytes)
        },
        used_percent: percent,
        quota_unlimited,
        trash_count,
        used_bytes,
        quota_bytes: user.quota_bytes,
        file_count,
    })
}

pub struct Crumb {
    pub id: i64,
    pub name: String,
}

pub struct FileItem {
    pub id: i64,
    pub name: String,
    pub is_dir: bool,
    pub size_text: String,
    pub updated_text: String,
    pub preview: String,
    pub ext_label: String,
    pub type_text: &'static str,
    pub hue: u32,
    pub mime: String,
}

impl FileItem {
    pub fn new(
        id: i64,
        name: String,
        is_dir: bool,
        size: i64,
        mime: Option<String>,
        updated_at: i64,
    ) -> Self {
        let preview = if is_dir {
            "none".to_string()
        } else {
            util::preview_kind(&name, mime.as_deref()).to_string()
        };
        let ext_label = if is_dir {
            String::new()
        } else {
            util::extension_label(&name)
        };
        let type_text = util::friendly_type(&name, mime.as_deref());
        let hue = util::hue_for(&name);
        Self {
            id,
            name,
            is_dir,
            size_text: if is_dir {
                "—".to_string()
            } else {
                util::human_size(size)
            },
            updated_text: util::format_ts(updated_at),
            preview,
            ext_label,
            type_text,
            hue,
            mime: mime.unwrap_or_default(),
        }
    }
}

pub struct FilesPage {
    pub common: Common,
    pub crumbs: Vec<Crumb>,
    pub entries: Vec<FileItem>,
    pub count: usize,
    pub dir_label: String,
    pub is_search: bool,
    pub query: String,
    pub query_encoded: String,
    pub parent_id: i64,
    pub sort: String,
    pub dir: String,
    pub view: String,
    pub sort_name_href: String,
    pub sort_size_href: String,
    pub sort_modified_href: String,
    pub view_list_href: String,
    pub view_grid_href: String,
    pub folder_dialog_url: String,
    pub upload_url: String,
}

pub struct TrashItem {
    pub id: i64,
    pub name: String,
    pub is_dir: bool,
    pub size_text: String,
    pub deleted_text: String,
    pub ext_label: String,
    pub hue: u32,
}

pub struct TrashPage {
    pub common: Common,
    pub entries: Vec<TrashItem>,
}

pub struct ShareItem {
    pub id: i64,
    pub token: String,
    pub name: String,
    pub is_dir: bool,
    pub url: String,
    pub has_password: bool,
    pub expires_text: String,
    pub downloads_text: String,
    pub created_text: String,
    pub active: bool,
    pub download_count: i64,
}

pub struct SharesPage {
    pub common: Common,
    pub shares: Vec<ShareItem>,
    pub new_share_url: String,
}

pub struct AdminUserRow {
    pub id: i64,
    pub username: String,
    pub initial: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub used_text: String,
    pub quota_text: String,
    pub percent: u8,
    pub quota_unlimited: bool,
    pub quota_gb: i64,
    pub file_count: i64,
    pub created_text: String,
    pub is_self: bool,
}

pub struct AdminPage {
    pub common: Common,
    pub users: Vec<AdminUserRow>,
    pub total_users: i64,
    pub total_files: i64,
    pub total_size_text: String,
    pub total_quota_text: String,
    pub default_quota_gb: i64,
}

pub struct AccountPage {
    pub common: Common,
    pub created_text: String,
    pub user_file_count: i64,
}

pub struct SettingsPage {
    pub common: Common,
    /// 当前运行版本与平台。
    pub current_version: String,
    pub update_supported: bool,
    pub platform_label: String,
    /// 配置文件路径；为空表示当前进程没有可写回的配置文件。
    pub config_path: String,
    /// 磁盘上保存的值（编辑表单以此为准）。
    pub listen: String,
    pub public_url: String,
    pub secure_cookies: bool,
    pub max_upload_size_mb: u64,
    pub default_quota_gb: i64,
    pub session_days: i64,
    /// 运行中的监听地址，用于提示“重启后生效”。
    pub running_listen: String,
    pub listen_pending: bool,
    pub data_dir: String,
    pub env_overrides: Vec<String>,
}

pub struct LoginPage {
    pub error: Option<String>,
    pub next: String,
    pub username: String,
}

pub struct SetupPage {
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub status: u16,
    pub title: String,
    pub message: String,
}

pub struct PublicEntry {
    pub id: i64,
    pub name: String,
    pub is_dir: bool,
    pub size_text: String,
    pub updated_text: String,
    pub ext_label: String,
    pub hue: u32,
    pub open_href: String,
    pub download_href: String,
    pub preview: String,
}

pub struct ShareCrumb {
    pub id: i64,
    pub name: String,
    pub href: String,
}

pub struct SharePage {
    pub token: String,
    pub owner_name: String,
    pub root_name: String,
    pub is_file: bool,
    pub file_name: String,
    pub file_size_text: String,
    pub file_preview: String,
    pub file_ext: String,
    pub file_hue: u32,
    pub raw_url: String,
    pub download_url: String,
    pub crumbs: Vec<ShareCrumb>,
    pub entries: Vec<PublicEntry>,
    pub downloads_left: String,
    pub expires_text: String,
    pub is_empty: bool,
    pub back_url: String,
    pub text_content: String,
    pub text_truncated: bool,
    /// 该分享设置了下载次数上限：不自动内联预览，避免预览消耗次数或绕过计数。
    pub download_limited: bool,
}

pub struct SharePasswordPage {
    pub token: String,
    pub owner_name: String,
    pub root_name: String,
    pub error: Option<String>,
}

pub struct RenameDialog {
    pub file_id: i64,
    pub name: String,
    pub is_dir: bool,
    pub action: String,
    pub csrf: String,
}

pub struct MoveDialog {
    pub file_id: i64,
    pub name: String,
    pub action: String,
    pub csrf: String,
    pub folders: Vec<FolderOption>,
}

pub struct FolderOption {
    pub id: i64,
    pub label: String,
    pub selected: bool,
}

pub struct FolderDialog {
    pub parent_id: i64,
    pub action: String,
    pub csrf: String,
    pub current_crumb: String,
}

pub struct ShareDialog {
    pub action: String,
    pub csrf: String,
    pub files: Vec<ShareFileOption>,
    pub fixed_file: bool,
    pub selected_id: i64,
    pub selected_name: String,
    pub is_dir: bool,
    pub min_expiry: String,
}

pub struct ShareFileOption {
    pub id: i64,
    pub name: String,
    pub is_dir: bool,
}

pub struct PreviewPage {
    pub common: Common,
    pub file_id: i64,
    pub name: String,
    pub size_text: String,
    pub kind: String,
    pub mime: String,
    pub raw_url: String,
    pub download_url: String,
    pub back_url: String,
    pub parent_id: i64,
    pub text_content: String,
    pub text_truncated: bool,
}

pub struct FolderMoveRow {
    pub id: i64,
    pub label: String,
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub page: LoginPage,
}

#[derive(Template)]
#[template(path = "setup.html")]
pub struct SetupTemplate {
    pub page: SetupPage,
}

#[derive(Template)]
#[template(path = "files.html")]
pub struct FilesTemplate {
    pub page: FilesPage,
}

#[derive(Template)]
#[template(path = "trash.html")]
pub struct TrashTemplate {
    pub page: TrashPage,
}

#[derive(Template)]
#[template(path = "shares.html")]
pub struct SharesTemplate {
    pub page: SharesPage,
}

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub page: SettingsPage,
}

#[derive(Template)]
#[template(path = "admin.html")]
pub struct AdminTemplate {
    pub page: AdminPage,
}

#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountTemplate {
    pub page: AccountPage,
}

#[derive(Template)]
#[template(path = "preview.html")]
pub struct PreviewTemplate {
    pub page: PreviewPage,
}

#[derive(Template)]
#[template(path = "share.html")]
pub struct ShareTemplate {
    pub page: SharePage,
}

#[derive(Template)]
#[template(path = "share_password.html")]
pub struct SharePasswordTemplate {
    pub page: SharePasswordPage,
}

#[derive(Template)]
#[template(path = "dialogs/rename.html")]
pub struct RenameTemplate {
    pub page: RenameDialog,
}

#[derive(Template)]
#[template(path = "dialogs/move.html")]
pub struct MoveTemplate {
    pub page: MoveDialog,
}

#[derive(Template)]
#[template(path = "dialogs/folder.html")]
pub struct FolderTemplate {
    pub page: FolderDialog,
}

#[derive(Template)]
#[template(path = "dialogs/share.html")]
pub struct ShareDialogTemplate {
    pub page: ShareDialog,
}
