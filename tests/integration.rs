use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cloudnest::config::Config;
use cloudnest::state::AppState;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

struct TestApp {
    app: Router,
    state: AppState,
    _dir: tempfile::TempDir,
}

async fn test_app() -> TestApp {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let cfg = Config {
        data_dir: dir.path().join("data"),
        listen: String::from("127.0.0.1:0"),
        max_upload_size_mb: 64,
        default_quota_gb: 1,
        ..Config::default()
    };
    let config_path = dir.path().join("config.toml");
    cfg.save(&config_path).expect("write config file");
    let state = AppState::with_config_file(cfg, config_path)
        .await
        .expect("app state");
    let app = cloudnest::routes::router(state.clone());
    TestApp {
        app,
        state,
        _dir: dir,
    }
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Body,
    content_type: Option<&str>,
) -> Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    let request = builder.body(body).expect("request");
    app.clone().oneshot(request).await.expect("response")
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn encode_pairs(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&urlencoding::encode(key));
        out.push('=');
        out.push_str(&urlencoding::encode(value));
    }
    out
}

fn form_body(pairs: &[(&str, &str)]) -> Body {
    Body::from(encode_pairs(pairs))
}

fn extract_csrf(html: &str) -> String {
    let marker = "name=\"csrf\" value=\"";
    let start = html.find(marker).expect("csrf field") + marker.len();
    let rest = &html[start..];
    let end = rest.find('"').expect("csrf end");
    rest[..end].to_string()
}

fn extract_cookie(response: &Response) -> String {
    let raw = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .expect("cookie utf8");
    raw.split(';').next().unwrap_or(raw).to_string()
}

