//! 分类管理处理器（对齐 Python categories.py，含 app_count 统计）。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::params;
use serde::Serialize;

use crate::auth::AuthUser;
use crate::db::utc_now_iso;
use crate::errors::{ApiError, ApiResult};
use crate::handlers::apply_update;
use crate::schemas;
use crate::state::AppState;

use rusqlite::OptionalExtension;

#[derive(Debug, Serialize)]
pub struct CategoryOut {
    pub id: i64,
    pub name: String,
    pub icon: Option<String>,
    pub sort_order: i64,
    pub app_count: i64,
}

const CAT_COLS: &str = "id, name, icon, sort_order";

pub async fn list_categories(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<CategoryOut>>> {
    let sql = format!(
        "SELECT {CAT_COLS}, (SELECT COUNT(*) FROM apps WHERE apps.category_id = categories.id) FROM categories ORDER BY sort_order, id"
    );
    let conn = state.db.lock();
    let mut stmt = conn.prepare(&sql)?;
    let cats = stmt.query_map([], row_to_cat)?.collect::<Result<Vec<_>, _>>()?;
    Ok(Json(cats))
}

fn row_to_cat(row: &rusqlite::Row<'_>) -> rusqlite::Result<CategoryOut> {
    Ok(CategoryOut {
        id: row.get(0)?,
        name: row.get(1)?,
        icon: row.get(2)?,
        sort_order: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
        app_count: row.get(4)?,
    })
}

fn fetch_cat(state: &AppState, id: i64) -> ApiResult<CategoryOut> {
    let sql = format!(
        "SELECT {CAT_COLS}, (SELECT COUNT(*) FROM apps WHERE apps.category_id = categories.id) FROM categories WHERE id = ?1"
    );
    state
        .db
        .lock()
        .query_row(&sql, [id], row_to_cat)
        .optional()?
        .ok_or_else(|| ApiError::not_found("分类不存在"))
}

pub async fn create_category(
    State(state): State<AppState>,
    _user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let data = schemas::category_create(&body)?;
    let now = utc_now_iso();
    let conn = state.db.lock();
    let inserted = conn.execute(
        "INSERT INTO categories (name, icon, sort_order, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![data.name, data.icon, data.sort_order, now],
    );
    drop(conn);
    match inserted {
        Ok(_) => {
            let id = state.db.lock().last_insert_rowid();
            Ok((StatusCode::CREATED, Json(fetch_cat(&state, id)?)).into_response())
        }
        Err(e) if ApiError::is_constraint(&e) => Err(ApiError::bad_request("分类名称已存在")),
        Err(e) => Err(e.into()),
    }
}

pub async fn update_category(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
    body: Bytes,
) -> ApiResult<Json<CategoryOut>> {
    let pairs = schemas::category_update(&body)?;
    let _existing = fetch_cat(&state, id)?;
    if !pairs.is_empty() {
        let now = utc_now_iso();
        let conn = state.db.lock();
        let result = apply_update(&conn, "categories", id, &pairs, &now);
        drop(conn);
        match result {
            Ok(_) => {}
            Err(e) if ApiError::is_constraint(&e) => {
                return Err(ApiError::bad_request("分类名称已存在"))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Json(fetch_cat(&state, id)?))
}

pub async fn delete_category(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    // 删除分类，其下应用自动归入「未分类」（外键 ON DELETE SET NULL）
    let changed = state.db.lock().execute("DELETE FROM categories WHERE id = ?1", [id])?;
    if changed == 0 {
        return Err(ApiError::not_found("分类不存在"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
