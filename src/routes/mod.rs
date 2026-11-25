pub mod admin;
pub mod auth_routes;
pub mod files;
pub mod shares;
pub mod trash;

use crate::state::AppState;
use askama::Template;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

const APP_CSS: &str = include_str!("../../static/app.css");
const APP_JS: &str = include_str!("../../static/app.js");
const HTMX_JS: &str = include_str!("../../static/htmx.min.js");
const FAVICON: &str = include_str!("../../static/favicon.svg");
const FONT_UI_400: &[u8] =
    include_bytes!("../../static/fonts/space-grotesk-latin-400-normal.woff2");
const FONT_UI_500: &[u8] =
    include_bytes!("../../static/fonts/space-grotesk-latin-500-normal.woff2");
const FONT_UI_600: &[u8] =
    include_bytes!("../../static/fonts/space-grotesk-latin-600-normal.woff2");
const FONT_UI_700: &[u8] =
    include_bytes!("../../static/fonts/space-grotesk-latin-700-normal.woff2");
const FONT_MONO_400: &[u8] =
    include_bytes!("../../static/fonts/ibm-plex-mono-latin-400-normal.woff2");
const FONT_MONO_500: &[u8] =
    include_bytes!("../../static/fonts/ibm-plex-mono-latin-500-normal.woff2");

pub fn router(state: AppState) -> Router {
    let max_body = state.config.max_upload_bytes() as usize;
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(health))
        .route("/static/app.css", get(asset_css))
        .route("/static/app.js", get(asset_js))
        .route("/static/htmx.min.js", get(asset_htmx))
        .route("/static/favicon.svg", get(asset_favicon))
        .route(
            "/static/fonts/space-grotesk-latin-400-normal.woff2",
            get(asset_font_ui_400),
        )
        .route(
            "/static/fonts/space-grotesk-latin-500-normal.woff2",
            get(asset_font_ui_500),
        )
        .route(
            "/static/fonts/space-grotesk-latin-600-normal.woff2",
            get(asset_font_ui_600),
        )
        .route(
            "/static/fonts/space-grotesk-latin-700-normal.woff2",
            get(asset_font_ui_700),
        )
        .route(
            "/static/fonts/ibm-plex-mono-latin-400-normal.woff2",
            get(asset_font_mono_400),
        )
        .route(
            "/static/fonts/ibm-plex-mono-latin-500-normal.woff2",
            get(asset_font_mono_500),
        )
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
            post(files::upload_chunk).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
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
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(max_body))
        .layer(middleware::from_fn(security_headers))
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

async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
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
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self'; frame-src 'self'; frame-ancestors 'self'; base-uri 'none'; form-action 'self'; object-src 'none'",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
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
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
        }
    }
}

async fn asset_css() -> Response {
    let mut response = APP_CSS.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    response
}

async fn asset_js() -> Response {
    let mut response = APP_JS.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    response
}

async fn asset_htmx() -> Response {
    let mut response = HTMX_JS.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    response
}

fn font_response(bytes: &'static [u8]) -> Response {
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("font/woff2"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

async fn asset_font_ui_400() -> Response {
    font_response(FONT_UI_400)
}

async fn asset_font_ui_500() -> Response {
    font_response(FONT_UI_500)
}

async fn asset_font_ui_600() -> Response {
    font_response(FONT_UI_600)
}

async fn asset_font_ui_700() -> Response {
    font_response(FONT_UI_700)
}

async fn asset_font_mono_400() -> Response {
    font_response(FONT_MONO_400)
}

async fn asset_font_mono_500() -> Response {
    font_response(FONT_MONO_500)
}

async fn asset_favicon() -> Response {
    let mut response = FAVICON.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
}

async fn not_found() -> Response {
    crate::error::AppError::NotFound("页面不存在".to_string()).into_response()
}