async fn setup_admin(t: &TestApp) -> (String, String) {
    let response = send(&t.app, Method::GET, "/setup", None, Body::empty(), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(
        &t.app,
        Method::POST,
        "/setup",
        None,
        form_body(&[
            ("username", "admin"),
            ("password", "admin-password-123"),
            ("password_confirm", "admin-password-123"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = extract_cookie(&response);
    let response = send(
        &t.app,
        Method::GET,
        "/files",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    let csrf = extract_csrf(&html);
    (cookie, csrf)
}

async fn login(t: &TestApp, username: &str, password: &str) -> String {
    let response = send(
        &t.app,
        Method::POST,
        "/login",
        None,
        form_body(&[("username", username), ("password", password)]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    extract_cookie(&response)
}

fn multipart_upload(
    csrf: &str,
    parent_id: i64,
    path: &str,
    filename: &str,
    content: &[u8],
) -> (String, Body) {
    const BOUNDARY: &str = "----cloudnesttestboundary";
    let mut body = Vec::new();
    for (name, value) in [
        ("csrf", csrf.to_string()),
        ("parent_id", parent_id.to_string()),
        ("path", path.to_string()),
    ] {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"files\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (
        format!("multipart/form-data; boundary={BOUNDARY}"),
        Body::from(body),
    )
}

async fn create_folder(t: &TestApp, cookie: &str, csrf: &str, parent: i64, name: &str) -> i64 {
    let response = send(
        &t.app,
        Method::POST,
        "/files/create-folder",
        Some(cookie),
        form_body(&[
            ("csrf", csrf),
            ("parent_id", &parent.to_string()),
            ("name", name),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    sqlx::query_scalar("SELECT id FROM files WHERE name = ? AND owner_id = 1")
        .bind(name)
        .fetch_one(&t.state.pool)
        .await
        .expect("folder id")
}

async fn upload(
    t: &TestApp,
    cookie: &str,
    csrf: &str,
    parent: i64,
    path: &str,
    name: &str,
    content: &[u8],
) -> i64 {
    let (content_type, body) = multipart_upload(csrf, parent, path, name, content);
    let response = send(
        &t.app,
        Method::POST,
        "/files/upload",
        Some(cookie),
        body,
        Some(&content_type),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    sqlx::query_scalar("SELECT id FROM files WHERE name = ? ORDER BY id DESC LIMIT 1")
        .bind(name)
        .fetch_one(&t.state.pool)
        .await
        .expect("file id")
}

#[tokio::test]
async fn setup_login_and_file_lifecycle() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;

    let folder = create_folder(&t, &cookie, &csrf, 0, "docs").await;
    let file_id = upload(
        &t,
        &cookie,
        &csrf,
        folder,
        "",
        "hello.txt",
        b"hello cloudnest",
    )
    .await;

    let response = send(
        &t.app,
        Method::GET,
        &format!("/files/{file_id}/download"),
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], b"hello cloudnest");

    // Range requests
    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/files/{file_id}/download"))
                .header(header::COOKIE, &cookie)
                .header(header::RANGE, "bytes=6-14")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_RANGE)
            .unwrap()
            .to_str()
            .unwrap(),
        "bytes 6-14/15"
    );

    let response = send(
        &t.app,
        Method::GET,
        &format!("/files/{file_id}/preview"),
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("hello cloudnest"));

    let response = send(
        &t.app,
        Method::GET,
        "/files?q=hello",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    let html = body_text(response).await;
    assert!(html.contains("hello.txt"));

    let response = send(
        &t.app,
        Method::POST,
        "/files/rename",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("id", &file_id.to_string()),
            ("name", "renamed.txt"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let response = send(
        &t.app,
        Method::POST,
        "/files/delete",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("id", &file_id.to_string()),
            ("return_path", "0"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let response = send(
        &t.app,
        Method::GET,
        "/trash",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    let html = body_text(response).await;
    assert!(html.contains("renamed.txt"));

    let response = send(
        &t.app,
        Method::POST,
        "/trash/restore",
        Some(&cookie),
        form_body(&[("csrf", &csrf), ("id", &file_id.to_string())]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let response = send(
        &t.app,
        Method::GET,
        &format!("/files/{file_id}/download"),
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Delete again, then permanently remove it.
    let response = send(
        &t.app,
        Method::POST,
        "/files/delete",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("id", &file_id.to_string()),
            ("return_path", "0"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = send(
        &t.app,
        Method::POST,
        "/trash/delete",
        Some(&cookie),
        form_body(&[("csrf", &csrf), ("id", &file_id.to_string())]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE id = ?")
        .bind(file_id)
        .fetch_one(&t.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    let response = send(
        &t.app,
        Method::POST,
        "/trash/empty",
        Some(&cookie),
        form_body(&[("csrf", &csrf)]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn resumable_upload_flow() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let payload = b"resumable content";
    let response = send(
        &t.app,
        Method::POST,
        "/api/upload/init",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("parent_id", "0"),
            ("path", ""),
            ("name", "resume.txt"),
            ("size", &payload.len().to_string()),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    let token = json["token"].as_str().unwrap().to_string();

    let response = send(
        &t.app,
        Method::POST,
        &format!("/api/upload/chunk/{token}"),
        Some(&cookie),
        Body::from(payload[..8].to_vec()),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "CSRF header required"
    );

    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/upload/chunk/{token}"))
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", &csrf)
                .header("x-offset", "0")
                .body(Body::from(payload[..8].to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(
        &t.app,
        Method::GET,
        &format!("/api/upload/{token}"),
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["received"].as_i64().unwrap(), 8);

    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/upload/chunk/{token}"))
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", &csrf)
                .header("x-offset", "8")
                .body(Body::from(payload[8..].to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/upload/complete/{token}"))
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", &csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let file_id: i64 = sqlx::query_scalar("SELECT id FROM files WHERE name = 'resume.txt'")
        .fetch_one(&t.state.pool)
        .await
        .unwrap();
    let response = send(
        &t.app,
        Method::GET,
        &format!("/files/{file_id}/download"),
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], payload);
}

#[tokio::test]
async fn chunked_upload_above_cloudflare_body_limit() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;

    // Cloudflare 免费版限制单个请求体 100 MB；这里上传超过 100 MiB 的文件，
    // 确认前端分片通道使用的接口不依赖单个大请求，且拼接后的内容完整。
    const CHUNK: usize = 4 * 1024 * 1024;
    let total: usize = 101 * 1024 * 1024 + 12345;

    let response = send(
        &t.app,
        Method::POST,
        "/api/upload/init",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("parent_id", "0"),
            ("path", ""),
            ("name", "cloudflare-big.bin"),
            ("size", &total.to_string()),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    let token = json["token"].as_str().unwrap().to_string();

    let mut hasher = Sha256::new();
    let mut offset = 0usize;
    while offset < total {
        let len = CHUNK.min(total - offset);
        let mut chunk = vec![0u8; len];
        // 每片写入当前偏移，既能验证顺序，也避免整块重复数据掩盖错位。
        chunk[..8].copy_from_slice(&(offset as u64).to_le_bytes());
        hasher.update(&chunk);

        let response = t
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/upload/chunk/{token}"))
                    .header(header::COOKIE, &cookie)
                    .header("x-csrf-token", &csrf)
                    .header("x-offset", offset.to_string())
                    .body(Body::from(chunk))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        offset += len;
    }
    let expected_hash = hex::encode(hasher.finalize());

    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/upload/complete/{token}"))
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", &csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (size, storage_key): (i64, String) =
        sqlx::query_as("SELECT size, storage_key FROM files WHERE name = 'cloudflare-big.bin'")
            .fetch_one(&t.state.pool)
            .await
            .unwrap();
    assert_eq!(size, total as i64);
    let stored = tokio::fs::read(t.state.stored_file_path(1, &storage_key))
        .await
        .unwrap();
    assert_eq!(stored.len(), total);
    assert_eq!(hex::encode(Sha256::digest(&stored)), expected_hash);
}

#[tokio::test]
async fn user_isolation_and_admin_controls() {
    let t = test_app().await;
    let (admin_cookie, csrf) = setup_admin(&t).await;
    let response = send(
        &t.app,
        Method::POST,
        "/admin/users/create",
        Some(&admin_cookie),
        form_body(&[
            ("csrf", &csrf),
            ("username", "bob"),
            ("password", "bob-password-123"),
            ("quota_gb", "1"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let bob_cookie = login(&t, "bob", "bob-password-123").await;
    let response = send(
        &t.app,
        Method::GET,
        "/files",
        Some(&bob_cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bob_html = body_text(response).await;
    let bob_csrf = extract_csrf(&bob_html);
    let bob_file = upload(&t, &bob_cookie, &bob_csrf, 0, "", "bob.txt", b"bob secret").await;
    let bob_owner: i64 = sqlx::query_scalar("SELECT owner_id FROM files WHERE id = ?")
        .bind(bob_file)
        .fetch_one(&t.state.pool)
        .await
        .unwrap();
    let bob_user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'bob'")
        .fetch_one(&t.state.pool)
        .await
        .unwrap();
    assert_eq!(bob_owner, bob_user_id);
    let response = send(
        &t.app,
        Method::GET,
        &format!("/files/{bob_file}/download"),
        Some(&admin_cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = send(
        &t.app,
        Method::POST,
        "/admin/users/toggle",
        Some(&admin_cookie),
        form_body(&[("csrf", &csrf), ("id", "2")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = send(
        &t.app,
        Method::GET,
        "/files",
        Some(&bob_cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");

    let response = send(
        &t.app,
        Method::POST,
        "/admin/users/delete",
        Some(&admin_cookie),
        form_body(&[("csrf", &csrf), ("id", "2")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = 'bob'")
        .fetch_one(&t.state.pool)
        .await
        .unwrap();
    assert_eq!(user_count, 0);
}

#[tokio::test]
async fn password_share_and_download_limits() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let file_id = upload(&t, &cookie, &csrf, 0, "", "shared.txt", b"shared content").await;
    let response = send(
        &t.app,
        Method::POST,
        "/shares/create",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("file_id", &file_id.to_string()),
            ("password", "share-pass-123"),
            ("max_downloads", "1"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let token: String = sqlx::query_scalar("SELECT token FROM shares WHERE file_id = ?")
        .bind(file_id)
        .fetch_one(&t.state.pool)
        .await
        .unwrap();

    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("提取密码"));

    // 未输入提取密码时，直链下载与原始内容都必须被拒绝（不能绕过密码）。
    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}/download"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}/raw"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        &t.app,
        Method::POST,
        &format!("/s/{token}/unlock"),
        None,
        form_body(&[("password", "wrong")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = send(
        &t.app,
        Method::POST,
        &format!("/s/{token}/unlock"),
        None,
        form_body(&[("password", "share-pass-123")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let share_cookie = extract_cookie(&response);

    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}/download"),
        Some(&share_cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], b"shared content");

    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}/download"),
        Some(&share_cookie),
        Body::empty(),
        None,
    )
    .await;
    assert!(
        response.status() == StatusCode::NOT_FOUND || response.status() == StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn security_rejects_traversal_and_missing_csrf() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let response = send(&t.app, Method::GET, "/files", None, Body::empty(), None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");

    let (content_type, body) = multipart_upload("", 0, "", "no.txt", b"x");
    let response = send(
        &t.app,
        Method::POST,
        "/files/upload",
        Some(&cookie),
        body,
        Some(&content_type),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let (content_type, body) = multipart_upload(&csrf, 0, "../../etc", "evil.txt", b"x");
    let response = send(
        &t.app,
        Method::POST,
        "/files/upload",
        Some(&cookie),
        body,
        Some(&content_type),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let (content_type, body) = multipart_upload(&csrf, 0, "", "../evil.txt", b"x");
    let response = send(
        &t.app,
        Method::POST,
        "/files/upload",
        Some(&cookie),
        body,
        Some(&content_type),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let name: String =
        sqlx::query_scalar("SELECT name FROM files WHERE owner_id = 1 ORDER BY id DESC LIMIT 1")
            .fetch_one(&t.state.pool)
            .await
            .unwrap();
    assert_eq!(name, "evil.txt");
}

#[tokio::test]
async fn login_errors_are_generic() {
    let t = test_app().await;
    let (_, _) = setup_admin(&t).await;
    let response = send(
        &t.app,
        Method::POST,
        "/login",
        None,
        form_body(&[("username", "admin"), ("password", "wrong-password")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let html = body_text(response).await;
    assert!(html.contains("用户名或密码错误"));
    assert!(!html.contains("argon2"));
}

#[tokio::test]
async fn quota_is_enforced() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    sqlx::query("UPDATE users SET quota_bytes = 10 WHERE id = 1")
        .execute(&t.state.pool)
        .await
        .unwrap();
    let (content_type, body) =
        multipart_upload(&csrf, 0, "", "too-big.txt", b"01234567890123456789");
    let response = send(
        &t.app,
        Method::POST,
        "/files/upload",
        Some(&cookie),
        body,
        Some(&content_type),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let (content_type, body) = multipart_upload(&csrf, 0, "", "fits.txt", b"12345");
    let response = send(
        &t.app,
        Method::POST,
        "/files/upload",
        Some(&cookie),
        body,
        Some(&content_type),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}

// ---- 网页设置与在线更新 ----

type FakeRoutes = Vec<(String, u16, Vec<u8>)>;

async fn fake_server<F>(build: F) -> String
where
    F: FnOnce(&str) -> FakeRoutes,
{
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind fake server");
    let addr = listener.local_addr().expect("fake server addr");
    let base = format!("http://{addr}");
    let routes: HashMap<String, (u16, Vec<u8>)> = build(&base)
        .into_iter()
        .map(|(path, status, body)| (path, (status, body)))
        .collect();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    match socket.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(n) => {
                            request.extend_from_slice(&buffer[..n]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let first_line = String::from_utf8_lossy(&request);
                let path = first_line
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let (status, body) = routes
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| (404, b"missing".to_vec()));
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut payload = response.into_bytes();
                payload.extend_from_slice(&body);
                let _ = socket.write_all(&payload).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    base
}

fn release_json(tag: &str, base: &str) -> Vec<u8> {
    let asset = cloudnest::update::release_asset_name().expect("asset name");
    serde_json::to_vec(&json!({
        "tag_name": tag,
        "assets": [
            { "name": asset, "browser_download_url": format!("{base}/asset") },
            { "name": "SHA256SUMS", "browser_download_url": format!("{base}/SHA256SUMS") }
        ]
    }))
    .expect("release json")
}

fn tar_gz_with_binary(binary: &[u8]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "cloudnest", binary)
            .expect("append tar entry");
        builder.finish().expect("finish tar");
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).expect("gzip write");
    encoder.finish().expect("gzip finish")
}

fn settings_form<'a>(
    csrf: &'a str,
    listen: &'a str,
    public_url: &'a str,
    upload_mb: &'a str,
    quota_gb: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("csrf", csrf),
        ("listen", listen),
        ("public_url", public_url),
        ("secure_cookies", "on"),
        ("max_upload_size_mb", upload_mb),
        ("default_quota_gb", quota_gb),
        ("session_days", "14"),
    ]
}

#[tokio::test]
async fn settings_page_hot_reloads_and_validates() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;

    let response = send(
        &t.app,
        Method::GET,
        "/admin/settings",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("站点设置"), "settings page title");
    assert!(html.contains("版本更新"), "update panel");
    assert!(html.contains(cloudnest::cli::VERSION), "current version");

    // 非法地址必须被拒绝，且不能写坏配置文件。
    let response = send(
        &t.app,
        Method::POST,
        "/admin/settings",
        Some(&cookie),
        form_body(&settings_form(
            &csrf,
            "127.0.0.1:19999",
            "ftp://invalid.example",
            "1",
            "7",
        )),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let file = std::fs::read_to_string(t.state.config_path().expect("config path")).unwrap();
    assert!(!file.contains("ftp://invalid.example"));

    // 合法保存：写入文件并立即热更新。
    let response = send(
        &t.app,
        Method::POST,
        "/admin/settings",
        Some(&cookie),
        form_body(&settings_form(
            &csrf,
            "127.0.0.1:19999",
            "https://cloud.example.com",
            "1",
            "7",
        )),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let file = std::fs::read_to_string(t.state.config_path().expect("config path")).unwrap();
    assert!(
        file.contains("listen = \"127.0.0.1:19999\""),
        "listen saved: {file}"
    );
    assert!(file.contains("default_quota_gb = 7"), "quota saved: {file}");
    assert!(
        file.contains("max_upload_size_mb = 1"),
        "upload limit saved"
    );
    assert!(file.contains("https://cloud.example.com"));

    let running = t.state.config();
    assert_eq!(running.default_quota_gb, 7, "quota hot reloaded");
    assert_eq!(running.max_upload_size_mb, 1, "upload limit hot reloaded");
    assert_eq!(running.session_days, 14);
    assert_eq!(
        running.public_url.as_deref(),
        Some("https://cloud.example.com")
    );
    assert!(
        running.secure_cookies,
        "https public url forces secure cookies"
    );
    assert_eq!(running.listen, "127.0.0.1:0", "listen needs a restart");

    // 热更新后的默认配额直接作用于新账户。
    let response = send(
        &t.app,
        Method::POST,
        "/admin/users/create",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("username", "settings-user"),
            ("password", "settings-user-pass-123"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let quota: i64 =
        sqlx::query_scalar("SELECT quota_bytes FROM users WHERE username = 'settings-user'")
            .fetch_one(&t.state.pool)
            .await
            .unwrap();
    assert_eq!(
        quota,
        7 * 1024 * 1024 * 1024,
        "new user uses reloaded default quota"
    );

    // 上传上限热更新后，请求体限制也立即收紧（这里用表单解析器验证 413）。
    let big = "x".repeat(1_100_000);
    let response = send(
        &t.app,
        Method::POST,
        "/account/password",
        Some(&cookie),
        form_body(&[("csrf", &csrf), ("new_password", &big)]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let response = send(
        &t.app,
        Method::GET,
        "/admin/settings",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("需重启服务后生效"), "restart hint shown");
}

#[tokio::test]
async fn update_check_and_staging_via_local_api() {
    let binary = b"#!/bin/sh\necho cloudnest-new\n".to_vec();
    let archive = tar_gz_with_binary(&binary);
    let checksum = hex::encode(Sha256::digest(&archive));
    let base = fake_server(|base| {
        vec![
            (
                "/repos/katriyda/cloudnest/releases/latest".to_string(),
                200,
                release_json("v9.9.9", base),
            ),
            ("/asset".to_string(), 200, archive.clone()),
            (
                "/SHA256SUMS".to_string(),
                200,
                format!(
                    "{checksum}  {}\n",
                    cloudnest::update::release_asset_name().unwrap()
                )
                .into_bytes(),
            ),
        ]
    })
    .await;

    let t = test_app().await;
    t.state.updates.set_api_base(base);
    let (cookie, _csrf) = setup_admin(&t).await;

    // 版本接口只对管理员开放，避免向匿名访问者泄露版本信息。
    let response = send(
        &t.app,
        Method::GET,
        "/api/version",
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = send(
        &t.app,
        Method::GET,
        "/api/version",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["version"], cloudnest::cli::VERSION);

    let response = send(
        &t.app,
        Method::GET,
        "/admin/update/check",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["latest"], "v9.9.9");
    assert_eq!(json["has_update"], true);
    assert_eq!(json["supported"], cfg!(target_os = "linux"));

    let work_dir = t._dir.path().join("stage");
    let service = t.state.updates.clone();
    let staged = tokio::task::spawn_blocking(move || service.stage_latest(&work_dir))
        .await
        .expect("stage task")
        .expect("stage release");
    assert_eq!(staged.version, "v9.9.9");
    assert_eq!(
        std::fs::read(&staged.path).expect("read staged binary"),
        binary
    );
}

#[tokio::test]
async fn update_staging_rejects_bad_checksum() {
    let archive = tar_gz_with_binary(b"not the real payload");
    let base = fake_server(move |base| {
        vec![
            (
                "/repos/katriyda/cloudnest/releases/latest".to_string(),
                200,
                release_json("v9.9.9", base),
            ),
            ("/asset".to_string(), 200, archive.clone()),
            (
                "/SHA256SUMS".to_string(),
                200,
                format!(
                    "0000  {}\n",
                    cloudnest::update::release_asset_name().unwrap()
                )
                .into_bytes(),
            ),
        ]
    })
    .await;
    let t = test_app().await;
    t.state.updates.set_api_base(base);
    let service = t.state.updates.clone();
    let work_dir = t._dir.path().join("stage-bad");
    let stage_path = work_dir.clone();
    let error = tokio::task::spawn_blocking(move || service.stage_latest(&stage_path))
        .await
        .expect("stage task")
        .expect_err("checksum must fail");
    assert!(error.0.contains("SHA256"), "unexpected error: {error}");
    assert!(!work_dir.join("cloudnest").exists());
}

#[tokio::test]
async fn web_update_apply_requires_csrf_and_never_touches_exe_on_error() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let archive = tar_gz_with_binary(b"payload");
    let base = fake_server(move |base| {
        vec![
            (
                "/repos/katriyda/cloudnest/releases/latest".to_string(),
                200,
                release_json("v9.9.9", base),
            ),
            ("/asset".to_string(), 200, archive.clone()),
            (
                "/SHA256SUMS".to_string(),
                200,
                format!(
                    "ffff  {}\n",
                    cloudnest::update::release_asset_name().unwrap()
                )
                .into_bytes(),
            ),
        ]
    })
    .await;
    t.state.updates.set_api_base(base);

    let response = send(
        &t.app,
        Method::POST,
        "/admin/update/apply",
        Some(&cookie),
        form_body(&[("csrf", "wrong-token")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["ok"], false);

    let response = send(
        &t.app,
        Method::POST,
        "/admin/update/apply",
        Some(&cookie),
        form_body(&[("csrf", &csrf)]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    let status = response.status();
    let json: Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["ok"], false);
    if cfg!(target_os = "linux") {
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(json["message"].as_str().unwrap_or("").contains("SHA256"));
    } else {
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["message"].as_str().unwrap_or("").contains("不支持"));
    }
}

#[tokio::test]
async fn settings_and_update_are_admin_only() {
    let t = test_app().await;
    let (_admin_cookie, admin_csrf) = setup_admin(&t).await;
    let response = send(
        &t.app,
        Method::POST,
        "/admin/users/create",
        Some(&_admin_cookie),
        form_body(&[
            ("csrf", &admin_csrf),
            ("username", "bob"),
            ("password", "bob-password-123"),
            ("quota_gb", "1"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = login(&t, "bob", "bob-password-123").await;

    let response = send(
        &t.app,
        Method::GET,
        "/admin/settings",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        &t.app,
        Method::GET,
        "/admin/update/check",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// ---- 安全回归测试（限流、限次、重定向、请求体、密码强度） ----

async fn create_share_for(
    t: &TestApp,
    cookie: &str,
    csrf: &str,
    file_id: i64,
    password: Option<&str>,
    max_downloads: Option<&str>,
) -> String {
    let file_id_text = file_id.to_string();
    let mut pairs: Vec<(&str, &str)> = vec![("csrf", csrf), ("file_id", &file_id_text)];
    if let Some(password) = password {
        pairs.push(("password", password));
    }
    if let Some(max_downloads) = max_downloads {
        pairs.push(("max_downloads", max_downloads));
    }
    let response = send(
        &t.app,
        Method::POST,
        "/shares/create",
        Some(cookie),
        form_body(&pairs),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER, "share created");
    sqlx::query_scalar("SELECT token FROM shares WHERE file_id = ? ORDER BY id DESC LIMIT 1")
        .bind(file_id)
        .fetch_one(&t.state.pool)
        .await
        .expect("share token")
}

#[tokio::test]
async fn login_is_rate_limited_after_repeated_failures() {
    let t = test_app().await;
    let (_cookie, _csrf) = setup_admin(&t).await;
    for _ in 0..10 {
        let response = send(
            &t.app,
            Method::POST,
            "/login",
            None,
            form_body(&[("username", "admin"), ("password", "wrong-password")]),
            Some("application/x-www-form-urlencoded"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = send(
        &t.app,
        Method::POST,
        "/login",
        None,
        form_body(&[("username", "admin"), ("password", "wrong-password")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().get(header::RETRY_AFTER).is_some(),
        "retry-after header"
    );

    // 其他用户名不受影响，避免一个账号拖垮所有人的登录。
    let response = send(
        &t.app,
        Method::POST,
        "/login",
        None,
        form_body(&[("username", "nobody"), ("password", "wrong-password")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_accounts_use_the_same_generic_error() {
    let t = test_app().await;
    let (_cookie, _csrf) = setup_admin(&t).await;
    let response = send(
        &t.app,
        Method::POST,
        "/login",
        None,
        form_body(&[("username", "ghost-user"), ("password", "wrong-password")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let html = body_text(response).await;
    assert!(html.contains("用户名或密码错误"));
    assert!(!html.contains("不存在"));
}

#[tokio::test]
async fn share_unlock_is_rate_limited() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let file_id = upload(&t, &cookie, &csrf, 0, "", "locked.txt", b"locked content").await;
    let token = create_share_for(&t, &cookie, &csrf, file_id, Some("share-pass-123"), None).await;

    for _ in 0..5 {
        let response = send(
            &t.app,
            Method::POST,
            &format!("/s/{token}/unlock"),
            None,
            form_body(&[("password", "wrong")]),
            Some("application/x-www-form-urlencoded"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = send(
        &t.app,
        Method::POST,
        &format!("/s/{token}/unlock"),
        None,
        form_body(&[("password", "wrong")]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn raw_content_counts_towards_download_limit() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let file_id = upload(
        &t,
        &cookie,
        &csrf,
        0,
        "",
        "limited.txt",
        b"limited secret content",
    )
    .await;
    let token = create_share_for(&t, &cookie, &csrf, file_id, None, Some("1")).await;

    // 限次分享页面不自动内联内容，避免预览绕过计数。
    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("限制了下载次数"), "limited notice shown");
    assert!(
        !html.contains("limited secret content"),
        "content must not be rendered for limited shares"
    );

    // raw 通道同样计入次数：第一次 200，第二次被拒。
    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}/raw"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], b"limited secret content");

    let response = send(
        &t.app,
        Method::GET,
        &format!("/s/{token}/raw"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert!(
        response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::NOT_FOUND,
        "second raw fetch blocked, got {}",
        response.status()
    );
    let count: i64 = sqlx::query_scalar("SELECT download_count FROM shares WHERE token = ?")
        .bind(&token)
        .fetch_one(&t.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "raw counts as one download");
}

#[tokio::test]
async fn share_password_minimum_length_is_enforced() {
    let t = test_app().await;
    let (cookie, csrf) = setup_admin(&t).await;
    let file_id = upload(&t, &cookie, &csrf, 0, "", "pw.txt", b"pw content").await;
    let response = send(
        &t.app,
        Method::POST,
        "/shares/create",
        Some(&cookie),
        form_body(&[
            ("csrf", &csrf),
            ("file_id", &file_id.to_string()),
            ("password", "a"),
        ]),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_next_never_redirects_offsite() {
    let t = test_app().await;
    let (_cookie, _csrf) = setup_admin(&t).await;
    for next in ["/\\evil.example", "//evil.example", "https://evil.example"] {
        let response = send(
            &t.app,
            Method::POST,
            "/login",
            None,
            form_body(&[
                ("username", "admin"),
                ("password", "admin-password-123"),
                ("next", next),
            ]),
            Some("application/x-www-form-urlencoded"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/files",
            "unsafe next {next} must fall back"
        );
    }
}

#[tokio::test]
async fn oversized_form_body_is_rejected() {
    let t = test_app().await;
    let (_cookie, _csrf) = setup_admin(&t).await;
    let body = format!("username=admin&password={}", "x".repeat(2_000_000));
    let response = send(
        &t.app,
        Method::POST,
        "/login",
        None,
        Body::from(body),
        Some("application/x-www-form-urlencoded"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// ---- 前端资源版本化与缓存策略回归测试 ----

fn header_text(response: &Response, name: header::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn pages_reference_versioned_assets_and_forbid_caching() {
    let t = test_app().await;
    let version = cloudnest::assets::ASSET_VERSION;

    let response = send(&t.app, Method::GET, "/setup", None, Body::empty(), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cache_control = header_text(&response, header::CACHE_CONTROL);
    let pragma = header_text(&response, header::PRAGMA);
    let expires = header_text(&response, header::EXPIRES);
    let cdn = header_text(
        &response,
        header::HeaderName::from_static("cdn-cache-control"),
    );
    let html = body_text(response).await;

    // 页面必须始终回源，否则旧 HTML 会一直引用旧资源地址。
    assert!(
        cache_control.contains("no-store"),
        "cache-control: {cache_control}"
    );
    assert!(
        cache_control.contains("no-cache"),
        "cache-control: {cache_control}"
    );
    assert_eq!(pragma, "no-cache");
    assert_eq!(expires, "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(cdn, "no-store");

    // 页面里的资源地址带内容哈希，升级后不会再命中旧 URL。
    let css = format!("/static/v/{version}/app.css");
    assert!(html.contains(&css), "versioned css url missing");
    assert!(html.contains(&format!("/static/v/{version}/app.js")));
    assert!(
        !html.contains("href=\"/static/app.css\""),
        "fixed css url must not be referenced anymore"
    );
    assert!(
        html.contains(&format!("name=\"cloudnest-assets\" content=\"{version}\"")),
        "asset version meta missing"
    );
}

// ---- 前端交互相关的服务端约定回归测试 ----

#[tokio::test]
async fn account_page_shows_flash_and_keeps_search_query() {
    let t = test_app().await;
    let (cookie, _csrf) = setup_admin(&t).await;

    // 修改密码后会跳回 /account?ok=…，页面必须把提示渲染出来（否则用户以为没生效）。
    let response = send(
        &t.app,
        Method::GET,
        "/account?ok=%E5%AF%86%E7%A0%81%E5%B7%B2%E6%9B%B4%E6%96%B0",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("toast-ok"), "account flash missing");
    assert!(html.contains("密码已更新"), "account flash text missing");

    let response = send(
        &t.app,
        Method::GET,
        "/account?err=%E6%93%8D%E4%BD%9C%E5%A4%B1%E8%B4%A5",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("toast-err"), "account error flash missing");

    // 顶栏搜索框要回填当前关键词，方便用户看到并继续修改搜索条件。
    let response = send(
        &t.app,
        Method::GET,
        "/files?q=report",
        Some(&cookie),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(
        html.contains("name=\"q\"") && html.contains("value=\"report\""),
        "search input must keep the current query"
    );

    // 文件页必须提供上传脚本依赖的 page-data 与上传入口标记。
    assert!(html.contains("id=\"page-data\""), "page-data missing");
    assert!(
        html.contains("data-file-input"),
        "file input marker missing"
    );
    assert!(
        html.contains("data-folder-input"),
        "folder input marker missing"
    );
}

#[tokio::test]
async fn versioned_assets_are_immutable_and_support_etag_revalidation() {
    let t = test_app().await;
    let version = cloudnest::assets::ASSET_VERSION;

    let response = send(
        &t.app,
        Method::GET,
        &format!("/static/v/{version}/app.css"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cache_control = header_text(&response, header::CACHE_CONTROL);
    let etag = header_text(&response, header::ETAG);
    assert_eq!(cache_control, "public, max-age=31536000, immutable");
    assert!(etag.starts_with('"') && etag.ends_with('"'), "etag: {etag}");
    let html = body_text(response).await;
    assert!(html.contains(":root"), "app.css body served");

    // 带 If-None-Match 的再次请求走 304，避免重复传输。
    let request = Request::builder()
        .uri(format!("/static/v/{version}/app.css"))
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let response = t.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(header_text(&response, header::ETAG), etag);
    let body = to_bytes(response.into_body(), 64).await.unwrap();
    assert!(body.is_empty(), "304 must not carry a body");

    // 字体同样按内容哈希提供。
    let response = send(
        &t.app,
        Method::GET,
        &format!("/static/v/{version}/fonts/space-grotesk-latin-400-normal.woff2"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_text(&response, header::CONTENT_TYPE), "font/woff2");
}

#[tokio::test]
async fn legacy_and_outdated_asset_urls_redirect_to_current_version() {
    let t = test_app().await;
    let version = cloudnest::assets::ASSET_VERSION;
    let current = format!("/static/v/{version}/app.js");

    // 升级前缓存的旧 HTML 只会请求固定地址：必须跳到当前版本，且跳转不可缓存。
    let response = send(
        &t.app,
        Method::GET,
        "/static/app.js",
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_text(&response, header::LOCATION), current);
    assert_eq!(header_text(&response, header::CACHE_CONTROL), "no-store");

    let response = send(
        &t.app,
        Method::GET,
        "/static/fonts/ibm-plex-mono-latin-500-normal.woff2",
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_text(&response, header::LOCATION),
        format!("/static/v/{version}/fonts/ibm-plex-mono-latin-500-normal.woff2")
    );

    // 旧版本号（例如浏览器还留着上一版页面）也回到当前版本，不会 404。
    let response = send(
        &t.app,
        Method::GET,
        "/static/v/000000000000/app.js",
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_text(&response, header::LOCATION), current);

    // 未知资源仍然 404，避免把任意路径都当成跳转。
    let response = send(
        &t.app,
        Method::GET,
        &format!("/static/v/{version}/missing.js"),
        None,
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
