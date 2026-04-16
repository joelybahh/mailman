use std::fs;
use std::io;
use std::path::Path;

use reqwest::Method;
use reqwest::blocking::multipart;

use crate::models::KeyValue;

pub(crate) fn computed_default_content_length(
    method: &Method,
    has_content_length_header: bool,
    known_body_length: Option<usize>,
    has_request_body: bool,
) -> Option<String> {
    if has_content_length_header {
        return None;
    }
    if let Some(known_body_length) = known_body_length {
        return Some(known_body_length.to_string());
    }
    if !has_request_body && should_set_zero_content_length(method) {
        return Some("0".to_owned());
    }
    None
}

pub(crate) fn should_add_default_content_type(
    has_request_body: bool,
    has_content_type_header: bool,
) -> bool {
    has_request_body && !has_content_type_header
}

pub(crate) fn default_content_type_for_mode(body_mode: &str, body: &str) -> Option<&'static str> {
    match body_mode {
        "none" => None,
        "urlencoded" => Some("application/x-www-form-urlencoded"),
        "binary" => Some("application/octet-stream"),
        "form-data" => None,
        _ => Some(infer_default_raw_content_type(body)),
    }
}

pub(crate) fn normalize_body_mode(mode: &str) -> &'static str {
    match mode.trim().to_ascii_lowercase().as_str() {
        "none" => "none",
        "raw" => "raw",
        "urlencoded" | "x-www-form-urlencoded" => "urlencoded",
        "formdata" | "form-data" | "multipart/form-data" => "form-data",
        "binary" | "file" => "binary",
        _ => "raw",
    }
}

pub(crate) fn normalize_body_mode_owned(mode: &str) -> String {
    normalize_body_mode(mode).to_owned()
}

#[derive(Debug)]
pub(crate) enum PreparedRequestBody {
    None,
    Text(String),
    Binary(Vec<u8>),
    Multipart(multipart::Form),
}

impl PreparedRequestBody {
    pub(crate) fn has_body(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn known_content_length(&self) -> Option<usize> {
        match self {
            Self::Text(body) => Some(body.len()),
            Self::Binary(body) => Some(body.len()),
            Self::None | Self::Multipart(_) => None,
        }
    }
}

pub(crate) fn build_prepared_request_body(
    body_mode: &str,
    resolved_body: &str,
) -> io::Result<PreparedRequestBody> {
    match body_mode {
        "none" => Ok(PreparedRequestBody::None),
        "binary" => {
            if resolved_body.is_empty() {
                Ok(PreparedRequestBody::None)
            } else {
                resolve_binary_payload(resolved_body).map(PreparedRequestBody::Binary)
            }
        }
        "form-data" => {
            let fields = parse_body_fields(resolved_body);
            if fields.is_empty() {
                Ok(PreparedRequestBody::None)
            } else {
                let mut form = multipart::Form::new();
                for (key, value) in fields {
                    if let Some(path) = value.strip_prefix('@') {
                        let path = path.trim();
                        let bytes = fs::read(path)?;
                        let file_name = Path::new(path)
                            .file_name()
                            .and_then(|item| item.to_str())
                            .unwrap_or("upload.bin")
                            .to_owned();
                        form = form.part(key, multipart::Part::bytes(bytes).file_name(file_name));
                    } else {
                        form = form.text(key, value);
                    }
                }
                Ok(PreparedRequestBody::Multipart(form))
            }
        }
        _ => {
            if resolved_body.is_empty() {
                Ok(PreparedRequestBody::None)
            } else {
                Ok(PreparedRequestBody::Text(resolved_body.to_owned()))
            }
        }
    }
}

pub(crate) fn parse_body_fields(body: &str) -> Vec<(String, String)> {
    body.split(['\n', '&'])
        .filter_map(|raw| {
            let item = raw.trim();
            if item.is_empty() {
                return None;
            }
            let (key, value) = item.split_once('=').unwrap_or((item, ""));
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

pub(crate) fn serialize_body_fields(fields: &[KeyValue], separator: &str) -> String {
    fields
        .iter()
        .filter_map(|field| {
            let key = field.key.trim();
            if key.is_empty() {
                return None;
            }

            let value = field.value.trim();
            Some(if value.is_empty() {
                key.to_owned()
            } else {
                format!("{key}={value}")
            })
        })
        .collect::<Vec<_>>()
        .join(separator)
}

fn should_set_zero_content_length(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::PATCH)
}

fn infer_default_raw_content_type(body: &str) -> &'static str {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "application/json"
    } else {
        "text/plain"
    }
}

fn resolve_binary_payload(body: &str) -> io::Result<Vec<u8>> {
    let trimmed = body.trim();
    if let Some(path) = trimmed.strip_prefix('@') {
        let path = path.trim();
        if path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Binary body uses '@' but no file path was provided.",
            ));
        }
        return fs::read(path);
    }

    Ok(body.as_bytes().to_vec())
}
