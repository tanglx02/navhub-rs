//! 请求体边界校验：手动解析 JSON，复刻 pydantic v2 的校验语义与 422 错误格式。
//!
//! 关键语义对齐：
//! - 必填字段缺失 → `missing`；显式 null 而类型不允许 → `*_type` 错误
//! - 更新模型（AppUpdate 等）：区分「未传（不更新）/ 传 null（清空）/ 传值（更新）」
//! - 所有字段的错误一次性收集返回（与 pydantic 行为一致）

use std::sync::OnceLock;

use regex::Regex;
use rusqlite::types::Value as SqlValue;
use serde_json::{Map, Value};

use crate::errors::{ApiError, ApiResult, ValErr};

pub const ICON_TYPES: &str = "'builtin', 'upload', 'url' or 'none'";

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)^https?://[^\s<>"]+$"#).expect("URL 正则编译失败"))
}

fn color_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^#[0-9a-fA-F]{8}$").expect("颜色正则编译失败"))
}

// ---------- 动态列值对（用于 PUT 部分更新，对齐 model_dump(exclude_unset=True)） ----------

/// (列名, SQL 值) 有序对，仅包含请求体中显式出现的字段。
pub type Pairs = Vec<(&'static str, SqlValue)>;

/// 解析请求体为 JSON 对象（解析失败 / 非对象 → 422）。
pub fn parse_body(bytes: &[u8]) -> ApiResult<Map<String, Value>> {
    if bytes.is_empty() {
        return Err(ApiError::unprocessable(vec![ValErr::json_invalid("expecting value: line 1 column 1 (char 0)")]));
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(obj)) => Ok(obj),
        Ok(_) => Err(ApiError::unprocessable(vec![ValErr::json_invalid("was expecting an object")])),
        Err(e) => Err(ApiError::unprocessable(vec![ValErr::json_invalid(e)])),
    }
}

fn finish<T>(obj: Map<String, Value>, build: impl FnOnce(&Map<String, Value>) -> (T, Vec<ValErr>)) -> ApiResult<T> {
    let (val, errs) = build(&obj);
    if errs.is_empty() {
        Ok(val)
    } else {
        Err(ApiError::unprocessable(errs))
    }
}

// ---------- 基础字段提取 ----------

/// 字符串字段：`min`/`max` 按字符数校验（对齐 Python len(str)）。
fn str_field(obj: &Map<String, Value>, f: &'static str, min: usize, max: usize, errs: &mut Vec<ValErr>) -> Option<String> {
    match obj.get(f) {
        None => None,
        Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let len = s.chars().count();
            if len < min {
                errs.push(ValErr::too_short(f, min).with_input(Value::String(s.clone())));
            } else if len > max {
                errs.push(ValErr::too_long(f, max).with_input(Value::String(s.clone())));
            } else {
                return Some(s.clone());
            }
            None
        }
        Some(other) => {
            errs.push(ValErr::string_type(f).with_input(other.clone()));
            None
        }
    }
}

/// 必填字符串（缺失 → missing；null → string_type）。
fn req_str(obj: &Map<String, Value>, f: &'static str, min: usize, max: usize, errs: &mut Vec<ValErr>) -> String {
    match obj.get(f) {
        None => {
            errs.push(ValErr::missing(f));
            String::new()
        }
        Some(_) => str_field(obj, f, min, max, errs).unwrap_or_default(),
    }
}

fn int_field(obj: &Map<String, Value>, f: &'static str, errs: &mut Vec<ValErr>) -> Option<i64> {
    match obj.get(f) {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(v) => Some(v),
            None => {
                errs.push(ValErr::int_type(f).with_input(Value::Number(n.clone())));
                None
            }
        },
        Some(Value::String(s)) => match s.trim().parse::<i64>() {
            Ok(v) => Some(v),
            Err(_) => {
                errs.push(ValErr::int_parsing("body", f).with_input(Value::String(s.clone())));
                None
            }
        },
        Some(other) => {
            errs.push(ValErr::int_type(f).with_input(other.clone()));
            None
        }
    }
}

/// 带默认值的 int（`sort_order: int = 0`：缺失 → 默认；null → int_type 错误）。
fn defaulted_int(obj: &Map<String, Value>, f: &'static str, default: i64, errs: &mut Vec<ValErr>) -> i64 {
    match obj.get(f) {
        None => default,
        Some(Value::Null) => {
            errs.push(ValErr::int_type(f).with_input(Value::Null));
            default
        }
        Some(_) => int_field(obj, f, errs).unwrap_or(default),
    }
}

fn truthy_str(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "1" | "on" | "yes" | "y" | "t" => Some(true),
        "false" | "0" | "off" | "no" | "n" | "f" => Some(false),
        _ => None,
    }
}

