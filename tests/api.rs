//! NavHub(Rust) API 全功能集成测试：与 Python 版 backend/tests/test_api.py 逐项对齐。
//!
//! 每个测试使用独立临时目录（NAVHUB_BASE_DIR 等价物）与独立 SQLite 库，
//! 通过 tower::ServiceExt::oneshot 直接驱动路由，无需监听端口。

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, Method, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use navhub::config::Config;
use navhub::db::Db;
use navhub::{build_router, AppState};

/// 独立测试环境：临时 base_dir + 独立数据库 + 完整路由。
struct TestApp {
    app: Router,
    tmp: PathBuf,
}

impl TestApp {
    fn new(with_dist: bool) -> TestApp {
        let tmp = std::env::temp_dir().join(format!("navhub-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let data_dir = tmp.join("database");
        let frontend_dist = tmp.join("frontend").join("dist");
        if with_dist {
            std::fs::create_dir_all(&frontend_dist).unwrap();
            std::fs::write(frontend_dist.join("index.html"), "<html><body>SPA</body></html>")
                .unwrap();
        }

        let cfg = Config {
            base_dir: tmp.clone(),
            app_name: "NavHub".into(),
            host: "127.0.0.1".into(),
            port: 0,
            debug: false,
            secret_key: String::new(),
            token_expire_minutes: 1440,
            cors_origins: "*".into(),
            login_max_failures: 5,
            login_lock_minutes: 15,
            uploads_dir: data_dir.join("uploads"),
            data_dir: data_dir.clone(),
            upload_max_mb: 2,
            admin_username: "admin".into(),
            admin_password: "admin123".into(),
            database_path: data_dir.join("nav.db"),
            frontend_dist,
        };
        let cfg = Arc::new(Config::finalize(cfg));
        let db = Arc::new(Db::init(&cfg).expect("初始化数据库失败"));
        let app = build_router(AppState::new(cfg, db));
        TestApp { app, tmp }
    }

    /// 驱动路由处理一个请求，返回 (状态码, 响应头, JSON 体或原始文本)。
    async fn send(&self, req: Request) -> (StatusCode, HeaderMap, Value) {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let (parts, body) = res.into_parts();
        let bytes = to_bytes(body, 10 * 1024 * 1024).await.unwrap();
        let json = match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(_) => Value::String(String::from_utf8_lossy(&bytes).into_owned()),
        };
        (parts.status, parts.headers, json)
    }

    /// 发送请求，返回 (状态码, 响应头, JSON 体或原始文本)。
    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Body,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        let req: Request = builder.body(body).unwrap();
        self.send(req).await
    }

    async fn json_call(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        payload: Value,
    ) -> (StatusCode, HeaderMap, Value) {
        let req = self.build_with_ct(
            method,
            uri,
            token,
            "application/json",
            Body::from(payload.to_string()),
        );
        self.send(req).await
    }

    fn build_with_ct(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        content_type: &str,
        body: Body,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", content_type);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(body).unwrap()
    }

    async fn upload(
        &self,
        token: Option<&str>,
        filename: &str,
        content_type: &str,
        data: &[u8],
    ) -> (StatusCode, HeaderMap, Value) {
        let boundary = "----navhubtestboundary";
        let mut buf = Vec::new();
        buf.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        buf.extend_from_slice(data);
        buf.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let req = self.build_with_ct(
            Method::POST,
            "/api/upload",
            token,
            &format!("multipart/form-data; boundary={boundary}"),
            Body::from(buf),
        );
        self.send(req).await
    }

