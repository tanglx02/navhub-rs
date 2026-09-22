//! 应用管理处理器：公开读取，认证后写入（对齐 Python apps.py）。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Uri;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::params;

use crate::auth::{AuthUser, OptionalAuthUser};
use crate::db::{row_to_app, utc_now_iso, AppOut, APP_COLS};
use crate::errors::{ApiError, ApiResult, ValErr};
use crate::handlers::{query_param, apply_update};
use crate::schemas;
use crate::state::AppState;

use rusqlite::OptionalExtension;

pub async fn list_apps(
    State(state): State<AppState>,
    user: OptionalAuthUser,
    uri: Uri,
) -> ApiResult<Json<Vec<AppOut>>> {
    let all = match query_param(&uri, "all") {
        None => false,
        Some(v) => schemas::query_bool(v)
            .ok_or_else(|| ApiError::unprocessable_one(ValErr::bool_parsing("query", "all")))?,
    };
    // 公开接口默认只返回启用应用；all=true 且带有效登录态时返回全部
    let where_clause = if all && user.0.is_some() { "" } else { "WHERE status = 1" };
    let sql = format!("SELECT {} FROM apps {} ORDER BY sort_order, id", APP_COLS, where_clause);

    let conn = state.db.lock();
    let mut stmt = conn.prepare(&sql)?;
    let apps = stmt.query_map([], row_to_app)?.collect::<Result<Vec<_>, _>>()?;
    Ok(Json(apps))
}

pub async fn get_app(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<AppOut>> {
    Ok(Json(fetch_app(&state, id)?))
}

fn fetch_app(state: &AppState, id: i64) -> ApiResult<AppOut> {
    let sql = format!("SELECT {APP_COLS} FROM apps WHERE id = ?1");
    state
        .db
        .lock()
        .query_row(&sql, [id], row_to_app)
        .optional()?
        .ok_or_else(|| ApiError::not_found("应用不存在"))
}

pub async fn create_app(
    State(state): State<AppState>,
    _user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let data = schemas::app_create(&body)?;
    let now = utc_now_iso();
    let conn = state.db.lock();
    let inserted = conn.execute(
        "INSERT INTO apps (name, url, description, icon_type, icon_value, color, category_id, sort_order, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            data.name,
            data.url,
            data.description,
            data.icon_type,
            data.icon_value,
            data.color,
            data.category_id,
            data.sort_order,
            data.status,
            now
        ],
    );
    drop(conn);
    match inserted {
        Ok(_) => {
            let id = state.db.lock().last_insert_rowid();
            let app = fetch_app(&state, id)?;
            Ok((StatusCode::CREATED, Json(app)).into_response())
        }
        Err(e) if ApiError::is_constraint(&e) => {
            Err(ApiError::bad_request("数据写入失败，请检查分类是否存在"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn update_app(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
    body: Bytes,
) -> ApiResult<Json<AppOut>> {
    let pairs = schemas::app_update(&body)?;
    let _existing = fetch_app(&state, id)?;
    if !pairs.is_empty() {
        let now = utc_now_iso();
        let conn = state.db.lock();
        let result = apply_update(&conn, "apps", id, &pairs, &now);
        drop(conn);
        match result {
            Ok(_) => {}
            Err(e) if ApiError::is_constraint(&e) => {
                return Err(ApiError::bad_request("数据更新失败，请检查分类是否存在"))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Json(fetch_app(&state, id)?))
}

pub async fn toggle_status(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
    uri: Uri,
) -> ApiResult<Json<AppOut>> {
    let raw = query_param(&uri, "status")
        .ok_or_else(|| ApiError::unprocessable_one(ValErr::param_missing("query", "status")))?;
    let status = schemas::query_bool(raw)
        .ok_or_else(|| ApiError::unprocessable_one(ValErr::bool_parsing("query", "status")))?;

    let now = utc_now_iso();
    let changed = state.db.lock().execute(
        "UPDATE apps SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status, now, id],
    )?;
    if changed == 0 {
        return Err(ApiError::not_found("应用不存在"));
    }
    Ok(Json(fetch_app(&state, id)?))
}

pub async fn delete_app(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let changed = state.db.lock().execute("DELETE FROM apps WHERE id = ?1", [id])?;
    if changed == 0 {
        return Err(ApiError::not_found("应用不存在"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
