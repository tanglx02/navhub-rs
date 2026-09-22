//! 业务处理器：auth / apps / categories / upload / settings / system。

pub mod apps;
pub mod auth;
pub mod categories;
pub mod settings;
pub mod system;
pub mod upload;

use axum::http::Uri;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;

use crate::schemas::Pairs;

/// 从查询串取参数原始值（本服务查询值均为 ASCII，无需百分号解码）。
pub(crate) fn query_param<'a>(uri: &'a Uri, key: &str) -> Option<&'a str> {
    let q = uri.query()?;
    q.split('&')
        .find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

/// 动态部分更新：SET 列名全部来自 schemas 白名单常量，无注入风险。
pub(crate) fn apply_update(
    conn: &Connection,
    table: &str,
    id: i64,
    pairs: &Pairs,
    now: &str,
) -> rusqlite::Result<usize> {
    let mut sql = format!("UPDATE {table} SET ");
    let mut vals: Vec<SqlValue> = Vec::with_capacity(pairs.len() + 2);
    for (i, (col, val)) in pairs.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("{col} = ?{}", i + 1));
        vals.push(val.clone());
    }
    let n = pairs.len();
    sql.push_str(&format!(", updated_at = ?{} WHERE id = ?{}", n + 1, n + 2));
    vals.push(SqlValue::Text(now.to_string()));
    vals.push(SqlValue::Integer(id));

    let refs: Vec<&dyn rusqlite::ToSql> = vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.prepare_cached(&sql)?.execute(refs.as_slice())
}
