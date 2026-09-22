//! 认证路由处理器：登录、当前用户、修改密码（对齐 Python auth.py）。

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use rand::seq::SliceRandom;
use rusqlite::params;
use serde_json::json;

use crate::auth::{
    create_access_token, hash_password_async, verify_password_async, AuthUser, ClientIp,
};
use crate::db::utc_now_iso;
use crate::errors::{ApiError, ApiResult};
use crate::schemas;
use crate::state::AppState;

const FAIL_MESSAGES: [&str; 3] = [
    "用户名或密码错误",
    "用户名或密码错误，请重试",
    "账号或密码不正确",
];

pub async fn login(
    State(state): State<AppState>,
    ip: ClientIp,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let req = schemas::login_request(&body)?;
    let key = format!("{}:{}", ip.0, req.username);

    let locked = state.limiter.check_locked(&key);
    if locked > 0 {
        return Err(ApiError::too_many_requests(format!(
            "失败次数过多，请 {} 分钟后重试",
            locked / 60 + 1
        )));
    }

    let user: Option<(i64, String, String)> = state
        .db
        .lock()
        .query_row(
            "SELECT id, username, password_hash FROM users WHERE username = ?1",
            params![req.username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    let ok = match &user {
        Some((_, _, hash)) => verify_password_async(req.password.clone(), hash.clone()).await,
        None => false,
    };
    if !ok {
        state.limiter.record_failure(&key);
        // 统一错误消息，不泄露用户是否存在
        let msg = FAIL_MESSAGES.choose(&mut rand::thread_rng()).unwrap_or(&FAIL_MESSAGES[0]);
        return Err(ApiError::unauthorized(*msg));
    }

    state.limiter.reset(&key);
    let (uid, username, _) = user.expect("认证通过则用户必存在");
    let token = create_access_token(uid, &state.cfg);
    Ok(Json(json!({
        "token": token,
        "user": { "id": uid, "username": username },
    })))
}

pub async fn me(user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({ "id": user.id, "username": user.username })))
}

pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let req = schemas::password_change(&body)?;

    let hash: Option<String> = state
        .db
        .lock()
        .query_row(
            "SELECT password_hash FROM users WHERE id = ?1",
            [user.id],
            |r| r.get(0),
        )
        .optional()?;
    let hash = hash.ok_or_else(|| ApiError::unauthorized("未登录或登录已过期"))?;

    if !verify_password_async(req.old_password.clone(), hash).await {
        return Err(ApiError::bad_request("原密码错误"));
    }
    if req.old_password == req.new_password {
        return Err(ApiError::bad_request("新密码不能与原密码相同"));
    }

    let new_hash = hash_password_async(req.new_password).await;
    let now = utc_now_iso();
    state.db.lock().execute(
        "UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE id = ?3",
        params![new_hash, now, user.id],
    )?;
    Ok(Json(json!({ "message": "密码修改成功" })))
}

use rusqlite::OptionalExtension;
