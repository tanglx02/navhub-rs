//! 应用状态：配置、数据库、登录限速器（均为 Arc 共享，整体 Clone 廉价）。

use std::sync::Arc;

use crate::auth::LoginRateLimiter;
use crate::config::Config;
use crate::db::Db;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Arc<Db>,
    pub limiter: Arc<LoginRateLimiter>,
}

impl AppState {
    pub fn new(cfg: Arc<Config>, db: Arc<Db>) -> Self {
        Self {
            limiter: Arc::new(LoginRateLimiter::new(cfg.login_max_failures, cfg.login_lock_minutes)),
            cfg,
            db,
        }
    }
}
