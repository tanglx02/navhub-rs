//! 配置加载：环境变量 > .env 文件 > 默认值（与 Python 版字段名保持兼容）。
//!
//! 所有路径基于项目根目录（`NAVHUB_BASE_DIR` 或可执行文件所在目录）计算，
//! 保证在 systemd / 任意工作目录下均可正确运行。

use std::path::{Path, PathBuf};

/// 项目根目录：优先 `NAVHUB_BASE_DIR`，否则取可执行文件所在目录（部署时二进制与数据同根）。
pub fn base_dir() -> PathBuf {
    if let Ok(d) = std::env::var("NAVHUB_BASE_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 读取环境变量（兼容大小写，对齐 pydantic-settings 的不区分大小写行为）。
fn get_env(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    for k in [key, lower.as_str(), &key.to_uppercase()] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn env_bool(key: &str) -> Option<bool> {
    get_env(key).map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on" | "y" | "t"))
}

/// 读取或生成 JWT 密钥并持久化到 config/secret.key（权限敏感文件）。
fn load_or_create_secret(base: &Path) -> String {
    if let Some(k) = get_env("SECRET_KEY") {
        return k;
    }
    let key_file = base.join("config").join("secret.key");
    if let Ok(existing) = std::fs::read_to_string(&key_file) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return existing;
        }
    }
    let key = generate_secret();
    let _ = std::fs::create_dir_all(key_file.parent().unwrap_or(base));
    if std::fs::write(&key_file, &key).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600));
        }
        tracing::info!("已生成新的 JWT 密钥: {}", key_file.display());
    }
    key
}

/// 生成 48 字节随机数的 base64url 编码（等价 Python secrets.token_urlsafe(48)）。
fn generate_secret() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut buf);
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(64);
    for chunk in buf.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Config {
    pub base_dir: PathBuf,
    pub app_name: String,
    pub host: String,
    pub port: u16,
    pub debug: bool,

    pub secret_key: String,
    pub token_expire_minutes: i64,
    pub cors_origins: String,

    pub login_max_failures: usize,
    pub login_lock_minutes: i64,

    pub data_dir: PathBuf,
    pub upload_max_mb: usize,

    pub admin_username: String,
    pub admin_password: String,

    // 派生路径
    pub database_path: PathBuf,
    pub uploads_dir: PathBuf,
    pub frontend_dist: PathBuf,
}

impl Config {
    /// 生产配置加载（进程环境 + BASE_DIR/.env + 默认值）。
    pub fn load() -> Config {
        let base = base_dir();
        let _ = dotenvy::from_path(base.join(".env"));

        let data_dir = get_env("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base.join("database"));

        let cfg = Config {
            base_dir: base.clone(),
            app_name: get_env("APP_NAME").unwrap_or_else(|| "NavHub".into()),
            host: get_env("HOST").unwrap_or_else(|| "0.0.0.0".into()),
            port: get_env("PORT").and_then(|v| v.parse().ok()).unwrap_or(8100),
            debug: env_bool("DEBUG").unwrap_or(false),
            secret_key: String::new(),
            token_expire_minutes: get_env("TOKEN_EXPIRE_MINUTES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1440),
            cors_origins: get_env("CORS_ORIGINS").unwrap_or_else(|| "*".into()),
            login_max_failures: get_env("LOGIN_MAX_FAILURES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            login_lock_minutes: get_env("LOGIN_LOCK_MINUTES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(15),
            data_dir: data_dir.clone(),
            upload_max_mb: get_env("UPLOAD_MAX_MB").and_then(|v| v.parse().ok()).unwrap_or(2),
            admin_username: get_env("ADMIN_USERNAME").unwrap_or_else(|| "admin".into()),
            admin_password: get_env("ADMIN_PASSWORD").unwrap_or_else(|| "admin123".into()),
            database_path: data_dir.join("nav.db"),
            uploads_dir: data_dir.join("uploads"),
            frontend_dist: base.join("frontend").join("dist"),
        };

        Self::finalize(cfg)
    }

    /// 测试/程序化构造后的公共收尾：密钥加载与目录创建。
    pub fn finalize(mut cfg: Config) -> Config {
        if cfg.secret_key.is_empty() {
            cfg.secret_key = load_or_create_secret(&cfg.base_dir);
        }
        let _ = std::fs::create_dir_all(&cfg.data_dir);
        let _ = std::fs::create_dir_all(&cfg.uploads_dir);
        cfg
    }
}
