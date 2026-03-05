use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;

use crate::models::{Endpoint, KeyValue, ResponseState};
use crate::request_body::{
    PreparedRequestBody, build_prepared_request_body, computed_default_content_length,
    default_content_type_for_mode, normalize_body_mode, parse_body_fields,
    should_add_default_content_type,
};

use super::core::{resolve_endpoint_url, resolve_placeholders};

pub(crate) fn execute_request(
    endpoint: Endpoint,
    env_vars: BTreeMap<String, String>,
) -> ResponseState {
    let mut output = ResponseState::default();

    let resolved_method = endpoint.method.trim().to_uppercase();
    let method = match Method::from_bytes(resolved_method.as_bytes()) {
        Ok(method) => method,
        Err(err) => {
            output.error = Some(format!(
                "Invalid HTTP method '{}': {err}",
                endpoint.method.trim()
            ));
            return output;
        }
    };

    let resolved_url = resolve_endpoint_url(&endpoint, &env_vars).trim().to_owned();
    if resolved_url.is_empty() {
        output.error = Some("Request URL is empty after placeholder resolution.".to_owned());
        return output;
    }
    let resolved_body = resolve_placeholders(&endpoint.body, &env_vars);
    let body_mode = normalize_body_mode(&endpoint.body_mode);
    let prepared_body = match build_prepared_request_body(body_mode, &resolved_body) {
        Ok(prepared_body) => prepared_body,
        Err(err) => {
            output.error = Some(format!("Failed to prepare request body: {err}"));
            return output;
        }
    };
    let has_request_body = prepared_body.has_body();

    let client = match Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            output.error = Some(format!("Failed to build HTTP client: {err}"));
            return output;
        }
    };

    let mut request_builder = client.request(method.clone(), resolved_url).header(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static("curl/8.0.0"),
    );

    let has_accept_header = endpoint
        .headers
        .iter()
        .any(|header| header.key.trim().eq_ignore_ascii_case("accept"));
    let has_content_length_header = endpoint
        .headers
        .iter()
        .any(|header| header.key.trim().eq_ignore_ascii_case("content-length"));
    let has_content_type_header = endpoint
        .headers
        .iter()
        .any(|header| header.key.trim().eq_ignore_ascii_case("content-type"));
    if !has_accept_header {
        request_builder = request_builder.header(
            HeaderName::from_static("accept"),
            HeaderValue::from_static("*/*"),
        );
    }
    if let Some(content_length) = computed_default_content_length(
        &method,
        has_content_length_header,
        prepared_body.known_content_length(),
        has_request_body,
    ) {
        let content_length = match HeaderValue::from_str(&content_length) {
            Ok(content_length) => content_length,
            Err(err) => {
                output.error = Some(format!("Invalid computed content-length: {err}"));
                return output;
            }
        };
        request_builder =
            request_builder.header(HeaderName::from_static("content-length"), content_length);
    }
    if should_add_default_content_type(has_request_body, has_content_type_header) {
        if let Some(inferred_content_type) =
            default_content_type_for_mode(body_mode, &resolved_body)
        {
            request_builder = request_builder.header(
                HeaderName::from_static("content-type"),
                HeaderValue::from_static(inferred_content_type),
            );
        }
    }

    for header in endpoint.headers {
        let resolved_key = resolve_placeholders(&header.key, &env_vars)
            .trim()
            .to_owned();
        let resolved_value = resolve_placeholders(&header.value, &env_vars)
            .trim()
            .to_owned();
        if resolved_key.is_empty() {
            continue;
        }

        let header_name = match HeaderName::from_bytes(resolved_key.as_bytes()) {
            Ok(header_name) => header_name,
            Err(err) => {
                output.error = Some(format!("Invalid header name '{}': {err}", resolved_key));
                return output;
            }
        };
        let header_value = match HeaderValue::from_str(&resolved_value) {
            Ok(header_value) => header_value,
            Err(err) => {
                output.error = Some(format!(
                    "Invalid header value for '{}': {err}",
                    header_name.as_str()
                ));
                return output;
            }
        };

        request_builder = request_builder.header(header_name, header_value);
    }

    match prepared_body {
        PreparedRequestBody::None => {}
        PreparedRequestBody::Text(body) => {
            request_builder = request_builder.body(body);
        }
        PreparedRequestBody::Binary(body) => {
            request_builder = request_builder.body(body);
        }
        PreparedRequestBody::Multipart(form) => {
            request_builder = request_builder.multipart(form);
        }
    }

    let started = Instant::now();
    let response = match request_builder.send() {
        Ok(response) => response,
        Err(err) => {
            output.error = Some(format!("Request failed: {err}"));
            return output;
        }
    };

    output.duration_ms = Some(started.elapsed().as_millis());
    output.status_code = Some(response.status().as_u16());
    output.status_text = response
        .status()
        .canonical_reason()
        .unwrap_or("Unknown")
        .to_owned();
    output.headers = response
        .headers()
        .iter()
        .map(|(key, value)| KeyValue {
            key: key.as_str().to_owned(),
            value: value.to_str().unwrap_or("<non-utf8>").to_owned(),
        })
        .collect();
    output.body = match response.text() {
        Ok(text) => text,
        Err(err) => format!("Failed to decode response body: {err}"),
    };

    output
}

