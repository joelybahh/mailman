use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use directories::BaseDirs;
use eframe::egui::Color32;

use crate::models::*;
use crate::request_body::normalize_body_mode;

pub(crate) fn default_endpoints() -> Vec<Endpoint> {
    vec![
        Endpoint::with_defaults(
            "ep-health".to_owned(),
            "Health Check",
            "GET",
            "https://${api_host}/health",
        ),
        Endpoint {
            id: "ep-login".to_owned(),
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: "Login".to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: "POST".to_owned(),
            url: "https://${api_host}/api/v1/login".to_owned(),
            query_params: vec![],
            headers: vec![KeyValue {
                key: "Content-Type".to_owned(),
                value: "application/json".to_owned(),
            }],
            body_mode: "raw".to_owned(),
            body: "{\n  \"email\": \"${email}\",\n  \"password\": \"${password}\"\n}".to_owned(),
        },
    ]
}

pub(crate) fn default_environments() -> Vec<Environment> {
    vec![
        Environment {
            id: "env-dev".to_owned(),
            name: "dev".to_owned(),
            file_name: "env-dev.json".to_owned(),
            variables: vec![
                KeyValue {
                    key: "api_host".to_owned(),
                    value: "localhost:8080".to_owned(),
                },
                KeyValue {
                    key: "email".to_owned(),
                    value: "dev@example.com".to_owned(),
                },
                KeyValue {
                    key: "password".to_owned(),
                    value: "dev-password".to_owned(),
                },
            ],
        },
        Environment {
            id: "env-staging".to_owned(),
            name: "staging".to_owned(),
            file_name: "env-staging.json".to_owned(),
            variables: vec![
                KeyValue {
                    key: "api_host".to_owned(),
                    value: "staging-api.example.com".to_owned(),
                },
                KeyValue {
                    key: "email".to_owned(),
                    value: "staging@example.com".to_owned(),
                },
                KeyValue {
                    key: "password".to_owned(),
                    value: "staging-password".to_owned(),
                },
            ],
        },
        Environment {
            id: "env-prod".to_owned(),
            name: "prod".to_owned(),
            file_name: "env-prod.json".to_owned(),
            variables: vec![
                KeyValue {
                    key: "api_host".to_owned(),
                    value: "api.example.com".to_owned(),
                },
                KeyValue {
                    key: "email".to_owned(),
                    value: "ops@example.com".to_owned(),
                },
                KeyValue {
                    key: "password".to_owned(),
                    value: "prod-password".to_owned(),
                },
            ],
        },
    ]
}

pub(crate) fn default_environment_index() -> Vec<EnvironmentIndexEntry> {
    default_environments()
        .into_iter()
        .map(|env| EnvironmentIndexEntry {
            id: env.id,
            name: env.name,
            file_name: env.file_name,
        })
        .collect()
}

pub(crate) fn default_variables_for_environment_name(name: &str) -> Vec<KeyValue> {
    default_environments()
        .into_iter()
        .find(|env| env.name.eq_ignore_ascii_case(name))
        .map(|env| env.variables)
        .unwrap_or_else(Vec::new)
}
pub(crate) fn resolve_placeholders(template: &str, env_vars: &BTreeMap<String, String>) -> String {
    PLACEHOLDER_PATTERN
        .replace_all(template, |captures: &regex::Captures<'_>| {
            let key = captures
                .get(1)
                .map(|item| item.as_str())
                .unwrap_or_default();
            env_vars
                .get(key)
                .cloned()
                .unwrap_or_else(|| captures[0].to_owned())
        })
        .to_string()
}

pub(crate) fn resolve_endpoint_url(
    endpoint: &Endpoint,
    env_vars: &BTreeMap<String, String>,
) -> String {
    let base_url = resolve_placeholders(&endpoint.url, env_vars)
        .trim()
        .to_owned();
    if endpoint.query_params.is_empty() {
        return base_url;
    }

    let mut pairs = vec![];
    for param in &endpoint.query_params {
        let key = resolve_placeholders(&param.key, env_vars).trim().to_owned();
        if key.is_empty() {
            continue;
        }
        let value = resolve_placeholders(&param.value, env_vars)
            .trim()
            .to_owned();
        if value.is_empty() {
            pairs.push(key);
        } else {
            pairs.push(format!("{key}={value}"));
        }
    }

    append_query_pairs_to_url(base_url, &pairs)
}