fn bool_field(obj: &Map<String, Value>, f: &'static str, errs: &mut Vec<ValErr>) -> Option<bool> {
    match obj.get(f) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => match truthy_str(s) {
            Some(v) => Some(v),
            None => {
                errs.push(ValErr::bool_parsing("body", f).with_input(Value::String(s.clone())));
                None
            }
        },
        Some(other) => {
            errs.push(ValErr::bool_type(f).with_input(other.clone()));
            None
        }
    }
}

/// 带默认值的 bool（`status: bool = True`：缺失 → 默认；null → bool_type 错误）。
fn defaulted_bool(obj: &Map<String, Value>, f: &'static str, default: bool, errs: &mut Vec<ValErr>) -> bool {
    match obj.get(f) {
        None => default,
        Some(Value::Null) => {
            errs.push(ValErr::bool_type(f).with_input(Value::Null));
            default
        }
        Some(_) => bool_field(obj, f, errs).unwrap_or(default),
    }
}

/// Literal 枚举字符串字段。
fn literal_field(obj: &Map<String, Value>, f: &'static str, errs: &mut Vec<ValErr>) -> Option<String> {
    match obj.get(f) {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if matches!(s.as_str(), "builtin" | "upload" | "url" | "none") =>
        {
            Some(s.clone())
        }
        Some(other) => {
            errs.push(ValErr::literal_error(f, ICON_TYPES).with_input(other.clone()));
            None
        }
    }
}

/// URL 字段业务校验（strip 后必须匹配 http/https 正则）。
fn checked_url(val: Option<String>, f: &'static str, errs: &mut Vec<ValErr>) -> Option<String> {
    let v = val?.trim().to_string();
    if url_re().is_match(&v) {
        Some(v)
    } else {
        errs.push(ValErr::value_error(f, "URL 必须以 http:// 或 https:// 开头且合法").with_input(Value::String(v)));
        None
    }
}

/// 颜色字段：None/空串 → None（清空）；否则必须 #RRGGBBAA。
fn checked_color(val: Option<String>, f: &'static str, errs: &mut Vec<ValErr>) -> Option<String> {
    let v = match val {
        None => return None,
        Some(s) if s.is_empty() => return None,
        Some(s) => s,
    };
    if color_re().is_match(&v) {
        Some(v)
    } else {
        errs.push(ValErr::value_error(f, "颜色格式必须为 #RRGGBBAA").with_input(Value::String(v)));
        None
    }
}

// ---------- 具体模型 ----------

#[derive(Debug, Clone)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

pub fn login_request(bytes: &[u8]) -> ApiResult<LoginRequest> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut errs = Vec::new();
        let req = LoginRequest {
            username: req_str(o, "username", 1, 50, &mut errs),
            password: req_str(o, "password", 1, 128, &mut errs),
        };
        (req, errs)
    })
}

#[derive(Debug, Clone)]
pub struct PasswordChange {
    pub old_password: String,
    pub new_password: String,
}

pub fn password_change(bytes: &[u8]) -> ApiResult<PasswordChange> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut errs = Vec::new();
        let req = PasswordChange {
            old_password: req_str(o, "old_password", 1, 128, &mut errs),
            new_password: req_str(o, "new_password", 6, 128, &mut errs),
        };
        (req, errs)
    })
}

#[derive(Debug, Clone)]
pub struct AppCreate {
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub icon_type: String,
    pub icon_value: Option<String>,
    pub color: Option<String>,
    pub category_id: Option<i64>,
    pub sort_order: i64,
    pub status: bool,
}

pub fn app_create(bytes: &[u8]) -> ApiResult<AppCreate> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut e = Vec::new();
        let v = AppCreate {
            name: req_str(o, "name", 1, 100, &mut e),
            url: checked_url(Some(req_str(o, "url", 1, 2048, &mut e)), "url", &mut e)
                .unwrap_or_default(),
            description: str_field(o, "description", 0, 500, &mut e),
            icon_type: literal_present(o, "icon_type", &mut e),
            icon_value: str_field(o, "icon_value", 0, 2048, &mut e),
            color: checked_color(str_field(o, "color", 0, 9, &mut e), "color", &mut e),
            category_id: int_field(o, "category_id", &mut e),
            sort_order: defaulted_int(o, "sort_order", 0, &mut e),
            status: defaulted_bool(o, "status", true, &mut e),
        };
        (v, e)
    })
}

/// `icon_type: Literal[...] = "none"`：缺失 → "none"；null → literal_error（非 Optional）。
fn literal_present(o: &Map<String, Value>, f: &'static str, errs: &mut Vec<ValErr>) -> String {
    match o.get(f) {
        None => "none".to_string(),
        Some(Value::Null) => {
            errs.push(ValErr::literal_error(f, ICON_TYPES).with_input(Value::Null));
            "none".to_string()
        }
        Some(_) => literal_field(o, f, errs).unwrap_or_else(|| "none".to_string()),
    }
}

