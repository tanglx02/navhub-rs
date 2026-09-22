//! 图标上传处理器（对齐 Python upload.py：双重校验 + 魔数 + 随机文件名）。

use std::io::Write as _;

use axum::extract::{Multipart, State};
use axum::Json;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::errors::{ApiError, ApiResult, ValErr};
use crate::state::AppState;

const ALLOWED_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "ico"];
const ALLOWED_CONTENT_TYPES: [&str; 8] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
    "image/x-icon",
    "image/vnd.microsoft.icon",
    "application/octet-stream",
];

pub async fn upload_icon(
    State(state): State<AppState>,
    _user: AuthUser,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    let max_bytes = state.cfg.upload_max_mb * 1024 * 1024;
    let field = loop {
        let next = mp
            .next_field()
            .await
            .map_err(|_| ApiError::bad_request("文件解析失败，请检查上传格式"))?;
        match next {
            Some(f) if f.name() == Some("file") => break f,
            Some(_) => continue,
            None => return Err(ApiError::unprocessable_one(ValErr::missing("file"))),
        }
    };

    let original_name = field
        .file_name()
        .map(|s| s.to_string())
        .ok_or_else(|| ApiError::bad_request("缺少文件"))?;

    // Content-Type 校验
    let content_type_ok = field
        .content_type()
        .is_some_and(|ct| ALLOWED_CONTENT_TYPES.contains(&ct));
    if !content_type_ok {
        return Err(ApiError::bad_request("不支持的文件类型"));
    }

    // 扩展名白名单（决定存储后缀）
    let suffix = match original_name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ALLOWED_EXTENSIONS.contains(&ext.to_lowercase().as_str()) => {
            format!(".{}", ext.to_lowercase())
        }
        _ => return Err(ApiError::bad_request("仅支持图片格式：png/jpg/jpeg/gif/webp/svg/ico")),
    };

    // 大小限制：读取整个字段（框架限额已在路由上放大到业务上限+128KB，
    // 超限错误在此统一映射为 413，与原 Python 版响应一致）
    let too_large = || {
        ApiError::payload_too_large(format!(
            "文件大小不能超过 {}MB",
            state.cfg.upload_max_mb
        ))
    };
    let content = match field.bytes().await {
        Ok(b) if b.len() > max_bytes => return Err(too_large()),
        Ok(b) => b,
        Err(_) => return Err(too_large()),
    };
    if content.is_empty() {
        return Err(ApiError::bad_request("文件内容为空"));
    }

    // 魔数校验，防止伪装成图片的恶意脚本
    check_magic_bytes(&content, &suffix)?;

    let filename = format!("{}{suffix}", uuid::Uuid::new_v4().simple());
    let dest = state.cfg.uploads_dir.join(&filename);
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&dest)?;
        f.write_all(&content)?;
        f.flush()
    })
    .await
    .map_err(|_| ApiError::message(axum::http::StatusCode::INTERNAL_SERVER_ERROR, "文件保存失败"))?
    .map_err(|_| ApiError::message(axum::http::StatusCode::INTERNAL_SERVER_ERROR, "文件保存失败"))?;

    Ok(Json(json!({ "url": format!("/uploads/{filename}"), "filename": filename })))
}

/// 基础魔数校验；SVG 允许纯文本，其余格式校验文件头。
fn check_magic_bytes(content: &[u8], suffix: &str) -> ApiResult<()> {
    let sig: &[u8] = match suffix {
        ".png" => b"\x89PNG",
        ".gif" => b"GIF8",
        ".webp" => b"RIFF",
        // jpg/jpeg / ico / svg 不做强校验（ico 头部变体多、svg 为文本），仍受白名单约束
        _ => return Ok(()),
    };
    if content.starts_with(sig) {
        Ok(())
    } else {
        Err(ApiError::bad_request("文件内容与格式不符"))
    }
}