pub(crate) fn normalize_endpoint_url_and_query_params(endpoint: &mut Endpoint) {
    endpoint
        .query_params
        .retain(|param| !param.key.trim().is_empty() || !param.value.trim().is_empty());
    if !endpoint.query_params.is_empty() {
        return;
    }

    let (base_url, query_params) = split_url_and_query_params(&endpoint.url);
    if query_params.is_empty() {
        return;
    }
    endpoint.url = base_url;
    endpoint.query_params = query_params;
}

pub(crate) fn normalize_postman_placeholders(input: &str) -> String {
    POSTMAN_PLACEHOLDER_PATTERN
        .replace_all(input, |captures: &regex::Captures<'_>| {
            let key = captures
                .get(1)
                .map(|item| item.as_str().trim())
                .unwrap_or_default();
            format!("${{{key}}}")
        })
        .to_string()
}

pub(crate) fn endpoint_dedup_key(endpoint: &Endpoint) -> String {
    if let Some(source_id) = non_empty_trimmed(&endpoint.source_request_id) {
        return format!("source-id|{}", source_id.to_ascii_lowercase());
    }

    let query_signature = endpoint
        .query_params
        .iter()
        .filter_map(|param| {
            let key = param.key.trim().to_ascii_lowercase();
            if key.is_empty() {
                return None;
            }
            Some(format!(
                "{}={}",
                key,
                param.value.trim().to_ascii_lowercase()
            ))
        })
        .collect::<Vec<_>>()
        .join("&");

    format!(
        "{}|{}|{}|{}|{}|{}",
        endpoint.method.trim().to_uppercase(),
        endpoint.url.trim().to_lowercase(),
        endpoint.collection.trim().to_lowercase(),
        endpoint.folder_path.trim().to_lowercase(),
        endpoint.name.trim().to_lowercase(),
        query_signature
    )
}

pub(crate) fn merge_endpoint_details(existing: &mut Endpoint, incoming: Endpoint) -> bool {
    let mut changed = false;
    let incoming_body_mode = normalize_body_mode(&incoming.body_mode).to_owned();

    if existing.source_collection_id.trim().is_empty()
        && !incoming.source_collection_id.trim().is_empty()
    {
        existing.source_collection_id = incoming.source_collection_id.clone();
        changed = true;
    }
    if existing.source_folder_id.trim().is_empty() && !incoming.source_folder_id.trim().is_empty() {
        existing.source_folder_id = incoming.source_folder_id.clone();
        changed = true;
    }

    if (existing.collection.trim().is_empty() || existing.collection.trim() == "General")
        && !incoming.collection.trim().is_empty()
        && incoming.collection.trim() != "General"
    {
        existing.collection = incoming.collection.clone();
        changed = true;
    }

    if existing.folder_path.trim().is_empty() && !incoming.folder_path.trim().is_empty() {
        existing.folder_path = incoming.folder_path.clone();
        changed = true;
    }

    if existing.headers.is_empty() && !incoming.headers.is_empty() {
        existing.headers = incoming.headers;
        changed = true;
    } else if !incoming.headers.is_empty() {
        let mut known_keys = existing
            .headers
            .iter()
            .map(|header| header.key.trim().to_ascii_lowercase())
            .collect::<BTreeSet<_>>();

        for header in incoming.headers {
            let key = header.key.trim();
            if key.is_empty() {
                continue;
            }
            let lower = key.to_ascii_lowercase();
            if known_keys.contains(&lower) {
                continue;
            }
            known_keys.insert(lower);
            existing.headers.push(header);
            changed = true;
        }
    }

    if existing.query_params.is_empty() && !incoming.query_params.is_empty() {
        existing.query_params = incoming.query_params;
        changed = true;
    }

    if existing.body.trim().is_empty() && !incoming.body.trim().is_empty() {
        existing.body = incoming.body;
        changed = true;
    }
    let existing_body_mode = normalize_body_mode(&existing.body_mode);
    if (existing.body_mode.trim().is_empty()
        || existing_body_mode == "none"
        || existing_body_mode == "raw")
        && incoming_body_mode != "none"
        && incoming_body_mode != existing_body_mode
    {
        existing.body_mode = incoming_body_mode;
        changed = true;
    }

    if existing.url != incoming.url
        && incoming.url.contains('?')
        && (!existing.url.contains('?') || incoming.url.len() > existing.url.len())
    {
        existing.url = incoming.url;
        changed = true;
    }

    changed
}

