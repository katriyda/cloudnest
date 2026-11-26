pub mod admin;
pub mod auth_routes;
pub mod files;
pub mod settings;
pub mod shares;
pub mod trash;

use crate::assets;
use crate::auth::AdminUser;
use crate::state::AppState;
use askama::Template;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

// 静态资源本体、内容哈希地址与缓存策略都在 crate::assets 中维护。

/// 非上传接口的请求体上限。表单/JSON 提取器会整体缓冲请求体，必须保持很小，
/// 否则未认证接口（例如 /login）可被超大请求体耗尽内存。
const FORM_BODY_BYTES: usize = 1024 * 1024;

/// 仅对 multipart 上传接口放宽到配置值（流式读取）；分片接口由路由级 16 MiB 覆盖。
async fn body_limits(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let limit = if request.uri().path() == "/files/upload" {
        state.config().max_upload_bytes() as usize
    } else {
        FORM_BODY_BYTES
    };
    DefaultBodyLimit::max(limit).apply(&mut request);
    next.run(request).await
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(health))
        // 当前版本：/static/v/<内容哈希>/…，可以长期缓存。
        .route("/static/v/{version}/{file}", get(asset_versioned))
        .route(
            "/static/v/{version}/fonts/{file}",
            get(asset_font_versioned),
        )
        // 旧固定地址：保留临时跳转，兼容升级前缓存的旧页面。
        .route("/static/{file}", get(asset_legacy))
        .route("/static/fonts/{file}", get(asset_font_legacy))
        .route(
            "/login",
            get(auth_routes::login_page).post(auth_routes::login_submit),
        )
        .route(
            "/setup",
            get(auth_routes::setup_page).post(auth_routes::setup_submit),
        )
        .route("/logout", post(auth_routes::logout))
        .route("/account", get(auth_routes::account_page))
        .route("/account/password", post(auth_routes::account_password))
        .route("/files", get(files::index))
        .route("/files/upload", post(files::upload))
        .route("/files/delete", post(files::delete_file))
        .route("/files/rename", post(files::rename))
        .route("/files/move", post(files::move_file))
        .route("/files/create-folder", post(files::create_folder))
        .route("/files/dialog/folder", get(files::folder_dialog))
        .route("/files/{id}/dialog/rename", get(files::rename_dialog))
        .route("/files/{id}/dialog/move", get(files::move_dialog))
        .route("/files/{id}/preview", get(files::preview))
        .route("/files/{id}/raw", get(files::raw))
        .route("/files/{id}/download", get(files::download))
        .route("/api/upload/init", post(files::upload_init))
        .route("/api/upload/{token}", get(files::upload_status))
        .route(
            "/api/upload/chunk/{token}",
            post(files::upload_chunk).layer(DefaultBodyLimit::max(files::MAX_CHUNK_BYTES)),
        )
        .route("/api/upload/complete/{token}", post(files::upload_complete))
        .route("/trash", get(trash::index))
        .route("/trash/restore", post(trash::restore))
        .route("/trash/delete", post(trash::delete_forever))
        .route("/trash/empty", post(trash::empty))
        .route("/shares", get(shares::index))
        .route("/shares/new", get(shares::new_dialog))
        .route("/shares/create", post(shares::create))
        .route("/shares/delete", post(shares::delete))
        .route("/s/{token}", get(shares::public_page))
        .route("/s/{token}/unlock", post(shares::unlock))
        .route("/s/{token}/file/{file_id}", get(shares::public_file_page))
        .route("/s/{token}/raw", get(shares::public_raw_root))
        .route("/s/{token}/raw/{file_id}", get(shares::public_raw))
        .route("/s/{token}/download", get(shares::public_download_root))
        .route(
            "/s/{token}/download/{file_id}",
            get(shares::public_download),
        )
        .route("/admin", get(admin::index))
        .route("/admin/users/create", post(admin::create_user))
        .route("/admin/users/toggle", post(admin::toggle_user))
        .route("/admin/users/delete", post(admin::delete_user))
        .route("/admin/users/password", post(admin::reset_password))
        .route("/admin/users/quota", post(admin::set_quota))
        .route("/admin/settings", get(settings::index).post(settings::save))
        .route("/admin/update/check", get(settings::check_update))
        .route("/admin/update/apply", post(settings::apply_update))
        .route("/api/version", get(api_version))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), body_limits))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn render<T: Template>(
    template: &T,
    status: StatusCode,
) -> Result<Response, crate::error::AppError> {
    let html = template
        .render()
        .map_err(|e| crate::error::AppError::Internal(format!("模板渲染失败: {e}")))?;
    Ok((
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response())
}

pub fn redirect_msg(path: &str, key: &str, message: &str) -> Redirect {
    let sep = if path.contains('?') { '&' } else { '?' };
    Redirect::to(&format!(
        "{path}{sep}{key}={}",
        urlencoding::encode(message)
    ))
}

pub fn redirect_with_cookie(
    destination: &str,
    cookie: &str,
) -> Result<Response, crate::error::AppError> {
    let mut response = Redirect::to(destination).into_response();
    let value = HeaderValue::from_str(cookie)
        .map_err(|e| crate::error::AppError::Internal(format!("Cookie 无效: {e}")))?;
    response.headers_mut().insert(header::SET_COOKIE, value);
    Ok(response)
}

async fn security_headers(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let needs_no_store = {
        let headers = response.headers();
        !headers.contains_key(header::CACHE_CONTROL)
            && headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value.starts_with("text/html") || value.starts_with("application/json")
                })
    };
    let https = state.config().secure_cookies;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("SAMEORIGIN"),
    );
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self'; frame-src 'self'; frame-ancestors 'self'; base-uri 'none'; form-action 'self'; object-src 'none'",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if needs_no_store {
        // 页面与接口响应绝不允许被浏览器或中间缓存复用。
        // 除标准的 no-store 外，再补上 HTTP/1.0 时代的 Pragma/Expires，
        // 以及 Cloudflare 等 CDN 会优先识别的 CDN-Cache-Control，
        // 覆盖那些会忽略 no-store 的旧浏览器、企业代理与 CDN。
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, max-age=0, must-revalidate"),
        );
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        headers.insert(
            header::EXPIRES,
            HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"),
        );
        headers.insert(
            header::HeaderName::from_static("cdn-cache-control"),
            HeaderValue::from_static("no-store"),
        );
    }
    if https {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}

