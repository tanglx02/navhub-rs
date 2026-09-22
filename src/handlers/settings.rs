//! 站点设置处理器：公开读取，认证后写入（对齐 Python settings.py）。

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use rusqlite::params;
use serde::Serialize;

use crate::auth::AuthUser;
use crate::errors::ApiResult;
use crate::schemas;
use crate::state::AppState;

const DEFAULTS: [(&str, &str); 2] = [
    ("site_name", "NavHub"),
    ("site_description", "我的应用导航"),
];

#[derive(Debug, Serialize)]
pub struct SettingsOut {
    pub site_name: String,
    pub site_description: String,
}

fn load_settings(state: &AppState) -> ApiResult<SettingsOut> {
    let mut map = serde_json::Map::new();
    for (k, v) in DEFAULTS {
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    let conn = state.db.lock();
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt.query_map([], |r| -> rusqlite::Result<(String, Option<String>)> {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    for row in rows {
        let (key, value) = row?;
        if value.is_some() && DEFAULTS.iter().any(|(k, _)| *k == key) {
            map.insert(key, serde_json::Value::String(value.unwrap()));
        }
    }
    Ok(SettingsOut {
        site_name: map["site_name"].as_str().unwrap_or("NavHub").to_string(),
        site_description: map["site_description"].as_str().unwrap_or("我的应用导航").to_string(),
    })
}

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<SettingsOut>> {
    Ok(Json(load_settings(&state)?))
}

pub async fn update_settings(
    State(state): State<AppState>,
    _user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<SettingsOut>> {
    let updates = schemas::settings_update(&body)?;
    {
        let conn = state.db.lock();
        for (key, value) in &updates {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![key, value],
            )?;
        }
    }
    Ok(Json(load_settings(&state)?))
}