/// AppUpdate：返回需要写入的 (列, 值) 对（缺失字段不出现在结果中）。
pub fn app_update(bytes: &[u8]) -> ApiResult<Pairs> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut e = Vec::new();
        let mut pairs: Pairs = Vec::new();
        macro_rules! str_opt {
            ($f:expr, $min:expr, $max:expr) => {
                if o.contains_key($f) {
                    pairs.push(($f, opt_text(o, $f, $min, $max, &mut e)));
                }
            };
        }
        str_opt!("name", 1, 100);
        if o.contains_key("url") {
            let raw = str_field(o, "url", 1, 2048, &mut e);
            let v = checked_url(raw, "url", &mut e);
            pairs.push(("url", text_or_null(v)));
        }
        str_opt!("description", 0, 500);
        if o.contains_key("icon_type") {
            pairs.push(("icon_type", text_or_null(literal_field(o, "icon_type", &mut e))));
        }
        str_opt!("icon_value", 0, 2048);
        if o.contains_key("color") {
            let raw = str_field(o, "color", 0, 9, &mut e);
            let v = checked_color(raw, "color", &mut e);
            pairs.push(("color", text_or_null(v)));
        }
        if o.contains_key("category_id") {
            pairs.push(("category_id", match int_field(o, "category_id", &mut e) {
                Some(v) => SqlValue::Integer(v),
                None => SqlValue::Null,
            }));
        }
        if o.contains_key("sort_order") {
            pairs.push(("sort_order", match int_field(o, "sort_order", &mut e) {
                Some(v) => SqlValue::Integer(v),
                None => SqlValue::Null,
            }));
        }
        if o.contains_key("status") {
            pairs.push(("status", match bool_field(o, "status", &mut e) {
                Some(v) => SqlValue::Integer(i64::from(v)),
                None => SqlValue::Null,
            }));
        }
        (pairs, e)
    })
}

fn text_or_null(v: Option<String>) -> SqlValue {
    match v {
        Some(s) => SqlValue::Text(s),
        None => SqlValue::Null,
    }
}

fn opt_text(o: &Map<String, Value>, f: &'static str, min: usize, max: usize, errs: &mut Vec<ValErr>) -> SqlValue {
    if matches!(o.get(f), Some(Value::Null)) {
        return SqlValue::Null;
    }
    text_or_null(str_field(o, f, min, max, errs))
}

#[derive(Debug, Clone)]
pub struct CategoryCreate {
    pub name: String,
    pub icon: Option<String>,
    pub sort_order: i64,
}

pub fn category_create(bytes: &[u8]) -> ApiResult<CategoryCreate> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut e = Vec::new();
        let v = CategoryCreate {
            name: req_str(o, "name", 1, 50, &mut e),
            icon: str_field(o, "icon", 0, 64, &mut e),
            sort_order: defaulted_int(o, "sort_order", 0, &mut e),
        };
        (v, e)
    })
}

pub fn category_update(bytes: &[u8]) -> ApiResult<Pairs> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut e = Vec::new();
        let mut pairs: Pairs = Vec::new();
        if o.contains_key("name") {
            pairs.push(("name", opt_text(o, "name", 1, 50, &mut e)));
        }
        if o.contains_key("icon") {
            pairs.push(("icon", opt_text(o, "icon", 0, 64, &mut e)));
        }
        if o.contains_key("sort_order") {
            pairs.push(("sort_order", match int_field(o, "sort_order", &mut e) {
                Some(v) => SqlValue::Integer(v),
                None => SqlValue::Null,
            }));
        }
        (pairs, e)
    })
}

/// SettingsUpdate：exclude_unset + exclude_none → 仅收集显式传且非 null 的键。
pub fn settings_update(bytes: &[u8]) -> ApiResult<Vec<(&'static str, String)>> {
    let obj = parse_body(bytes)?;
    finish(obj, |o| {
        let mut e = Vec::new();
        let mut out: Vec<(&'static str, String)> = Vec::new();
        if let Some(v) = str_field(o, "site_name", 1, 50, &mut e) {
            out.push(("site_name", v));
        }
        if let Some(v) = str_field(o, "site_description", 0, 200, &mut e) {
            out.push(("site_description", v));
        }
        (out, e)
    })
}

/// 查询参数布尔解析（对齐 FastAPI bool 转换：true/false/1/0/on/off/yes/no/t/f/y/n）。
pub fn query_bool(raw: &str) -> Option<bool> {
    truthy_str(raw)
}