async fn root() -> Redirect {
    Redirect::to("/files")
}

async fn health(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(err) => {
            tracing::error!("健康检查失败: {err}");
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response()
        }
    }
}

/// 版本匹配：按内容哈希长期缓存。版本不匹配（旧页面/旧缓存引用）：跳回当前地址。
async fn asset_versioned(
    Path((version, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    match assets::lookup(&file) {
        Some(asset) => assets::serve(asset, &version, &headers),
        None => not_found().await,
    }
}

async fn asset_font_versioned(
    Path((version, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    match assets::lookup_font(&file) {
        Some(asset) => assets::serve(asset, &version, &headers),
        None => not_found().await,
    }
}

// 升级前缓存的旧 HTML 只会引用固定地址；这里始终跳转到带内容哈希的当前版本，
// 跳转响应本身 no-store，避免中间缓存把这个地址也缓存住。
async fn asset_legacy(Path(file): Path<String>) -> Response {
    match assets::lookup(&file) {
        Some(asset) => assets::redirect_to(asset.url()),
        None => not_found().await,
    }
}

async fn asset_font_legacy(Path(file): Path<String>) -> Response {
    match assets::lookup_font(&file) {
        Some(asset) => assets::redirect_to(asset.url()),
        None => not_found().await,
    }
}

async fn api_version(_admin: AdminUser) -> Response {
    axum::Json(serde_json::json!({
        "version": crate::cli::VERSION,
        "web_update": crate::update::web_update_supported(),
    }))
    .into_response()
}

async fn not_found() -> Response {
    crate::error::AppError::NotFound("页面不存在".to_string()).into_response()
}
