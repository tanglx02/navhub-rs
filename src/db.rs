//! SQLite 数据层：连接管理、建表、种子数据（与 Python 版 schema/seed 完全对齐）。

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::auth::hash_password;
use crate::config::Config;

/// UTC 当前时间，ISO 8601 微秒精度（对齐 SQLAlchemy DateTime 序列化格式）。
pub fn utc_now_iso() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{micros:06}+00:00",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil_from_days 算法：Unix 天数 → (年, 月, 日)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username VARCHAR(50) NOT NULL UNIQUE,
    password_hash VARCHAR(128) NOT NULL,
    created_at DATETIME,
    updated_at DATETIME
);
CREATE TABLE IF NOT EXISTS categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name VARCHAR(50) NOT NULL UNIQUE,
    icon VARCHAR(64),
    sort_order INTEGER,
    created_at DATETIME,
    updated_at DATETIME
);
CREATE TABLE IF NOT EXISTS apps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name VARCHAR(100) NOT NULL,
    url VARCHAR(2048) NOT NULL,
    description TEXT,
    icon_type VARCHAR(10),
    icon_value VARCHAR(2048),
    color VARCHAR(9),
    category_id INTEGER,
    sort_order INTEGER,
    status BOOLEAN,
    created_at DATETIME,
    updated_at DATETIME,
    FOREIGN KEY (category_id) REFERENCES categories (id) ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key VARCHAR(50) PRIMARY KEY,
    value TEXT
);
";

/// 默认分类（与 Python seed 一致）。
pub const DEFAULT_CATEGORIES: &[(&str, &str, i64)] = &[
    ("常用工具", "tool", 0),
    ("开发运维", "code", 1),
    ("影音娱乐", "media", 2),
    ("网盘存储", "cloud", 3),
    ("系统设备", "server", 4),
];

/// 示例应用：(名称, 链接, 描述, 所属分类, 图标, 排序)。
pub const SAMPLE_APPS: &[(&str, &str, &str, &str, &str, i64)] = &[
    ("本导航系统", "http://localhost:8100/", "编辑我或删除我", "常用工具", "compass", 0),
    ("路由器管理", "http://192.168.1.1", "家庭路由器后台", "系统设备", "router", 0),
    ("Nginx 示例站", "https://nginx.org", "示例条目，可在后台删除", "开发运维", "server", 1),
];

/// 数据库句柄：单连接 + 互斥锁。个人导航站并发极低，单连接足够且最省内存。
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// 打开连接、启用 WAL / 外键、建表并写入种子数据。
    pub fn init(cfg: &Config) -> rusqlite::Result<Db> {
        let conn = Connection::open(&cfg.database_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        let db = Db { conn: Mutex::new(conn) };
        db.seed(cfg)?;
        Ok(db)
    }

    /// 短暂持锁访问连接（所有查询均为微秒级，不跨 await）。
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("数据库锁中毒")
    }

    /// 首次启动初始化：管理员、默认分类、示例应用、站点设置。
    fn seed(&self, cfg: &Config) -> rusqlite::Result<()> {
        let now = utc_now_iso();
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        // 管理员账号
        let has_admin: i64 = tx.query_row(
            "SELECT COUNT(*) FROM users WHERE username = ?1",
            params![cfg.admin_username],
            |r| r.get(0),
        )?;
        if has_admin == 0 {
            let hash = hash_password(&cfg.admin_password);
            tx.execute(
                "INSERT INTO users (username, password_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                params![cfg.admin_username, hash, now],
            )?;
            tracing::warn!(
                "已创建初始管理员账号 {:?}（默认密码，请登录后立即修改）",
                cfg.admin_username
            );
        }

        // 默认分类（按名称查重，等价 Python 逐条检查逻辑）
        for (name, icon, sort) in DEFAULT_CATEGORIES {
            let exists: i64 = tx.query_row(
                "SELECT COUNT(*) FROM categories WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )?;
            if exists == 0 {
                tx.execute(
                    "INSERT INTO categories (name, icon, sort_order, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![name, icon, sort, now],
                )?;
            }
        }

        // 示例应用（仅当应用表为空时写入）
        let app_count: i64 = tx.query_row("SELECT COUNT(*) FROM apps", [], |r| r.get(0))?;
        if app_count == 0 {
            for (name, url, desc, cat, icon, sort) in SAMPLE_APPS {
                tx.execute(
                    "INSERT INTO apps (name, url, description, icon_type, icon_value, color, category_id, sort_order, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'builtin', ?4, NULL, (SELECT id FROM categories WHERE name = ?5), ?6, 1, ?7, ?7)",
                    params![name, url, desc, icon, cat, sort, now],
                )?;
            }
        }

        // 站点设置
        let defaults: &[(&str, &str)] = &[
            ("site_name", &cfg.app_name),
            ("site_description", "集中管理与快速访问我的所有 Web 应用"),
        ];
        for (key, value) in defaults {
            tx.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }

        tx.commit()
    }
}

/// 应用输出模型（对齐 Python AppOut 字段）。
#[derive(Debug, Serialize)]
pub struct AppOut {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub icon_type: Option<String>,
    pub icon_value: Option<String>,
    pub color: Option<String>,
    pub category_id: Option<i64>,
    pub sort_order: i64,
    pub status: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub const APP_COLS: &str =
    "id, name, url, description, icon_type, icon_value, color, category_id, sort_order, status, created_at, updated_at";

pub fn row_to_app(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppOut> {
    Ok(AppOut {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        description: row.get(3)?,
        icon_type: row.get(4)?,
        icon_value: row.get(5)?,
        color: row.get(6)?,
        category_id: row.get(7)?,
        sort_order: row.get::<_, Option<i64>>(8)?.unwrap_or(0),
        status: row.get::<_, Option<bool>>(9)?.unwrap_or(true),
        created_at: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
        updated_at: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
    })
}

/// 应用表路径穿越防护/规范化辅助：确认数据目录存在（供静态服务使用）。
pub fn ensure_dir(path: &Path) {
    let _ = std::fs::create_dir_all(path);
}