    /// 以 admin/admin123 登录，返回 token。
    async fn login(&self) -> String {
        let (status, _, body) = self
            .json_call(
                Method::POST,
                "/api/auth/login",
                None,
                json!({ "username": "admin", "password": "admin123" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "登录失败: {body}");
        body["token"].as_str().unwrap().to_string()
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn header_str(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(String::from)
}

// ---------- 基础 ----------

#[tokio::test]
async fn test_health() {
    let app = TestApp::new(false);
    let (status, _, body) = app.call(Method::GET, "/api/health", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "ok" }));
}

#[tokio::test]
async fn test_seed_data_exists() {
    let app = TestApp::new(false);
    let (_, _, cats) = app.call(Method::GET, "/api/categories", None, Body::empty()).await;
    assert!(cats.as_array().unwrap().len() >= 1);
    let (_, _, apps) = app.call(Method::GET, "/api/apps", None, Body::empty()).await;
    assert!(apps.as_array().unwrap().len() >= 1);
}

// ---------- 认证与权限 ----------

#[tokio::test]
async fn test_protected_api_requires_token() {
    let app = TestApp::new(false);
    assert_eq!(
        app.json_call(Method::POST, "/api/apps", None, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.call(Method::DELETE, "/api/apps/1", None, Body::empty()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.json_call(Method::POST, "/api/categories", None, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.json_call(Method::PUT, "/api/settings", None, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.upload(None, "a.png", "image/png", b"x").await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn test_invalid_token_rejected() {
    let app = TestApp::new(false);
    let (status, _, _) = app
        .call(Method::GET, "/api/auth/me", Some("invalid.token.here"), Body::empty())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_login_wrong_password() {
    let app = TestApp::new(false);
    let (status, _, _) = app
        .json_call(
            Method::POST,
            "/api/auth/login",
            None,
            json!({ "username": "admin", "password": "wrong-pass" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_login_me_and_change_password() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let (status, _, me) = app
        .call(Method::GET, "/api/auth/me", Some(&token), Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["username"], "admin");

    // 修改密码 → 旧密码失效 → 新密码可登录 → 还原
    let (status, _, _) = app
        .json_call(
            Method::PUT,
            "/api/auth/password",
            Some(&token),
            json!({ "old_password": "admin123", "new_password": "newpass123" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let bad = app
        .json_call(
            Method::POST,
            "/api/auth/login",
            None,
            json!({ "username": "admin", "password": "admin123" }),
        )
        .await;
    assert_eq!(bad.0, StatusCode::UNAUTHORIZED);
    let (status, _, body) = app
        .json_call(
            Method::POST,
            "/api/auth/login",
            None,
            json!({ "username": "admin", "password": "newpass123" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let token2 = body["token"].as_str().unwrap().to_string();
    let (status, _, _) = app
        .json_call(
            Method::PUT,
            "/api/auth/password",
            Some(&token2),
            json!({ "old_password": "newpass123", "new_password": "admin123" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------- 应用 CRUD ----------

#[tokio::test]
async fn test_app_crud_flow() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let payload = json!({
        "name": "测试应用",
        "url": "https://example.com/app",
        "description": "CRUD 测试",
        "icon_type": "builtin",
        "icon_value": "star",
        "color": "#4f7cffff",
        "category_id": null,
        "sort_order": 99,
        "status": true,
    });
    let (status, _, created) = app
        .json_call(Method::POST, "/api/apps", Some(&token), payload)
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let app_id = created["id"].as_i64().unwrap();

    // 公开列表可见（启用状态）
    let (_, _, list) = app.call(Method::GET, "/api/apps", None, Body::empty()).await;
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["id"].as_i64() == Some(app_id)));

    // 更新
    let (status, _, upd) = app
        .json_call(
            Method::PUT,
            &format!("/api/apps/{app_id}"),
            Some(&token),
            json!({ "name": "测试应用-改", "sort_order": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(upd["name"], "测试应用-改");
    assert_eq!(upd["sort_order"], 1);

    // 禁用后公开列表不可见，管理列表可见
    app.call(
        Method::PATCH,
        &format!("/api/apps/{app_id}/status?status=false"),
        Some(&token),
        Body::empty(),
    )
    .await;
    let (_, _, pub_list) = app.call(Method::GET, "/api/apps", None, Body::empty()).await;
    assert!(!pub_list
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["id"].as_i64() == Some(app_id)));
    let (_, _, all_list) = app
        .call(
            Method::GET,
            "/api/apps?all=true",
            Some(&token),
            Body::empty(),
        )
        .await;
    assert!(all_list
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["id"].as_i64() == Some(app_id)));

    // 删除
    let (status, _, _) = app
        .call(Method::DELETE, &format!("/api/apps/{app_id}"), Some(&token), Body::empty())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.call(Method::GET, &format!("/api/apps/{app_id}"), None, Body::empty())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.call(Method::DELETE, &format!("/api/apps/{app_id}"), Some(&token), Body::empty())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn test_app_url_validation() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let (status, _, _) = app
        .json_call(
            Method::POST,
            "/api/apps",
            Some(&token),
            json!({ "name": "x", "url": "javascript:alert(1)" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = app
        .json_call(
            Method::POST,
            "/api/apps",
            Some(&token),
            json!({ "name": "x", "url": "ftp://x.com" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_app_color_validation() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let (status, _, _) = app
        .json_call(
            Method::POST,
            "/api/apps",
            Some(&token),
            json!({ "name": "x", "url": "https://a.com", "color": "red" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_update_nonexistent_app() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let (status, _, _) = app
        .json_call(
            Method::PUT,
            "/api/apps/999999",
            Some(&token),
            json!({ "name": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------- 分类 CRUD ----------

#[tokio::test]
async fn test_category_crud_and_cascade() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let (status, _, cat) = app
        .json_call(
            Method::POST,
            "/api/categories",
            Some(&token),
            json!({ "name": "测试分类A", "icon": "code", "sort_order": 50 }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let cat_id = cat["id"].as_i64().unwrap();

    // 重名被拒
    let (status, _, _) = app
        .json_call(
            Method::POST,
            "/api/categories",
            Some(&token),
            json!({ "name": "测试分类A" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 应用挂到该分类
    let (_, _, app_json) = app
        .json_call(
            Method::POST,
            "/api/apps",
            Some(&token),
            json!({ "name": "分类内应用", "url": "https://c.com", "category_id": cat_id }),
        )
        .await;
    let app_id = app_json["id"].as_i64().unwrap();
    assert_eq!(app_json["category_id"], cat_id);

    // 分类列表带计数
    let (_, _, cats) = app.call(Method::GET, "/api/categories", None, Body::empty()).await;
    let target = cats
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "测试分类A")
        .unwrap();
    assert_eq!(target["app_count"], 1);

    // 删除分类 → 应用归为未分类
    let (status, _, _) = app
        .call(Method::DELETE, &format!("/api/categories/{cat_id}"), Some(&token), Body::empty())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, moved) = app
        .call(Method::GET, &format!("/api/apps/{app_id}"), None, Body::empty())
        .await;
    assert_eq!(moved["category_id"], Value::Null);
    app.call(Method::DELETE, &format!("/api/apps/{app_id}"), Some(&token), Body::empty())
        .await;
}

// ---------- 站点设置 ----------

#[tokio::test]
async fn test_settings_read_update() {
    let app = TestApp::new(false);
    let (status, _, s) = app.call(Method::GET, "/api/settings", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(s.get("site_name").is_some());

    let token = app.login().await;
    let (status, _, upd) = app
        .json_call(
            Method::PUT,
            "/api/settings",
            Some(&token),
            json!({ "site_name": "我的导航" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(upd["site_name"], "我的导航");
    // 公开可读
    let (_, _, s2) = app.call(Method::GET, "/api/settings", None, Body::empty()).await;
    assert_eq!(s2["site_name"], "我的导航");
}

// ---------- 上传安全 ----------

#[tokio::test]
async fn test_upload_requires_auth_and_validates() {
    let app = TestApp::new(false);
    let token = app.login().await;

    // PNG 魔数 + 随机内容
    let mut png: Vec<u8> = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&uuid::Uuid::new_v4().as_bytes().repeat(8));
    let (status, _, ok) = app.upload(Some(&token), "icon.png", "image/png", &png).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert!(ok["url"].as_str().unwrap().starts_with("/uploads/"));

    // 伪装为 png 的脚本（魔数不符）被拒
    let (status, _, _) = app
        .upload(Some(&token), "evil.png", "image/png", b"<?php system($_GET[0]); ?>")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 非白名单扩展名被拒
    let (status, _, _) = app
        .upload(Some(&token), "page.html", "text/html", b"<h1>hi</h1>")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------- 注入防护（参数化查询验证） ----------

#[tokio::test]
async fn test_sql_injection_safe() {
    let app = TestApp::new(false);
    let token = app.login().await;
    let payloads = [
        "'; DROP TABLE apps;--",
        "1' OR '1'='1",
        "\"><script>alert(1)</script>",
        "Robert'); DROP TABLE Students;--",
    ];
    for p in payloads {
        let (status, _, created) = app
            .json_call(
                Method::POST,
                "/api/apps",
                Some(&token),
                json!({ "name": p, "url": "https://inject.test" }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["id"].as_i64().unwrap();
        // 数据原样存储（作为普通字符串），表未被破坏
        let (_, _, got) = app
            .call(Method::GET, &format!("/api/apps/{id}"), None, Body::empty())
            .await;
        assert_eq!(got["name"], p);
        app.call(Method::DELETE, &format!("/api/apps/{id}"), Some(&token), Body::empty())
            .await;
    }
    // apps 表仍然可用
    assert_eq!(
        app.call(Method::GET, "/api/apps", None, Body::empty()).await.0,
        StatusCode::OK
    );
}

// ---------- 登录限速 ----------

#[tokio::test]
async fn test_login_rate_limit() {
    let app = TestApp::new(false);
    let victim = "rate-limit-victim";
    let mut codes = Vec::new();
    for _ in 0..7 {
        let (status, _, _) = app
            .json_call(
                Method::POST,
                "/api/auth/login",
                None,
                json!({ "username": victim, "password": "nope" }),
            )
            .await;
        codes.push(status);
    }
    assert_eq!(&codes[..5], &[StatusCode::UNAUTHORIZED; 5]);
    assert!(codes[5..].iter().all(|c| *c == StatusCode::TOO_MANY_REQUESTS));
}

// ---------- SPA 静态托管 ----------

#[tokio::test]
async fn test_spa_fallback() {
    let app = TestApp::new(true);
    let (status, headers, body) = app.call(Method::GET, "/", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(header_str(&headers, axum::http::header::CONTENT_TYPE)
        .unwrap_or_default()
        .contains("text/html"));
    assert_eq!(body, Value::String("<html><body>SPA</body></html>".into()));

    // 深层前端路由回退到 index.html
    let (status, _, _) = app
        .call(Method::GET, "/admin/apps", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);

    // 不存在的 API 路径不被 SPA 吞掉
    let (status, _, not_found) = app
        .call(Method::GET, "/api/nonexistent", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(not_found["detail"], "接口不存在");
}

#[tokio::test]
async fn test_no_dist_root_hint() {
    let app = TestApp::new(false);
    let (status, _, body) = app.call(Method::GET, "/", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["message"]
        .as_str()
        .unwrap_or_default()
        .contains("前端尚未构建"));
    // 前端缺失时其余路径按 FastAPI 惯例 404
    assert_eq!(
        app.call(Method::GET, "/admin/apps", None, Body::empty()).await.0,
        StatusCode::NOT_FOUND
    );
}

// ---------- 安全响应头 ----------

#[tokio::test]
async fn test_security_headers() {
    let app = TestApp::new(false);
    let (_, headers, _) = app.call(Method::GET, "/api/health", None, Body::empty()).await;
    assert_eq!(
        header_str(&headers, axum::http::header::X_CONTENT_TYPE_OPTIONS).as_deref(),
        Some("nosniff")
    );
    assert_eq!(
        header_str(&headers, axum::http::header::X_FRAME_OPTIONS).as_deref(),
        Some("DENY")
    );
    assert_eq!(
        header_str(&headers, axum::http::header::REFERRER_POLICY).as_deref(),
        Some("strict-origin-when-cross-origin")
    );
}