fn split_url_and_query_params(url: &str) -> (String, Vec<KeyValue>) {
    let Some(query_start) = url.find('?') else {
        return (url.to_owned(), vec![]);
    };

    let mut base = url[..query_start].to_owned();
    let mut query_and_fragment = url[query_start + 1..].to_owned();
    let mut fragment = String::new();
    if let Some(hash_index) = query_and_fragment.find('#') {
        fragment = query_and_fragment[hash_index..].to_owned();
        query_and_fragment.truncate(hash_index);
    }

    let mut query_params = vec![];
    for item in query_and_fragment.split('&') {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (key, value) = if let Some(eq_index) = trimmed.find('=') {
            (&trimmed[..eq_index], &trimmed[eq_index + 1..])
        } else {
            (trimmed, "")
        };

        query_params.push(KeyValue {
            key: key.trim().to_owned(),
            value: value.trim().to_owned(),
        });
    }

    if !fragment.is_empty() {
        base.push_str(&fragment);
    }

    (base, query_params)
}

fn append_query_pairs_to_url(mut url: String, query_pairs: &[String]) -> String {
    if query_pairs.is_empty() {
        return url;
    }

    let mut fragment = String::new();
    if let Some(hash_index) = url.find('#') {
        fragment = url[hash_index..].to_owned();
        url.truncate(hash_index);
    }

    if !url.contains('?') {
        url.push('?');
    } else if !url.ends_with('?') && !url.ends_with('&') {
        url.push('&');
    }
    url.push_str(&query_pairs.join("&"));

    if !fragment.is_empty() {
        url.push_str(&fragment);
    }

    url
}
pub(crate) fn non_empty_trimmed(input: &str) -> Option<&str> {
    let value = input.trim();
    if value.is_empty() { None } else { Some(value) }
}

pub(crate) fn split_folder_path(input: &str) -> Vec<&str> {
    input
        .split(&['/', '\\'][..])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

pub(crate) fn safe_path_segment(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            output.push(ch);
        } else {
            output.push('_');
        }
    }

    let collapsed = output.trim_matches('.').trim_matches('_').trim().to_owned();
    if collapsed.is_empty() {
        "untitled".to_owned()
    } else {
        collapsed
    }
}

pub(crate) fn normalize_folder_path(input: &str) -> String {
    split_folder_path(input).join(" / ")
}

pub(crate) fn default_postman_directories() -> Vec<PathBuf> {
    let mut output = vec![];

    if let Some(base_dirs) = BaseDirs::new() {
        let home = base_dirs.home_dir();

        #[cfg(target_os = "macos")]
        {
            output.push(home.join("Library/Application Support/Postman"));
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(appdata) = std::env::var_os("APPDATA") {
                output.push(PathBuf::from(appdata).join("Postman"));
            }
            output.push(base_dirs.data_dir().join("Postman"));
        }

        #[cfg(target_os = "linux")]
        {
            output.push(home.join(".config/Postman"));
            output.push(home.join(".var/app/com.getpostman.Postman/config/Postman"));
            output.push(home.join("snap/postman/current/.config/Postman"));
        }
    }

    output.into_iter().filter(|path| path.exists()).collect()
}

pub(crate) fn create_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{prefix}-{}-{}", now.as_secs(), now.subsec_nanos())
}

pub(crate) fn method_color(method: &str) -> Color32 {
    match method {
        "GET" => Color32::from_rgb(97, 175, 239),
        "POST" => Color32::from_rgb(152, 195, 121),
        "PUT" => Color32::from_rgb(229, 192, 123),
        "PATCH" => Color32::from_rgb(198, 120, 221),
        "DELETE" => Color32::from_rgb(224, 108, 117),
        _ => Color32::from_rgb(171, 178, 191),
    }
}
