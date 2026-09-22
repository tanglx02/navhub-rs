//! 安全模块：bcrypt 密码哈希、JWT 签发/校验、登录限速、认证提取器。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::json;

use crate::config::Config;
use crate::errors::{ApiError, ApiResult};
use crate::state::AppState;

pub const BCRYPT_COST: u32 = 12;

// ---------- 密码 ----------

pub fn hash_password(plain: &str) -> String {
    bcrypt::hash(plain, BCRYPT_COST).expect("bcrypt 哈希失败")
}

pub fn verify_password(plain: &str, hashed: &str) -> bool {
    bcrypt::verify(plain, hashed).unwrap_or(false)
}

/// 在阻塞线程中执行昂贵的 bcrypt 校验，避免阻塞 tokio worker。
pub async fn verify_password_async(plain: String, hashed: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&plain, &hashed))
        .await
        .unwrap_or(false)
}

/// 在阻塞线程中执行昂贵的 bcrypt 哈希。
pub async fn hash_password_async(plain: String) -> String {
    tokio::task::spawn_blocking(move || hash_password(&plain))
        .await
        .expect("bcrypt 任务失败")
}

// ---------- JWT ----------

pub fn create_access_token(user_id: i64, cfg: &Config) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let claims = json!({
        "sub": user_id.to_string(),
        "exp": now + cfg.token_expire_minutes * 60,
        "iat": now,
    });
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(cfg.secret_key.as_bytes()),
    )
    .expect("JWT 签发失败")
}

/// 解码并校验 token，返回用户 ID；失败返回 None。
pub fn decode_token(token: &str, cfg: &Config) -> Option<i64> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let data = jsonwebtoken::decode::<serde_json::Value>(
        token,
        &DecodingKey::from_secret(cfg.secret_key.as_bytes()),
        &validation,
    )
    .ok()?;
    data.claims["sub"].as_str()?.parse().ok()
}

// ---------- 登录限速（内存实现，单进程场景足够） ----------

pub struct LoginRateLimiter {
    max_failures: usize,
    lock_seconds: u64,
    records: Mutex<HashMap<String, Vec<Instant>>>,
}

impl LoginRateLimiter {
    pub fn new(max_failures: usize, lock_minutes: i64) -> Self {
        Self {
            max_failures,
            lock_seconds: (lock_minutes.max(0) as u64) * 60,
            records: Mutex::new(HashMap::new()),
        }
    }

    /// 返回剩余锁定秒数；0 表示未锁定。
    pub fn check_locked(&self, key: &str) -> u64 {
        let mut map = self.records.lock().expect("限速锁中毒");
        let now = Instant::now();
        let records = map.entry(key.to_string()).or_default();
        records.retain(|t| now.duration_since(*t).as_secs() < self.lock_seconds);
        if records.len() >= self.max_failures {
            let elapsed = now.duration_since(records[0]).as_secs();
            self.lock_seconds.saturating_sub(elapsed).max(1)
        } else {
            0
        }
    }

    pub fn record_failure(&self, key: &str) {
        self.records
            .lock()
            .expect("限速锁中毒")
            .entry(key.to_string())
            .or_default()
            .push(Instant::now());
    }

    pub fn reset(&self, key: &str) {
        self.records.lock().expect("限速锁中毒").remove(key);
    }
}

// ---------- 认证提取器 ----------

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
    pub username: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let token = bearer_token(parts);
        let ok = match token {
            Some(t) => decode_token(&t, &state.cfg)
                .and_then(|uid| lookup_user(state, uid)),
            None => None,
        };
        ok.ok_or_else(|| ApiError::unauthorized("未登录或登录已过期"))
    }
}

/// 可选认证：有有效 Token 返回用户，否则 None（不报错）。
#[derive(Debug, Clone, Default)]
pub struct OptionalAuthUser(pub Option<AuthUser>);

impl FromRequestParts<AppState> for OptionalAuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = bearer_token(parts)
            .and_then(|t| decode_token(&t, &state.cfg))
            .and_then(|uid| lookup_user(state, uid));
        Ok(Self(user))
    }
}

fn lookup_user(state: &AppState, uid: i64) -> Option<AuthUser> {
    state
        .db
        .lock()
        .query_row(
            "SELECT id, username FROM users WHERE id = ?1",
            [uid],
            |r| {
                Ok(AuthUser {
                    id: r.get(0)?,
                    username: r.get(1)?,
                })
            },
        )
        .ok()
}

/// 解析 `Authorization: Bearer <token>`（scheme 大小写不敏感）。
fn bearer_token(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let mut it = value.split_whitespace();
    let scheme = it.next()?;
    let token = it.next()?;
    if it.next().is_some() {
        return None;
    }
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token.to_string())
    } else {
        None
    }
}

// ---------- 客户端 IP ----------

pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        Ok(Self(ip))
    }
}
