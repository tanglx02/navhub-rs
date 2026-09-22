//! 统一 API 错误：响应体固定为 `{"detail": ...}`，与前端错误解析契约对齐。
//!
//! 422 校验错误复刻 pydantic v2 的 `{"detail": [{loc, msg, type, input}]}` 数组格式，
//! 前端按 `data.detail[0].msg` 提取展示。

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

/// 单条字段校验错误（对齐 pydantic v2 错误项）。
#[derive(Debug, Clone)]
pub struct ValErr {
    pub loc: Vec<&'static str>,
    pub msg: String,
    pub typ: &'static str,
    pub input: Option<Value>,
}

impl ValErr {
    fn new(typ: &'static str, loc: Vec<&'static str>, msg: impl Into<String>) -> Self {
        Self { loc, msg: msg.into(), typ, input: None }
    }

    pub fn with_input(mut self, v: Value) -> Self {
        self.input = Some(v);
        self
    }

    /// 必填字段缺失。
    pub fn missing(field: &'static str) -> Self {
        Self::new("missing", vec!["body", field], "Field required")
    }

    /// 字段应为字符串但收到 null / 非字符串。
    pub fn string_type(field: &'static str) -> Self {
        Self::new("string_type", vec!["body", field], "Input should be a valid string")
    }

    pub fn too_short(field: &'static str, min: usize) -> Self {
        let unit = if min == 1 { "character" } else { "characters" };
        Self::new(
            "string_too_short",
            vec!["body", field],
            format!("String should have at least {min} {unit}"),
        )
    }

    pub fn too_long(field: &'static str, max: usize) -> Self {
        Self::new(
            "string_too_long",
            vec!["body", field],
            format!("String should have at most {max} characters"),
        )
    }

    /// 自定义 ValueError（URL / 颜色等业务校验）。
    pub fn value_error(field: &'static str, msg: impl Into<String>) -> Self {
        Self::new("value_error", vec!["body", field], format!("Value error, {}", msg.into()))
    }

    pub fn literal_error(field: &'static str, expected: &str) -> Self {
        Self::new("literal_error", vec!["body", field], format!("Input should be {expected}"))
    }

    /// 查询/路径参数缺失。
    pub fn param_missing(at: &'static str, name: &'static str) -> Self {
        Self::new("missing", vec![at, name], "Field required")
    }

    pub fn int_parsing(at: &'static str, name: &'static str) -> Self {
        Self::new(
            "int_parsing",
            vec![at, name],
            "Input should be a valid integer, unable to parse string as an integer",
        )
    }

    /// 应为整数但收到 null（创建场景的必填 int）。
    pub fn int_type(field: &'static str) -> Self {
        Self::new("int_type", vec!["body", field], "Input should be a valid integer")
    }

    /// 应为布尔但收到 null（创建场景的必填 bool）。
    pub fn bool_type(field: &'static str) -> Self {
        Self::new("bool_type", vec!["body", field], "Input should be a valid boolean")
    }

    pub fn bool_parsing(at: &'static str, name: &'static str) -> Self {
        Self::new(
            "bool_parsing",
            vec![at, name],
            "Input should be a valid boolean, unable to interpret input",
        )
    }

    pub fn json_invalid(detail: impl std::fmt::Display) -> Self {
        Self::new("json_invalid", vec!["body"], format!("JSON decode error: {detail}"))
    }
}

/// API 错误（业务层统一抛出）。
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Value,
    www_auth: bool,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    fn new(status: StatusCode, body: Value) -> Self {
        Self { status, body, www_auth: false }
    }

    /// 通用 `{"detail": "..."}` 错误。
    pub fn message(status: StatusCode, detail: impl Into<String>) -> Self {
        Self::new(status, json!({ "detail": detail.into() }))
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        let mut e = Self::new(StatusCode::UNAUTHORIZED, json!({ "detail": detail.into() }));
        e.www_auth = true;
        e
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::message(StatusCode::BAD_REQUEST, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::message(StatusCode::NOT_FOUND, detail)
    }

    pub fn too_many_requests(detail: impl Into<String>) -> Self {
        Self::message(StatusCode::TOO_MANY_REQUESTS, detail)
    }

    pub fn payload_too_large(detail: impl Into<String>) -> Self {
        Self::message(StatusCode::PAYLOAD_TOO_LARGE, detail)
    }

    /// 422 校验错误（数组格式）。
    pub fn unprocessable(errors: Vec<ValErr>) -> Self {
        let items: Vec<Value> = errors
            .iter()
            .map(|e| {
                let mut obj = json!({
                    "type": e.typ,
                    "loc": e.loc,
                    "msg": e.msg,
                });
                if let Some(input) = &e.input {
                    obj["input"] = input.clone();
                }
                obj
            })
            .collect();
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, json!({ "detail": items }))
    }

    /// 单条 422 快捷方式。
    pub fn unprocessable_one(err: ValErr) -> Self {
        Self::unprocessable(vec![err])
    }

    /// 判断是否为 SQLite 唯一/外键约束冲突。
    pub fn is_constraint(err: &rusqlite::Error) -> bool {
        matches!(
            err,
            rusqlite::Error::SqliteFailure(e, _)
                if e.code == rusqlite::ErrorCode::ConstraintViolation
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = (self.status, axum::Json(self.body)).into_response();
        if self.www_auth {
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("数据库错误: {e}");
        Self::message(StatusCode::INTERNAL_SERVER_ERROR, "服务器内部错误")
    }
}