pub(crate) fn build_curl_command(
    endpoint: &Endpoint,
    env_vars: &BTreeMap<String, String>,
) -> String {
    let resolved_method = endpoint.method.trim().to_uppercase();
    let resolved_url = resolve_endpoint_url(endpoint, env_vars);
    let resolved_body = resolve_placeholders(&endpoint.body, env_vars);
    let body_mode = normalize_body_mode(&endpoint.body_mode);
    let has_content_type_header = endpoint
        .headers
        .iter()
        .any(|header| header.key.trim().eq_ignore_ascii_case("content-type"));
    let has_request_body = build_prepared_request_body(body_mode, &resolved_body)
        .map(|prepared| prepared.has_body())
        .unwrap_or_else(|_| !resolved_body.is_empty());

    let mut lines = vec![
        "curl".to_owned(),
        format!("  --request {}", shell_single_quote(&resolved_method)),
        format!("  --url {}", shell_single_quote(&resolved_url)),
    ];

    if should_add_default_content_type(has_request_body, has_content_type_header) {
        if let Some(content_type) = default_content_type_for_mode(body_mode, &resolved_body) {
            lines.push(format!(
                "  --header {}",
                shell_single_quote(&format!("Content-Type: {content_type}"))
            ));
        }
    }

    for header in &endpoint.headers {
        let resolved_key = resolve_placeholders(&header.key, env_vars);
        if resolved_key.trim().is_empty() {
            continue;
        }
        let resolved_value = resolve_placeholders(&header.value, env_vars);
        let header_pair = format!("{resolved_key}: {resolved_value}");
        lines.push(format!("  --header {}", shell_single_quote(&header_pair)));
    }

    match body_mode {
        "none" => {}
        "form-data" => {
            for (key, value) in parse_body_fields(&resolved_body) {
                if let Some(path) = value.strip_prefix('@') {
                    lines.push(format!(
                        "  --form {}",
                        shell_single_quote(&format!("{key}=@{}", path.trim()))
                    ));
                } else {
                    lines.push(format!(
                        "  --form {}",
                        shell_single_quote(&format!("{key}={value}"))
                    ));
                }
            }
        }
        "binary" => {
            if !resolved_body.is_empty() {
                let trimmed = resolved_body.trim();
                if let Some(path) = trimmed.strip_prefix('@') {
                    lines.push(format!(
                        "  --data-binary @{}",
                        shell_single_quote(path.trim())
                    ));
                } else {
                    lines.push(format!(
                        "  --data-binary {}",
                        shell_single_quote(&resolved_body)
                    ));
                }
            }
        }
        _ => {
            if !resolved_body.is_empty() {
                lines.push(format!(
                    "  --data-raw {}",
                    shell_single_quote(&resolved_body)
                ));
            }
        }
    }

    if lines.len() == 1 {
        lines[0].clone()
    } else {
        format!("{} \\\n{}", lines[0], lines[1..].join(" \\\n"))
    }
}

fn shell_single_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_owned();
    }
    format!("'{}'", input.replace('\'', "'\\''"))
}
