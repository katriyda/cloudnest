use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use cloudnest::config::Config;
use cloudnest::state::AppState;
use serde_json::Value;
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
    let state = AppState::new(cfg).await.expect("app state");
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
