//! 路由装配：API 路由、上传目录静态托管、前端 SPA 回退、CORS 与安全响应头。
//!
//! 行为对齐 Python 版 main.py：
//! - CORS 允许方法/头白名单一致；`CORS_ORIGINS=*` 时镜像请求 Origin（等价 Starlette 带凭证行为）
//! - 全部响应追加 X-Content-Type-Options / X-Frame-Options / Referrer-Policy
//! - dist 存在时：/assets 静态目录 + 根级文件服务（防路径穿越）+ index.html 回退，
//!   未知 /api/* 路径返回 404 `{"detail": "接口不存在"}`
//! - dist 不存在时：/ 返回前端构建提示

use std::path::Path;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, Method, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde_json::json;
use tower::ServiceExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::config::Config;
use crate::errors::ApiError;
use crate::handlers::{apps, auth, categories, settings, system, upload};
use crate::state::AppState;

/// 构建完整应用路由（含中间件层）。
pub fn build_router(state: AppState) -> Router {
    let cfg = state.cfg.clone();

    let api = Router::new()
        // 系统
        .route("/api/health", get(system::health))
        // 认证
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/password", put(auth::change_password))
        // 应用
        .route(
            "/api/apps",
            get(apps::list_apps).post(apps::create_app),
        )
        .route(
            "/api/apps/{app_id}",
            get(apps::get_app).put(apps::update_app).delete(apps::delete_app),
        )
        .route("/api/apps/{app_id}/status", patch(apps::toggle_status))
        // 分类
        .route(
            "/api/categories",
            get(categories::list_categories).post(categories::create_category),
        )
        .route(
            "/api/categories/{category_id}",
            put(categories::update_category).delete(categories::delete_category),
        )
        // 图标上传：放大框架体限额到「业务上限 + 128KB」，让业务侧 413 校验先触发
        .route(
            "/api/upload",
            post(upload::upload_icon).layer(DefaultBodyLimit::max(
                cfg.upload_max_mb * 1024 * 1024 + 128 * 1024,
            )),
        )
        // 站点设置
        .route(
            "/api/settings",
            get(settings::get_settings).put(settings::update_settings),
        );

    let mut app = Router::new()
        .merge(api)
        .nest_service("/uploads", ServeDir::new(&cfg.uploads_dir));

    let dist = cfg.frontend_dist.clone();
    if dist.is_dir() {
        let assets = dist.join("assets");
        if assets.is_dir() {
            app = app.nest_service("/assets", ServeDir::new(assets));
        }
        app = app.fallback(get(spa_fallback));
    } else {
        app = app.route("/", get(root_hint)).fallback(get(no_dist_404));
    }

    app.with_state(state)
        .layer(cors_layer(&cfg))
        .layer(middleware::from_fn(security_headers))
}

/// 前端未构建时，其余路径的统一 404（对齐 FastAPI 默认行为）。
async fn no_dist_404() -> Response {
    ApiError::not_found("Not Found").into_response()
}

/// GET 通配回退：静态文件 → index.html（与 Python 版 spa_fallback 一致）。
async fn spa_fallback(State(state): State<AppState>, uri: Uri) -> Response {
    let dist = &state.cfg.frontend_dist;
    let rel = uri.path().trim_start_matches('/');
    if rel.starts_with("api/") {
        return ApiError::not_found("接口不存在").into_response();
    }
    if let Some(candidate) = resolve_dist_file(dist, rel) {
        match ServeFile::new(candidate)
            .oneshot(Request::new(Body::empty()))
            .await
        {
            Ok(res) => return res.into_response(),
            Err(_) => {}
        }
    }
    serve_index(dist).await
}

async fn serve_index(dist: &Path) -> Response {
    match ServeFile::new(dist.join("index.html"))
        .oneshot(Request::new(Body::empty()))
        .await
    {
        Ok(res) => res.into_response(),
        Err(_) => ApiError::not_found("Not Found").into_response(),
    }
}

/// 解析并校验 dist 内的候选文件：percent 解码 + canonicalize 防路径穿越。
fn resolve_dist_file(dist: &Path, rel: &str) -> Option<std::path::PathBuf> {
    if rel.is_empty() {
        return None;
    }
    let decoded = percent_decode(rel)?;
    let candidate = dist.join(decoded);
    let canonical = candidate.canonicalize().ok()?;
    let dist_canonical = dist.canonicalize().ok()?;
    if canonical.is_file() && canonical.starts_with(&dist_canonical) {
        Some(canonical)
    } else {
        None
    }
}

/// 前端未构建时的根路径提示（对齐 Python 版 root_hint）。
async fn root_hint() -> Json<serde_json::Value> {
    Json(json!({
        "message": "前端尚未构建。请在 frontend 目录执行 npm run build，或参见 docs/DEPLOY.md。"
    }))
}

/// CORS 层：方法 / 头白名单与原 CORSMiddleware 配置一致。
fn cors_layer(cfg: &Config) -> CorsLayer {
    let origin = if cfg.cors_origins.trim() == "*" {
        // 带凭证时不能回显字面 "*"，镜像请求 Origin（等价 Starlette 行为）
        AllowOrigin::mirror_request()
    } else {
        let list: Vec<HeaderValue> = cfg
            .cors_origins
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| HeaderValue::from_str(s).ok())
            .collect();
        AllowOrigin::list(list)
    };
    CorsLayer::new()
        .allow_origin(origin)
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// 为所有响应追加安全头（对齐 Python 版 security_headers 中间件）。
async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    res
}

/// 最小百分号解码（仅用于静态文件路径还原）。
fn percent_decode(s: &str) -> Option<String> {
    fn hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = hex(*bytes.get(i + 1)?)?;
                let lo = hex(*bytes.get(i + 2)?)?;
                out.push(hi << 4 | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}
