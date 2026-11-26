//! 静态资源清单与响应构造。
//!
//! 资源地址由 build.rs 生成：`/static/v/<内容哈希>/…`。URL 里带内容哈希，
//! 所以可以放心使用一年期的 `immutable` 缓存；而页面 HTML 始终 `no-store`，
//! 新版本页面必然引用新的资源地址，旧缓存不会再被使用。
//!
//! 旧的固定地址（`/static/app.css` 等）保留为临时跳转，避免升级前缓存的旧 HTML
//! 拿到 404，也让残留在中间缓存里的旧页面能自动回到当前版本。

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

include!(concat!(env!("OUT_DIR"), "/asset_urls.rs"));

/// 模板里使用的资源地址。字段都是 `&'static str`，可以直接输出到 HTML。
#[derive(Clone, Copy, Debug)]
pub struct Assets {
    pub version: &'static str,
    pub app_css: &'static str,
    pub app_js: &'static str,
    pub theme_js: &'static str,
    pub htmx_js: &'static str,
    pub alpine_js: &'static str,
    pub favicon: &'static str,
}

impl Assets {
    pub const fn new() -> Self {
        Self {
            version: ASSET_VERSION,
            app_css: APP_CSS_URL,
            app_js: APP_JS_URL,
            theme_js: THEME_JS_URL,
            htmx_js: HTMX_JS_URL,
            alpine_js: ALPINE_JS_URL,
            favicon: FAVICON_URL,
        }
    }
}

impl Default for Assets {
    fn default() -> Self {
        Self::new()
    }
}

/// 带内容哈希的地址可以缓存一年，且不再回源校验。
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// 跳过缓存必须回源的响应（旧地址的临时跳转）。
const NO_STORE: &str = "no-store";

const APP_CSS_BODY: &[u8] = include_bytes!("../static/app.css");
const APP_JS_BODY: &[u8] = include_bytes!("../static/app.js");
const THEME_JS_BODY: &[u8] = include_bytes!("../static/theme.js");
const HTMX_JS_BODY: &[u8] = include_bytes!("../static/htmx.min.js");
const ALPINE_JS_BODY: &[u8] = include_bytes!("../static/alpine.min.js");
const FAVICON_BODY: &[u8] = include_bytes!("../static/favicon.svg");
const FONT_UI_400_BODY: &[u8] =
    include_bytes!("../static/fonts/space-grotesk-latin-400-normal.woff2");
const FONT_UI_500_BODY: &[u8] =
    include_bytes!("../static/fonts/space-grotesk-latin-500-normal.woff2");
const FONT_UI_600_BODY: &[u8] =
    include_bytes!("../static/fonts/space-grotesk-latin-600-normal.woff2");
const FONT_UI_700_BODY: &[u8] =
    include_bytes!("../static/fonts/space-grotesk-latin-700-normal.woff2");
const FONT_MONO_400_BODY: &[u8] =
    include_bytes!("../static/fonts/ibm-plex-mono-latin-400-normal.woff2");
const FONT_MONO_500_BODY: &[u8] =
    include_bytes!("../static/fonts/ibm-plex-mono-latin-500-normal.woff2");

const CSS_TYPE: &str = "text/css; charset=utf-8";
const JS_TYPE: &str = "application/javascript; charset=utf-8";
const SVG_TYPE: &str = "image/svg+xml";
const FONT_TYPE: &str = "font/woff2";

/// 一个可对外提供的静态资源。
#[derive(Clone, Copy)]
pub struct Asset {
    url: &'static str,
    body: &'static [u8],
    content_type: &'static str,
    etag: &'static str,
}

impl Asset {
    const fn new(
        url: &'static str,
        body: &'static [u8],
        content_type: &'static str,
        etag: &'static str,
    ) -> Self {
        Self {
            url,
            body,
            content_type,
            etag,
        }
    }

    /// 当前版本的访问地址。
    pub const fn url(self) -> &'static str {
        self.url
    }
}

/// 站点根目录下的静态资源（按文件名查表）。
pub fn lookup(name: &str) -> Option<Asset> {
    Some(match name {
        "app.css" => Asset::new(APP_CSS_URL, APP_CSS_BODY, CSS_TYPE, APP_CSS_ETAG),
        "app.js" => Asset::new(APP_JS_URL, APP_JS_BODY, JS_TYPE, APP_JS_ETAG),
        "theme.js" => Asset::new(THEME_JS_URL, THEME_JS_BODY, JS_TYPE, THEME_JS_ETAG),
        "htmx.min.js" => Asset::new(HTMX_JS_URL, HTMX_JS_BODY, JS_TYPE, HTMX_JS_ETAG),
        "alpine.min.js" => Asset::new(ALPINE_JS_URL, ALPINE_JS_BODY, JS_TYPE, ALPINE_JS_ETAG),
        "favicon.svg" => Asset::new(FAVICON_URL, FAVICON_BODY, SVG_TYPE, FAVICON_ETAG),
        _ => return None,
    })
}

/// 字体目录下的静态资源（按文件名查表）。
pub fn lookup_font(name: &str) -> Option<Asset> {
    Some(match name {
        "space-grotesk-latin-400-normal.woff2" => Asset::new(
            FONT_UI_400_URL,
            FONT_UI_400_BODY,
            FONT_TYPE,
            FONT_UI_400_ETAG,
        ),
        "space-grotesk-latin-500-normal.woff2" => Asset::new(
            FONT_UI_500_URL,
            FONT_UI_500_BODY,
            FONT_TYPE,
            FONT_UI_500_ETAG,
        ),
        "space-grotesk-latin-600-normal.woff2" => Asset::new(
            FONT_UI_600_URL,
            FONT_UI_600_BODY,
            FONT_TYPE,
            FONT_UI_600_ETAG,
        ),
        "space-grotesk-latin-700-normal.woff2" => Asset::new(
            FONT_UI_700_URL,
            FONT_UI_700_BODY,
            FONT_TYPE,
            FONT_UI_700_ETAG,
        ),
        "ibm-plex-mono-latin-400-normal.woff2" => Asset::new(
            FONT_MONO_400_URL,
            FONT_MONO_400_BODY,
            FONT_TYPE,
            FONT_MONO_400_ETAG,
        ),
        "ibm-plex-mono-latin-500-normal.woff2" => Asset::new(
            FONT_MONO_500_URL,
            FONT_MONO_500_BODY,
            FONT_TYPE,
            FONT_MONO_500_ETAG,
        ),
        _ => return None,
    })
}

fn etag_matches(candidates: &str, etag: &str) -> bool {
    candidates.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

fn cacheable(body: &'static [u8], content_type: &'static str, etag: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::ETAG, HeaderValue::from_static(etag));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
    response
}

/// 生成资源响应：版本匹配时按内容哈希长期缓存，版本不匹配则跳到当前地址。
pub fn serve(asset: Asset, requested_version: &str, request_headers: &HeaderMap) -> Response {
    if requested_version != ASSET_VERSION {
        // 旧页面或残留缓存引用了以前的版本：跳到当前版本，避免新旧混用。
        return redirect_to(asset.url);
    }
    if request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| etag_matches(value, asset.etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        let headers = response.headers_mut();
        headers.insert(header::ETAG, HeaderValue::from_static(asset.etag));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
        return response;
    }
    cacheable(asset.body, asset.content_type, asset.etag)
}

/// 旧固定地址 → 当前带哈希地址。跳转本身禁止缓存，保证每次都能回到最新版本。
pub fn redirect_to(url: &'static str) -> Response {
    let mut response = Redirect::temporary(url).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    response
}
