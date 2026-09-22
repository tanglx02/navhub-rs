//! NavHub（Rust + Axum）— 自托管 Web 应用导航站。
//!
//! 模块划分：
//! - [`config`]：配置加载（环境变量 / .env / 自动生成 JWT 密钥）
//! - [`db`]：SQLite 连接、建表、种子数据与领域查询
//! - [`auth`]：bcrypt 密码、JWT、登录限速、认证提取器
//! - [`errors`]：统一 API 错误（`{"detail": "..."}`，与前端契约对齐）
//! - [`schemas`]：请求体结构与边界校验（URL / 颜色 / 长度 / 枚举）
//! - [`handlers`]：业务处理器（auth / apps / categories / upload / settings / system）
//! - [`routes`]：路由装配、静态托管、SPA 回退、安全响应头
pub mod auth;
pub mod config;
pub mod db;
pub mod errors;
pub mod handlers;
pub mod routes;
pub mod schemas;
pub mod state;

pub use routes::build_router;
pub use state::AppState;

use tracing_subscriber::EnvFilter;

/// 程序入口（由 src/main.rs 调用）。
pub fn main_entry() {
    let workers = std::env::var("NAVHUB_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2); // 个人站 2 个 worker 足够，内存占用更低

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("navhub=info,tower_http=warn")),
        )
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .expect("创建 tokio 运行时失败");

    if let Err(e) = runtime.block_on(run()) {
        tracing::error!("服务异常退出: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = std::sync::Arc::new(config::Config::load());
    let db = std::sync::Arc::new(db::Db::init(&cfg)?);
    let state = AppState::new(cfg.clone(), db);

    let app = build_router(state.clone());

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("NavHub(Rust) 已启动: http://{} （监听 {addr}）", cfg.host);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("收到停止信号，正在优雅退出…");
}
