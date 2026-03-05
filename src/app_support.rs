use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use directories::BaseDirs;
use eframe::egui::Color32;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rand::RngCore;
use rand::rngs::OsRng;
use regex::Regex;
use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use rusty_leveldb::{DB as LevelDb, LdbIterator, Options as LevelDbOptions};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::models::*;
use crate::request_body::{
    PreparedRequestBody, build_prepared_request_body, computed_default_content_length,
    default_content_type_for_mode, normalize_body_mode, normalize_body_mode_owned,
    parse_body_fields, should_add_default_content_type,
};

#[derive(Default, Debug)]
pub(crate) struct ImportSummary {
    pub(crate) directories_scanned: usize,
    pub(crate) files_scanned: usize,
    pub(crate) endpoints_added: usize,
    pub(crate) endpoints_updated: usize,
    pub(crate) environments_added: usize,
    pub(crate) environment_variables_merged: usize,
}

impl ImportSummary {
    pub(crate) fn to_message(&self) -> String {
        format!(
            "Postman import: scanned {} dirs / {} files, added {} endpoints, updated {} existing endpoints, added {} environments, merged {} environment vars.",
            self.directories_scanned,
            self.files_scanned,
            self.endpoints_added,
            self.endpoints_updated,
            self.environments_added,
            self.environment_variables_merged,
        )
    }
}

#[derive(Default, Debug)]
pub(crate) struct ImportScanResult {
    pub(crate) files_scanned: usize,
    pub(crate) endpoints: Vec<Endpoint>,
    pub(crate) environments: Vec<ImportedEnvironment>,
    pub(crate) collection_names_by_id: BTreeMap<String, String>,
    pub(crate) folders_by_id: BTreeMap<String, ImportedFolderMeta>,
}

#[derive(Default, Debug)]
pub(crate) struct WorkspaceImportContext {
    pub(crate) workspace_ids: BTreeSet<String>,
    pub(crate) collection_ids: BTreeSet<String>,
}

#[derive(Debug)]
pub(crate) struct ImportedEnvironment {
    pub(crate) name: String,
    pub(crate) variables: Vec<KeyValue>,
}

#[derive(Default, Debug, Clone)]
pub(crate) struct ImportedFolderMeta {
    pub(crate) name: String,
    pub(crate) parent_folder_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostmanCollectionExport {
    info: Option<PostmanCollectionInfo>,
    item: Option<Vec<PostmanCollectionItem>>,
}

#[derive(Debug, Deserialize)]
struct PostmanCollectionInfo {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostmanCollectionItem {
    name: Option<String>,
    request: Option<PostmanRequest>,
    item: Option<Vec<PostmanCollectionItem>>,
}

#[derive(Debug, Deserialize)]
struct PostmanRequest {
    id: Option<String>,
    method: Option<String>,
    header: Option<Vec<PostmanHeader>>,
    body: Option<PostmanBody>,
    url: Option<PostmanUrl>,
    auth: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PostmanHeader {
    key: Option<String>,
    value: Option<serde_json::Value>,
    disabled: Option<bool>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PostmanBody {
    mode: Option<String>,
    raw: Option<String>,
    urlencoded: Option<Vec<PostmanField>>,
    formdata: Option<Vec<PostmanField>>,
    file: Option<serde_json::Value>,
    graphql: Option<PostmanGraphqlBody>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PostmanUrl {
    RawString(String),
    UrlObject(PostmanUrlObject),
}

#[derive(Debug, Deserialize)]
struct PostmanUrlObject {
    raw: Option<String>,
    protocol: Option<String>,
    host: Option<serde_json::Value>,
    path: Option<serde_json::Value>,
    port: Option<String>,
    query: Option<Vec<PostmanField>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PostmanField {
    pub(crate) key: Option<String>,
    pub(crate) value: Option<serde_json::Value>,
    #[serde(rename = "type")]
    pub(crate) field_type: Option<String>,
    pub(crate) src: Option<serde_json::Value>,
    pub(crate) disabled: Option<bool>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PostmanGraphqlBody {
    query: Option<String>,
    variables: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PostmanEnvironmentExport {
    name: Option<String>,
    values: Option<Vec<PostmanEnvironmentValue>>,
}

#[derive(Debug, Deserialize)]
struct PostmanEnvironmentValue {
    key: Option<String>,
    value: Option<serde_json::Value>,
    enabled: Option<bool>,
}

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

    let resolved_url = resolve_placeholders(&endpoint.url, &env_vars)
        .trim()
        .to_owned();
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

pub(crate) fn request_body_mode_from_data(
    data: &serde_json::Map<String, serde_json::Value>,
) -> String {
    if let Some(body_mode) = data
        .get("dataMode")
        .and_then(serde_json::Value::as_str)
        .map(normalize_body_mode_owned)
    {
        return body_mode;
    }

    if let Some(body) = data.get("body").and_then(serde_json::Value::as_object) {
        if let Some(mode) = body
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .map(normalize_body_mode_owned)
        {
            return mode;
        }
        if body.get("urlencoded").is_some_and(|value| !value.is_null()) {
            return "urlencoded".to_owned();
        }
        if body.get("formdata").is_some_and(|value| !value.is_null()) {
            return "form-data".to_owned();
        }
        if body.get("file").is_some_and(|value| !value.is_null()) {
            return "binary".to_owned();
        }
        if body.get("raw").is_some_and(|value| !value.is_null()) {
            return "raw".to_owned();
        }
    }

    if data
        .get("rawModeData")
        .is_some_and(|value| !value.is_null())
    {
        return "raw".to_owned();
    }

    "none".to_owned()
}

fn postman_request_body_mode(request: &PostmanRequest) -> String {
    let Some(body) = request.body.as_ref() else {
        return "none".to_owned();
    };

    if let Some(mode) = body.mode.as_deref() {
        return normalize_body_mode_owned(mode);
    }

    if body.urlencoded.is_some() {
        return "urlencoded".to_owned();
    }
    if body.formdata.is_some() {
        return "form-data".to_owned();
    }
    if body.file.is_some() {
        return "binary".to_owned();
    }
    if body.raw.is_some() || body.graphql.is_some() {
        return "raw".to_owned();
    }
    "none".to_owned()
}

pub(crate) fn build_curl_command(
    endpoint: &Endpoint,
    env_vars: &BTreeMap<String, String>,
) -> String {
    let resolved_method = endpoint.method.trim().to_uppercase();
    let resolved_url = resolve_placeholders(&endpoint.url, env_vars);
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

    format!(
        "{}|{}|{}|{}|{}",
        endpoint.method.trim().to_uppercase(),
        endpoint.url.trim().to_lowercase(),
        endpoint.collection.trim().to_lowercase(),
        endpoint.folder_path.trim().to_lowercase(),
        endpoint.name.trim().to_lowercase()
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

pub(crate) fn read_json_or_default<T>(path: &Path) -> io::Result<Option<T>>
where
    T: DeserializeOwned,
{
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    let value = serde_json::from_str::<T>(&raw).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid JSON at {}: {err}", path.display()),
        )
    })?;

    Ok(Some(value))
}

pub(crate) fn write_json_pretty<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize + ?Sized,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let raw = serde_json::to_string_pretty(value).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to serialize {}: {err}", path.display()),
        )
    })?;

    fs::write(path, raw)?;
    Ok(())
}

pub(crate) fn create_security_metadata(
    password: &str,
) -> Result<(SecurityMetadata, KeyMaterial), String> {
    let mut salt = [0_u8; 16];
    OsRng.fill_bytes(&mut salt);

    let key = derive_key(password, &salt)?;
    let verifier = encrypt_bytes(&key, VERIFIER_PLAINTEXT)?;

    Ok((
        SecurityMetadata {
            version: 1,
            salt_b64: BASE64.encode(salt),
            verifier,
        },
        key,
    ))
}

pub(crate) fn verify_password(
    password: &str,
    metadata: &SecurityMetadata,
) -> Result<KeyMaterial, String> {
    let salt = BASE64
        .decode(metadata.salt_b64.as_bytes())
        .map_err(|err| format!("invalid metadata salt: {err}"))?;
    let key = derive_key(password, &salt)?;

    let verifier = decrypt_bytes(&key, &metadata.verifier)?;
    if verifier != VERIFIER_PLAINTEXT {
        return Err("password verification failed".to_owned());
    }

    Ok(key)
}

fn derive_key(password: &str, salt: &[u8]) -> Result<KeyMaterial, String> {
    let mut key = [0_u8; 32];
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|err| err.to_string())?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| err.to_string())?;
    Ok(key)
}

pub(crate) fn encrypt_bytes(key: &KeyMaterial, plaintext: &[u8]) -> Result<EncryptedBlob, String> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|err| format!("encryption failed: {err}"))?;

    Ok(EncryptedBlob {
        version: 1,
        nonce_b64: BASE64.encode(nonce_bytes),
        ciphertext_b64: BASE64.encode(ciphertext),
    })
}

pub(crate) fn decrypt_bytes(key: &KeyMaterial, blob: &EncryptedBlob) -> Result<Vec<u8>, String> {
    let nonce = BASE64
        .decode(blob.nonce_b64.as_bytes())
        .map_err(|err| format!("invalid nonce encoding: {err}"))?;
    if nonce.len() != 24 {
        return Err("invalid nonce length".to_owned());
    }

    let ciphertext = BASE64
        .decode(blob.ciphertext_b64.as_bytes())
        .map_err(|err| format!("invalid ciphertext encoding: {err}"))?;

    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|err| format!("decryption failed: {err}"))
}

pub(crate) fn serialize_workspace_bundle(
    payload: &SharedWorkspacePayload,
    password: &str,
) -> Result<String, String> {
    if password.trim().is_empty() {
        return Err("Bundle password is required.".to_owned());
    }

    let payload_bytes = serde_json::to_vec(payload)
        .map_err(|err| format!("Failed to encode payload JSON: {err}"))?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&payload_bytes)
        .map_err(|err| format!("Failed to gzip payload: {err}"))?;
    let compressed = encoder
        .finish()
        .map_err(|err| format!("Failed to finalize gzip payload: {err}"))?;

    let mut salt = [0_u8; 16];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(password, &salt)?;
    let encrypted = encrypt_bytes(&key, &compressed)?;

    let bundle = SharedWorkspaceBundleFile {
        version: 1,
        salt_b64: BASE64.encode(salt),
        encrypted,
    };

    serde_json::to_string_pretty(&bundle)
        .map_err(|err| format!("Failed to encode bundle JSON: {err}"))
}

pub(crate) fn deserialize_workspace_bundle(
    raw: &str,
    password: &str,
) -> Result<SharedWorkspacePayload, String> {
    if password.trim().is_empty() {
        return Err("Bundle password is required.".to_owned());
    }

    let bundle = serde_json::from_str::<SharedWorkspaceBundleFile>(raw)
        .map_err(|err| format!("Invalid bundle file JSON: {err}"))?;
    if bundle.version != 1 {
        return Err(format!("Unsupported bundle version: {}", bundle.version));
    }

    let salt = BASE64
        .decode(bundle.salt_b64.as_bytes())
        .map_err(|err| format!("Invalid bundle salt: {err}"))?;
    let key = derive_key(password, &salt)?;
    let compressed = decrypt_bytes(&key, &bundle.encrypted)?;

    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut payload_bytes = Vec::new();
    decoder
        .read_to_end(&mut payload_bytes)
        .map_err(|err| format!("Failed to decompress bundle payload: {err}"))?;

    let payload = serde_json::from_slice::<SharedWorkspacePayload>(&payload_bytes)
        .map_err(|err| format!("Invalid bundle payload JSON: {err}"))?;
    if payload.version != 1 {
        return Err(format!("Unsupported payload version: {}", payload.version));
    }

    Ok(payload)
}

pub(crate) fn scan_postman_directory(
    path: &Path,
    workspace_name_filter: Option<&str>,
) -> ImportScanResult {
    let context_root = resolve_postman_context_root(path);
    let scan_root = resolve_postman_scan_root(path, &context_root);
    let mut result = scan_postman_json_exports(&scan_root);
    let import_context = build_workspace_import_context(&context_root, workspace_name_filter);
    let cache_result = scan_postman_cached_payloads(&scan_root, &import_context);
    let leveldb_result = scan_postman_leveldb_payloads(path, &import_context);
    let requester_result = scan_postman_requester_logs(&context_root, &import_context);

    merge_import_scan_result(&mut result, cache_result);
    merge_import_scan_result(&mut result, leveldb_result);
    merge_import_scan_result(&mut result, requester_result);
    result
}

fn merge_import_scan_result(target: &mut ImportScanResult, incoming: ImportScanResult) {
    target.files_scanned += incoming.files_scanned;
    target.endpoints.extend(incoming.endpoints);
    target.environments.extend(incoming.environments);
    target
        .collection_names_by_id
        .extend(incoming.collection_names_by_id);
    target.folders_by_id.extend(incoming.folders_by_id);
}

fn resolve_postman_scan_root(path: &Path, context_root: &Path) -> PathBuf {
    if path.is_file() && is_leveldb_data_file(path) {
        return context_root.to_path_buf();
    }
    if is_leveldb_directory(path) {
        return context_root.to_path_buf();
    }
    path.to_path_buf()
}

fn resolve_postman_context_root(path: &Path) -> PathBuf {
    let mut cursor = if path.is_file() {
        path.parent().map(Path::to_path_buf)
    } else {
        Some(path.to_path_buf())
    };

    while let Some(candidate) = cursor {
        let local_storage_leveldb = candidate.join("Local Storage").join("leveldb");
        let partitions = candidate.join("Partitions");
        if local_storage_leveldb.exists() || partitions.exists() {
            return candidate;
        }
        cursor = candidate.parent().map(Path::to_path_buf);
    }

    path.to_path_buf()
}

fn scan_postman_json_exports(path: &Path) -> ImportScanResult {
    let mut result = ImportScanResult::default();

    for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| !ext.eq_ignore_ascii_case("json"))
            .unwrap_or(true)
        {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.len() > 8 * 1024 * 1024 {
            continue;
        }

        result.files_scanned += 1;
        let raw = match fs::read_to_string(entry.path()) {
            Ok(raw) => raw,
            Err(_) => continue,
        };

        if let Some(mut imported_endpoints) = parse_postman_collection(&raw) {
            result.endpoints.append(&mut imported_endpoints);
        }

        if let Some(imported_environment) = parse_postman_environment(&raw, entry.path()) {
            result.environments.push(imported_environment);
        }
    }

    result
}

fn build_workspace_import_context(
    postman_root: &Path,
    workspace_name_filter: Option<&str>,
) -> WorkspaceImportContext {
    let workspace_ids = collect_workspace_ids(postman_root, workspace_name_filter);
    let mut collection_ids = collect_collection_ids_for_workspaces(postman_root, &workspace_ids);
    if !workspace_ids.is_empty() && collection_ids.len() < 3 {
        collection_ids.clear();
    }

    WorkspaceImportContext {
        workspace_ids,
        collection_ids,
    }
}

fn collect_workspace_ids(
    postman_root: &Path,
    workspace_name_filter: Option<&str>,
) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    let normalized_filter = workspace_name_filter
        .map(slugify_workspace_name)
        .filter(|value| !value.is_empty());

    let mut local_storage_files = vec![];
    for entry in WalkDir::new(postman_root)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("ldb") || ext.eq_ignore_ascii_case("log"))
            != Some(true)
        {
            continue;
        }
        let as_text = entry.path().to_string_lossy();
        if as_text.contains("Local Storage/leveldb") || as_text.contains("Local Storage\\leveldb") {
            local_storage_files.push(entry.path().to_path_buf());
        }
    }

    for file_path in &local_storage_files {
        let Ok(raw) = fs::read(file_path) else {
            continue;
        };
        if raw.len() > 32 * 1024 * 1024 {
            continue;
        }

        let text = String::from_utf8_lossy(&raw);
        for captures in LAST_ACTIVE_WORKSPACE_PATTERN.captures_iter(&text) {
            let Some(workspace_id) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let Some(workspace_name) = captures.get(2).map(|capture| capture.as_str()) else {
                continue;
            };

            let should_take = match &normalized_filter {
                Some(slug_filter) => slugify_workspace_name(workspace_name) == *slug_filter,
                None => true,
            };

            if should_take {
                output.insert(workspace_id.to_owned());
            }
        }
    }

    if output.is_empty() {
        for file_path in &local_storage_files {
            let Ok(raw) = fs::read(file_path) else {
                continue;
            };
            if raw.len() > 32 * 1024 * 1024 {
                continue;
            }

            let text = String::from_utf8_lossy(&raw);
            if let Some(slug_filter) = &normalized_filter {
                let pattern = format!(
                    r"workspace/{}~([0-9a-fA-F-]{{36}})",
                    regex::escape(slug_filter)
                );
                let Ok(regex) = Regex::new(&pattern) else {
                    continue;
                };
                for captures in regex.captures_iter(&text) {
                    if let Some(workspace_id) = captures.get(1).map(|capture| capture.as_str()) {
                        output.insert(workspace_id.to_owned());
                    }
                }
            } else if let Some(captures) = WORKSPACE_ROUTE_PATTERN.captures(&text) {
                if let Some(workspace_id) = captures.get(1).map(|capture| capture.as_str()) {
                    output.insert(workspace_id.to_owned());
                    break;
                }
            }
        }
    }

    output
}

fn collect_collection_ids_for_workspaces(
    postman_root: &Path,
    workspace_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    if workspace_ids.is_empty() {
        return output;
    }

    for entry in WalkDir::new(postman_root)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("ldb") || ext.eq_ignore_ascii_case("log"))
            != Some(true)
        {
            continue;
        }
        let as_text = entry.path().to_string_lossy();
        if !as_text.contains("indexeddb.leveldb") {
            continue;
        }

        let Ok(raw) = fs::read(entry.path()) else {
            continue;
        };
        if raw.len() > 64 * 1024 * 1024 {
            continue;
        }

        let text = String::from_utf8_lossy(&raw);
        for captures in WORKSPACE_COLLECTION_PATTERN.captures_iter(&text) {
            let Some(collection_id) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let Some(workspace_id) = captures.get(2).map(|capture| capture.as_str()) else {
                continue;
            };

            if workspace_ids.contains(workspace_id) {
                output.insert(collection_id.to_owned());
            }
        }
    }

    output
}

fn scan_postman_cached_payloads(
    postman_root: &Path,
    import_context: &WorkspaceImportContext,
) -> ImportScanResult {
    let mut result = ImportScanResult::default();

    for entry in WalkDir::new(postman_root)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if !is_postman_cache_data_file(entry.path()) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > 16 * 1024 * 1024 {
            continue;
        }

        let Ok(raw) = fs::read(entry.path()) else {
            continue;
        };
        result.files_scanned += 1;

        let Some(decoded_payload) = decode_first_gzip_payload(&raw) else {
            continue;
        };
        if decoded_payload.len() < 80 {
            continue;
        }
        if !decoded_payload.starts_with('{') && !decoded_payload.starts_with('[') {
            continue;
        }
        if !(decoded_payload.contains("\"model\"")
            || decoded_payload.contains("\"entities\"")
            || decoded_payload.contains("\"model_id\"")
            || (decoded_payload.contains("\"url\"")
                && (decoded_payload.contains("\"method\"")
                    || decoded_payload.contains("\"request\""))))
        {
            continue;
        }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(&decoded_payload) else {
            continue;
        };
        extract_import_entities_from_cache_json(&json, import_context, &mut result);
    }

    result
}

fn scan_postman_leveldb_payloads(
    source_path: &Path,
    import_context: &WorkspaceImportContext,
) -> ImportScanResult {
    let mut result = ImportScanResult::default();
    let leveldb_directories = resolve_leveldb_directories_for_import(source_path);

    for leveldb_dir in leveldb_directories {
        result.files_scanned += 1;
        let extracted_by_db =
            scan_postman_leveldb_directory_with_db(&leveldb_dir, import_context, &mut result);

        if extracted_by_db {
            continue;
        }

        for file_path in collect_leveldb_files(&leveldb_dir) {
            let Ok(metadata) = file_path.metadata() else {
                continue;
            };
            if metadata.len() > 96 * 1024 * 1024 {
                continue;
            }

            let Ok(raw) = fs::read(&file_path) else {
                continue;
            };
            result.files_scanned += 1;
            extract_import_entities_from_leveldb_binary(&raw, import_context, &mut result);
        }
    }

    result
}

fn resolve_leveldb_directories_for_import(source_path: &Path) -> Vec<PathBuf> {
    if is_leveldb_directory(source_path) {
        return vec![source_path.to_path_buf()];
    }

    if source_path.is_file() && is_leveldb_data_file(source_path) {
        if let Some(parent) = source_path.parent() {
            if is_leveldb_directory(parent) {
                return vec![parent.to_path_buf()];
            }
        }
        return vec![];
    }

    let directories = discover_leveldb_directories(source_path);
    if directories.is_empty() {
        return vec![];
    }

    if let Some(latest_dir) = select_latest_leveldb_directory(&directories) {
        return vec![latest_dir];
    }

    vec![]
}

fn discover_leveldb_directories(root: &Path) -> Vec<PathBuf> {
    let mut output = vec![];
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_dir() {
            continue;
        }
        if is_leveldb_directory(entry.path()) {
            output.push(entry.path().to_path_buf());
        }
    }
    output
}

fn is_leveldb_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lowered = name.to_ascii_lowercase();
    lowered.ends_with("leveldb")
}

fn is_leveldb_data_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("ldb") || ext.eq_ignore_ascii_case("log"))
        .unwrap_or(false)
}

fn collect_leveldb_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    let Ok(read_dir) = fs::read_dir(dir) else {
        return files;
    };

    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if is_leveldb_data_file(&path) {
            files.push(path);
        }
    }

    files
}

fn select_latest_leveldb_directory(directories: &[PathBuf]) -> Option<PathBuf> {
    directories
        .iter()
        .filter_map(|directory| {
            newest_leveldb_file_time(directory).map(|timestamp| {
                let directory_text = directory.to_string_lossy().to_ascii_lowercase();
                let priority =
                    if directory_text.contains("https_desktop.postman.com_0.indexeddb.leveldb") {
                        3_u8
                    } else if directory_text.contains("indexeddb.leveldb") {
                        2_u8
                    } else if directory_text.contains("local storage/leveldb")
                        || directory_text.contains("local storage\\leveldb")
                    {
                        1_u8
                    } else {
                        0_u8
                    };
                ((priority, timestamp), directory.clone())
            })
        })
        .max_by_key(|(sort_key, _)| *sort_key)
        .map(|(_, directory)| directory)
}

fn scan_postman_leveldb_directory_with_db(
    leveldb_dir: &Path,
    import_context: &WorkspaceImportContext,
    result: &mut ImportScanResult,
) -> bool {
    let before_total = result.endpoints.len() + result.environments.len();
    let mut processed_entries = 0usize;

    let mut remove_temp_after = None;
    let mut db = if let Some(db) = open_leveldb_database(leveldb_dir) {
        db
    } else if let Some(temp_dir) = copy_leveldb_directory_to_temp(leveldb_dir) {
        let opened = open_leveldb_database(&temp_dir);
        if opened.is_some() {
            remove_temp_after = Some(temp_dir.clone());
        }
        match opened {
            Some(db) => db,
            None => {
                let _ = fs::remove_dir_all(temp_dir);
                return false;
            }
        }
    } else {
        return false;
    };

    if let Ok(mut iter) = db.new_iter() {
        while let Some((key, value)) = iter.next() {
            processed_entries += 1;
            if processed_entries > 1_500_000 {
                break;
            }

            if !value.is_empty() && value.len() <= 4 * 1024 * 1024 {
                extract_import_entities_from_leveldb_binary(&value, import_context, result);
            }
            if !key.is_empty() && key.len() <= 512 * 1024 {
                extract_import_entities_from_leveldb_binary(&key, import_context, result);
            }
        }
    }

    if let Some(temp_dir) = remove_temp_after {
        let _ = fs::remove_dir_all(temp_dir);
    }

    let after_total = result.endpoints.len() + result.environments.len();
    after_total > before_total
}

fn open_leveldb_database(leveldb_dir: &Path) -> Option<LevelDb> {
    let mut options = LevelDbOptions::default();
    options.create_if_missing = false;
    options.error_if_exists = false;
    LevelDb::open(leveldb_dir, options).ok()
}

fn copy_leveldb_directory_to_temp(source_dir: &Path) -> Option<PathBuf> {
    let temp_dir = std::env::temp_dir().join(format!("mailman-leveldb-{}", create_id("tmp")));
    fs::create_dir_all(&temp_dir).ok()?;

    let read_dir = fs::read_dir(source_dir).ok()?;
    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let should_copy = is_leveldb_data_file(&path)
            || file_name.eq_ignore_ascii_case("CURRENT")
            || file_name.eq_ignore_ascii_case("LOCK")
            || file_name.eq_ignore_ascii_case("LOG")
            || file_name.eq_ignore_ascii_case("LOG.old")
            || file_name.starts_with("MANIFEST-");
        if !should_copy {
            continue;
        }

        let target_path = temp_dir.join(file_name);
        if fs::copy(&path, &target_path).is_err() {
            let _ = fs::remove_dir_all(&temp_dir);
            return None;
        }
    }

    Some(temp_dir)
}

fn newest_leveldb_file_time(directory: &Path) -> Option<SystemTime> {
    collect_leveldb_files(directory)
        .iter()
        .filter_map(|path| path.metadata().ok())
        .filter_map(|metadata| metadata.modified().ok())
        .max()
}

pub(crate) fn extract_import_entities_from_leveldb_binary(
    raw: &[u8],
    import_context: &WorkspaceImportContext,
    result: &mut ImportScanResult,
) {
    if find_bytes(raw, GZIP_MAGIC, 0).is_some() {
        if let Some(decoded_gzip) = decode_first_gzip_payload(raw) {
            extract_import_entities_from_leveldb_binary(
                decoded_gzip.as_bytes(),
                import_context,
                result,
            );
        }
    }

    let mut cursor = 0usize;
    let mut extracted = 0usize;

    while cursor < raw.len() && extracted < 10_000 {
        if raw[cursor] != b'{' && raw[cursor] != b'[' {
            cursor += 1;
            continue;
        }

        let Some(end_index) = find_balanced_json_end(raw, cursor, 2 * 1024 * 1024) else {
            cursor += 1;
            continue;
        };

        let payload = &raw[cursor..end_index];
        cursor = end_index;
        extracted += 1;

        if payload.len() < 80 || !payload_looks_like_postman_model(payload) {
            continue;
        }

        let Some(value) = parse_json_like_payload(payload) else {
            continue;
        };
        extract_import_entities_from_cache_json(&value, import_context, result);
    }

    let mut escaped_cursor = 0usize;
    while escaped_cursor < raw.len() && extracted < 20_000 {
        if escaped_cursor + 3 >= raw.len()
            || raw[escaped_cursor] != b'{'
            || raw[escaped_cursor + 1] != b'\\'
            || raw[escaped_cursor + 2] != b'"'
        {
            escaped_cursor += 1;
            continue;
        }

        let Some(end_index) =
            find_balanced_braces_end_ignoring_strings(raw, escaped_cursor, 2 * 1024 * 1024)
        else {
            escaped_cursor += 1;
            continue;
        };

        let payload = &raw[escaped_cursor..end_index];
        escaped_cursor = end_index;
        extracted += 1;

        if payload.len() < 80 {
            continue;
        }

        let Some(value) = parse_json_like_payload(payload) else {
            continue;
        };
        extract_import_entities_from_cache_json(&value, import_context, result);
    }
}

fn parse_json_like_payload(payload: &[u8]) -> Option<serde_json::Value> {
    let decoded = String::from_utf8_lossy(payload);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&decoded) {
        return Some(value);
    }

    let trimmed = decoded.trim_matches(char::from(0)).trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        if let Ok(unwrapped) = serde_json::from_str::<String>(trimmed) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&unwrapped) {
                return Some(value);
            }
        }
    }

    if trimmed.contains("\\\"") {
        if let Some(unescaped) = decode_escaped_json_string(trimmed) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&unescaped) {
                return Some(value);
            }
        }
    }

    None
}

fn find_balanced_braces_end_ignoring_strings(
    raw: &[u8],
    start_index: usize,
    max_length: usize,
) -> Option<usize> {
    if start_index >= raw.len() || raw[start_index] != b'{' {
        return None;
    }

    let end_limit = raw.len().min(start_index + max_length);
    let mut depth = 1usize;
    let mut cursor = start_index + 1;
    while cursor < end_limit {
        match raw[cursor] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(cursor + 1);
                }
            }
            _ => {}
        }
        cursor += 1;
    }

    None
}

fn scan_postman_requester_logs(
    postman_root: &Path,
    import_context: &WorkspaceImportContext,
) -> ImportScanResult {
    let mut result = ImportScanResult::default();
    let logs_dir = postman_root.join("logs");
    if !logs_dir.exists() {
        return result;
    }

    let Ok(read_dir) = fs::read_dir(&logs_dir) else {
        return result;
    };

    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("renderer-requester") || !file_name.ends_with(".log") {
            continue;
        }

        let Ok(metadata) = path.metadata() else {
            continue;
        };
        if metadata.len() > 64 * 1024 * 1024 {
            continue;
        }

        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        result.files_scanned += 1;

        let text = String::from_utf8_lossy(&raw);
        for captures in REQUESTER_CONFLICT_PATTERN.captures_iter(&text) {
            let Some(encoded_payload) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let decoded = decode_escaped_json_string(encoded_payload);
            let Some(decoded_payload) = decoded else {
                continue;
            };

            let Ok(value) = serde_json::from_str::<serde_json::Value>(&decoded_payload) else {
                continue;
            };
            let Some(object) = value.as_object() else {
                continue;
            };

            if let Some(endpoint) = endpoint_from_requester_log_object(object, import_context) {
                result.endpoints.push(endpoint);
            }
        }
    }

    result
}

fn decode_escaped_json_string(encoded: &str) -> Option<String> {
    let wrapped = format!("\"{encoded}\"");
    serde_json::from_str::<String>(&wrapped).ok()
}

fn endpoint_from_requester_log_object(
    object: &serde_json::Map<String, serde_json::Value>,
    import_context: &WorkspaceImportContext,
) -> Option<Endpoint> {
    let model_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if model_type != "request" {
        return None;
    }

    let collection_id = object
        .get("_collectionId")
        .and_then(serde_json::Value::as_str)
        .or_else(|| object.get("collection").and_then(serde_json::Value::as_str));
    if !import_context.collection_ids.is_empty() {
        if let Some(collection_id) = collection_id {
            if !import_context.collection_ids.contains(collection_id) {
                return None;
            }
        }
    }

    let url = request_url_from_data(object)?;
    if url.trim().is_empty() {
        return None;
    }

    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Imported Request");
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .map(|value| value.trim().to_uppercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_owned());

    let collection_name = collection_name_from_requester_resource(object);
    let folder_path = requester_folder_path_from_object(object);

    Some(Endpoint {
        id: create_id("ep"),
        source_request_id: object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_default(),
        source_collection_id: collection_id.unwrap_or_default().to_owned(),
        source_folder_id: folder_id_from_value(object.get("folder")).unwrap_or_default(),
        name: name.to_owned(),
        collection: collection_name,
        folder_path,
        method,
        url: normalize_postman_placeholders(&url),
        headers: request_headers_from_data(object),
        body_mode: request_body_mode_from_data(object),
        body: request_body_from_data(object),
    })
}

fn collection_name_from_requester_resource(
    object: &serde_json::Map<String, serde_json::Value>,
) -> String {
    if let Some(name) = object
        .get("collection")
        .and_then(serde_json::Value::as_object)
        .and_then(|collection| collection.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return name.to_owned();
    }

    if let Some(parent_collection) = object
        .get("_permissions")
        .and_then(serde_json::Value::as_object)
        .and_then(|permissions| permissions.get("parentCollection"))
        .and_then(serde_json::Value::as_object)
    {
        if let Some(name) = parent_collection
            .get("cache")
            .and_then(serde_json::Value::as_object)
            .and_then(|cache| cache.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return name.to_owned();
        }

        if let Some(name) = parent_collection
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return name.to_owned();
        }
    }

    "General".to_owned()
}

fn requester_folder_path_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> String {
    object
        .get("folder")
        .and_then(serde_json::Value::as_object)
        .and_then(|folder| folder.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_default()
}

fn payload_looks_like_postman_model(payload: &[u8]) -> bool {
    (find_bytes(payload, b"\"meta\"", 0).is_some()
        && (find_bytes(payload, b"\"model\"", 0).is_some()
            || find_bytes(payload, b"\"entities\"", 0).is_some()
            || find_bytes(payload, b"\"model_id\"", 0).is_some()))
        || (find_bytes(payload, b"\\\"meta\\\"", 0).is_some()
            && (find_bytes(payload, b"\\\"model\\\"", 0).is_some()
                || find_bytes(payload, b"\\\"entities\\\"", 0).is_some()
                || find_bytes(payload, b"\\\"model_id\\\"", 0).is_some()))
        || find_bytes(payload, b"\"type\":\"request\"", 0).is_some()
        || find_bytes(payload, b"\"type\": \"request\"", 0).is_some()
        || find_bytes(payload, b"\\\"type\\\":\\\"request\\\"", 0).is_some()
        || (find_bytes(payload, b"\"request\"", 0).is_some()
            && find_bytes(payload, b"\"url\"", 0).is_some())
}

fn find_balanced_json_end(raw: &[u8], start_index: usize, max_length: usize) -> Option<usize> {
    if start_index >= raw.len() {
        return None;
    }
    let opening = raw[start_index];
    if opening != b'{' && opening != b'[' {
        return None;
    }

    let mut stack = vec![if opening == b'{' { b'}' } else { b']' }];
    let mut in_string = false;
    let mut escaped = false;
    let end_limit = raw.len().min(start_index + max_length);

    let mut cursor = start_index + 1;
    while cursor < end_limit {
        let byte = raw[cursor];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            cursor += 1;
            continue;
        }

        match byte {
            b'"' => {
                in_string = true;
            }
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                let Some(expected) = stack.pop() else {
                    return None;
                };
                if byte != expected {
                    return None;
                }
                if stack.is_empty() {
                    return Some(cursor + 1);
                }
            }
            _ => {}
        }

        cursor += 1;
    }

    None
}

fn is_postman_cache_data_file(path: &Path) -> bool {
    let path_text = path.to_string_lossy();
    path_text.contains("Cache/Cache_Data") || path_text.contains("Cache\\Cache_Data")
}

fn decode_first_gzip_payload(raw: &[u8]) -> Option<String> {
    let mut cursor = 0usize;
    let mut attempts = 0usize;

    while cursor < raw.len() {
        let Some(found_index) = find_bytes(raw, GZIP_MAGIC, cursor) else {
            break;
        };
        attempts += 1;
        if attempts > 12 {
            break;
        }

        let mut decoder = GzDecoder::new(&raw[found_index..]);
        let mut decoded = Vec::with_capacity(4096);
        if decoder.read_to_end(&mut decoded).is_ok() && !decoded.is_empty() {
            return Some(String::from_utf8_lossy(&decoded).to_string());
        }

        cursor = found_index + 1;
    }

    None
}

fn find_bytes(haystack: &[u8], needle: &[u8], start_index: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || start_index >= haystack.len() {
        return None;
    }

    haystack[start_index..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| start_index + position)
}

fn extract_import_entities_from_cache_json(
    value: &serde_json::Value,
    import_context: &WorkspaceImportContext,
    result: &mut ImportScanResult,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some((collection_id, collection_name)) =
                collection_reference_from_cache_object(map)
            {
                result
                    .collection_names_by_id
                    .insert(collection_id, collection_name);
            }
            if let Some((folder_id, folder_meta)) = folder_reference_from_cache_object(map) {
                result.folders_by_id.insert(folder_id, folder_meta);
            }

            let endpoint = endpoint_from_cache_object(map, import_context)
                .or_else(|| endpoint_from_requester_log_object(map, import_context))
                .or_else(|| endpoint_from_collection_item_object(map));
            if let Some(endpoint) = endpoint {
                result.endpoints.push(endpoint);
            }
            if let Some(environment) = environment_from_cache_object(map, import_context) {
                result.environments.push(environment);
            }

            for child in map.values() {
                extract_import_entities_from_cache_json(child, import_context, result);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                extract_import_entities_from_cache_json(item, import_context, result);
            }
        }
        _ => {}
    }
}

pub(crate) fn endpoint_from_cache_object(
    object: &serde_json::Map<String, serde_json::Value>,
    import_context: &WorkspaceImportContext,
) -> Option<Endpoint> {
    let model_name = object
        .get("meta")
        .and_then(|meta| meta.get("model"))
        .and_then(serde_json::Value::as_str)?;
    if model_name != "request" {
        return None;
    }

    let data = object.get("data")?.as_object()?;
    let collection_id = collection_id_from_request_data(data);
    let collection_name = collection_name_from_request_data(data);
    if !import_context.collection_ids.is_empty() {
        if let Some(collection_id_value) = collection_id.as_ref() {
            if !import_context.collection_ids.contains(collection_id_value) {
                return None;
            }
        }
    }

    let url = request_url_from_data(data)?;
    if url.trim().is_empty() {
        return None;
    }

    let name = data
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Imported Request");
    let method = data
        .get("method")
        .and_then(serde_json::Value::as_str)
        .map(|value| value.trim().to_uppercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_owned());

    Some(Endpoint {
        id: create_id("ep"),
        source_request_id: request_source_id_from_cache_object(object, data),
        source_collection_id: collection_id.unwrap_or_default(),
        source_folder_id: request_folder_id_from_data(data),
        name: name.to_owned(),
        collection: collection_name,
        folder_path: request_folder_path_from_data(data),
        method,
        url: normalize_postman_placeholders(&url),
        headers: request_headers_from_data(data),
        body_mode: request_body_mode_from_data(data),
        body: request_body_from_data(data),
    })
}

fn request_source_id_from_cache_object(
    object: &serde_json::Map<String, serde_json::Value>,
    data: &serde_json::Map<String, serde_json::Value>,
) -> String {
    object
        .get("model_id")
        .or_else(|| object.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            data.get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn collection_reference_from_cache_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, String)> {
    let model_name = object
        .get("meta")
        .and_then(|meta| meta.get("model"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)?;
    if model_name != "collection" {
        return None;
    }

    let data = object.get("data")?.as_object()?;
    let id = data
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let name = data
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    Some((id.to_owned(), name.to_owned()))
}

fn folder_reference_from_cache_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, ImportedFolderMeta)> {
    let model_name = object
        .get("meta")
        .and_then(|meta| meta.get("model"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)?;
    if model_name != "folder" {
        return None;
    }

    let data = object.get("data")?.as_object()?;
    let id = data
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let name = data
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    let parent_folder_id = folder_id_from_value(data.get("folder"));
    Some((
        id.to_owned(),
        ImportedFolderMeta {
            name: name.to_owned(),
            parent_folder_id,
        },
    ))
}

fn collection_id_from_request_data(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    match data.get("collection") {
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(serde_json::Value::Object(collection)) => collection
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn folder_id_from_value(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(id)) => non_empty_trimmed(id).map(str::to_owned),
        Some(serde_json::Value::Object(folder)) => folder
            .get("id")
            .and_then(serde_json::Value::as_str)
            .and_then(non_empty_trimmed)
            .map(str::to_owned),
        _ => None,
    }
}

fn request_folder_id_from_data(data: &serde_json::Map<String, serde_json::Value>) -> String {
    folder_id_from_value(data.get("folder")).unwrap_or_default()
}

fn endpoint_from_collection_item_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<Endpoint> {
    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    let request_value = object.get("request")?;
    let request = serde_json::from_value::<PostmanRequest>(request_value.clone()).ok()?;
    let collection_name = object
        .get("collection")
        .and_then(serde_json::Value::as_object)
        .and_then(|collection| collection.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("General");
    let mut endpoint = endpoint_from_postman_request(name, &request, collection_name, "")?;
    endpoint.source_collection_id = match object.get("collection") {
        Some(serde_json::Value::String(id)) => id.trim().to_owned(),
        Some(serde_json::Value::Object(collection)) => collection
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    };
    endpoint.source_folder_id = folder_id_from_value(object.get("folder")).unwrap_or_default();
    Some(endpoint)
}

fn collection_name_from_request_data(data: &serde_json::Map<String, serde_json::Value>) -> String {
    match data.get("collection") {
        Some(serde_json::Value::Object(collection)) => collection
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "General".to_owned()),
        _ => "General".to_owned(),
    }
}

fn request_folder_path_from_data(data: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut parts = vec![];
    let mut cursor = data.get("folder").and_then(serde_json::Value::as_object);

    let mut guard = 0usize;
    while let Some(folder) = cursor {
        guard += 1;
        if guard > 16 {
            break;
        }
        if let Some(name) = folder
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(name.to_owned());
        }
        cursor = folder.get("folder").and_then(serde_json::Value::as_object);
    }

    parts.reverse();
    parts.join(" / ")
}

pub(crate) fn request_url_from_data(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let url_value = data.get("url")?;
    let mut url = match url_value {
        serde_json::Value::String(url) => url.trim().to_owned(),
        serde_json::Value::Object(url_object) => {
            if let Some(raw) = url_object
                .get("raw")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                raw.to_owned()
            } else {
                build_url_from_components(url_object)?
            }
        }
        _ => return None,
    };

    let mut query_pairs = request_query_pairs_from_data(data);
    if let Some(auth_query) = data
        .get("auth")
        .map(auth_query_pairs_from_value)
        .filter(|pairs| !pairs.is_empty())
    {
        query_pairs.extend(auth_query);
    }
    if !query_pairs.is_empty() && !url.contains('?') {
        let query_text = query_pairs
            .into_iter()
            .map(|query| {
                if query.value.trim().is_empty() {
                    query.key
                } else {
                    format!("{}={}", query.key, query.value)
                }
            })
            .collect::<Vec<_>>()
            .join("&");
        if !query_text.is_empty() {
            url.push('?');
            url.push_str(&query_text);
        }
    }

    Some(url)
}

pub(crate) fn request_headers_from_data(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Vec<KeyValue> {
    let headers_value = data
        .get("headerData")
        .or_else(|| data.get("headers"))
        .or_else(|| data.get("header"));
    let mut headers = headers_value
        .map(parse_key_value_entries)
        .unwrap_or_default();
    if let Some(auth_headers) = data
        .get("auth")
        .map(auth_headers_from_value)
        .filter(|headers| !headers.is_empty())
    {
        merge_header_pairs(&mut headers, auth_headers);
    }
    headers
}

pub(crate) fn request_body_from_data(data: &serde_json::Map<String, serde_json::Value>) -> String {
    let raw_mode_data = data
        .get("rawModeData")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default();
    if !raw_mode_data.trim().is_empty() {
        return normalize_postman_placeholders(&raw_mode_data);
    }

    if let Some(raw_body) = data
        .get("body")
        .and_then(serde_json::Value::as_object)
        .and_then(|body| body.get("raw"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_postman_placeholders(raw_body);
    }

    let data_mode = data
        .get("dataMode")
        .and_then(serde_json::Value::as_str)
        .map(|mode| mode.trim().to_ascii_lowercase());
    if let Some(data_payload) = data.get("data") {
        let rendered = render_postman_body_payload(data_payload, data_mode.as_deref());
        if !rendered.trim().is_empty() {
            return rendered;
        }
    }

    if let Some(body) = data.get("body").and_then(serde_json::Value::as_object) {
        let mode = body
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .map(|mode| mode.trim().to_ascii_lowercase());

        for key in ["urlencoded", "formdata", "graphql", "file"] {
            if let Some(payload) = body.get(key) {
                let effective_mode = mode.as_deref().or(Some(key));
                let rendered = render_postman_body_payload(payload, effective_mode);
                if !rendered.trim().is_empty() {
                    return rendered;
                }
            }
        }
    }

    String::new()
}

fn build_url_from_components(
    url_object: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let protocol = url_object
        .get("protocol")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https");

    let host = postman_path_like_value_to_string(url_object.get("host")?)?;
    let host = host.trim().trim_matches('/');
    if host.is_empty() {
        return None;
    }

    let mut url = format!("{protocol}://{host}");
    if let Some(port) = url_object
        .get("port")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        url.push(':');
        url.push_str(port);
    }

    if let Some(path) = url_object
        .get("path")
        .and_then(postman_path_like_value_to_string)
        .map(|path| path.trim_matches('/').to_owned())
        .filter(|path| !path.is_empty())
    {
        url.push('/');
        url.push_str(&path);
    }

    Some(url)
}

fn request_query_pairs_from_data(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Vec<KeyValue> {
    let mut output = vec![];
    let mut dedup = BTreeSet::new();

    let mut push_values = |value: &serde_json::Value| {
        for pair in parse_key_value_entries(value) {
            let key = pair.key.trim();
            if key.is_empty() {
                continue;
            }
            let dedup_key = format!("{key}\u{1f}{}", pair.value.trim());
            if dedup.contains(&dedup_key) {
                continue;
            }
            dedup.insert(dedup_key);
            output.push(pair);
        }
    };

    if let Some(query) = data.get("queryParams") {
        push_values(query);
    }

    if let Some(serde_json::Value::Object(url_object)) = data.get("url") {
        if let Some(query) = url_object.get("queryParams") {
            push_values(query);
        }
        if let Some(query) = url_object.get("query") {
            push_values(query);
        }
    }

    output
}

fn parse_key_value_entries(value: &serde_json::Value) -> Vec<KeyValue> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                if !postman_field_enabled(object) {
                    return None;
                }

                let key = object
                    .get("key")
                    .or_else(|| object.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())?;
                let value = object
                    .get("value")
                    .cloned()
                    .map(json_value_to_string)
                    .unwrap_or_default();

                Some(KeyValue {
                    key: normalize_postman_placeholders(key),
                    value: normalize_postman_placeholders(value.trim()),
                })
            })
            .collect(),
        serde_json::Value::Object(map) => map
            .iter()
            .filter_map(|(key, value)| {
                let trimmed = key.trim();
                if trimmed.is_empty() {
                    return None;
                }
                Some(KeyValue {
                    key: normalize_postman_placeholders(trimmed),
                    value: normalize_postman_placeholders(
                        json_value_to_string(value.clone()).trim(),
                    ),
                })
            })
            .collect(),
        serde_json::Value::String(text) => text
            .lines()
            .filter_map(|line| parse_header_like_line(line))
            .collect(),
        _ => vec![],
    }
}

fn parse_header_like_line(line: &str) -> Option<KeyValue> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (key, value) = if let Some(index) = trimmed.find(':') {
        (&trimmed[..index], &trimmed[index + 1..])
    } else if let Some(index) = trimmed.find('=') {
        (&trimmed[..index], &trimmed[index + 1..])
    } else {
        return None;
    };

    let key = key.trim();
    if key.is_empty() {
        return None;
    }

    Some(KeyValue {
        key: normalize_postman_placeholders(key),
        value: normalize_postman_placeholders(value.trim()),
    })
}

fn postman_field_enabled(field: &serde_json::Map<String, serde_json::Value>) -> bool {
    if field
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    field
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

fn merge_header_pairs(existing: &mut Vec<KeyValue>, incoming: Vec<KeyValue>) {
    let mut known_keys = existing
        .iter()
        .map(|header| header.key.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();

    for header in incoming {
        let key = header.key.trim();
        if key.is_empty() {
            continue;
        }
        let lower = key.to_ascii_lowercase();
        if known_keys.contains(&lower) {
            continue;
        }
        known_keys.insert(lower);
        existing.push(header);
    }
}

fn auth_headers_from_value(auth_value: &serde_json::Value) -> Vec<KeyValue> {
    let Some(auth) = auth_value.as_object() else {
        return vec![];
    };
    let auth_type = auth
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if auth_type.is_empty() || auth_type == "noauth" || auth_type == "inherit" {
        return vec![];
    }

    let fields = auth_fields_map(auth, &auth_type);
    match auth_type.as_str() {
        "bearer" => {
            let token = fields
                .get("token")
                .or_else(|| fields.get("value"))
                .map(|value| value.trim())
                .unwrap_or_default();
            if token.is_empty() {
                vec![]
            } else {
                vec![KeyValue {
                    key: "Authorization".to_owned(),
                    value: format!("Bearer {}", normalize_postman_placeholders(token)),
                }]
            }
        }
        "oauth2" => {
            let token = fields
                .get("accesstoken")
                .or_else(|| fields.get("token"))
                .map(|value| value.trim())
                .unwrap_or_default();
            if token.is_empty() {
                vec![]
            } else {
                vec![KeyValue {
                    key: "Authorization".to_owned(),
                    value: format!("Bearer {}", normalize_postman_placeholders(token)),
                }]
            }
        }
        "apikey" => {
            let key = fields
                .get("key")
                .map(|value| value.trim())
                .unwrap_or_default();
            let value = fields
                .get("value")
                .map(|value| value.trim())
                .unwrap_or_default();
            let location = fields
                .get("in")
                .or_else(|| fields.get("location"))
                .map(|value| value.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "header".to_owned());
            if key.is_empty() || value.is_empty() || location == "query" {
                vec![]
            } else {
                vec![KeyValue {
                    key: normalize_postman_placeholders(key),
                    value: normalize_postman_placeholders(value),
                }]
            }
        }
        _ => vec![],
    }
}

fn auth_query_pairs_from_value(auth_value: &serde_json::Value) -> Vec<KeyValue> {
    let Some(auth) = auth_value.as_object() else {
        return vec![];
    };
    let auth_type = auth
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if auth_type != "apikey" {
        return vec![];
    }

    let fields = auth_fields_map(auth, &auth_type);
    let key = fields
        .get("key")
        .map(|value| value.trim())
        .unwrap_or_default();
    let value = fields
        .get("value")
        .map(|value| value.trim())
        .unwrap_or_default();
    let location = fields
        .get("in")
        .or_else(|| fields.get("location"))
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "header".to_owned());

    if key.is_empty() || value.is_empty() || location != "query" {
        return vec![];
    }

    vec![KeyValue {
        key: normalize_postman_placeholders(key),
        value: normalize_postman_placeholders(value),
    }]
}

fn auth_fields_map(
    auth: &serde_json::Map<String, serde_json::Value>,
    auth_type: &str,
) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    let Some(container) = auth.get(auth_type) else {
        return output;
    };

    match container {
        serde_json::Value::Array(entries) => {
            for entry in entries {
                let Some(object) = entry.as_object() else {
                    continue;
                };
                if !postman_field_enabled(object) {
                    continue;
                }
                let key = object
                    .get("key")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| value.trim().to_ascii_lowercase())
                    .filter(|value| !value.is_empty());
                let Some(key) = key else {
                    continue;
                };
                let value = object
                    .get("value")
                    .cloned()
                    .map(json_value_to_string)
                    .unwrap_or_default();
                output.insert(key, value);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let trimmed_key = key.trim().to_ascii_lowercase();
                if trimmed_key.is_empty() {
                    continue;
                }
                output.insert(trimmed_key, json_value_to_string(value.clone()));
            }
        }
        _ => {}
    }

    output
}

fn render_postman_body_payload(value: &serde_json::Value, mode: Option<&str>) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => normalize_postman_placeholders(text),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            let normalized_mode = mode
                .map(|item| item.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if normalized_mode == "urlencoded" || normalized_mode == "formdata" {
                let kvs = parse_key_value_entries(value);
                if !kvs.is_empty() {
                    let separator = if normalized_mode == "urlencoded" {
                        "&"
                    } else {
                        "\n"
                    };
                    return kvs
                        .into_iter()
                        .map(|item| {
                            if item.value.trim().is_empty() {
                                item.key
                            } else {
                                format!("{}={}", item.key, item.value)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(separator);
                }
            }

            if normalized_mode == "graphql" {
                if let Some(graphql_object) = value.as_object() {
                    let query = graphql_object
                        .get("query")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default();
                    if !query.is_empty() {
                        let mut output = normalize_postman_placeholders(query);
                        if let Some(variables) = graphql_object
                            .get("variables")
                            .filter(|variables| !variables.is_null())
                        {
                            output.push_str("\n\n");
                            output.push_str(&format!(
                                "variables: {}",
                                normalize_postman_placeholders(
                                    &serde_json::to_string_pretty(variables)
                                        .unwrap_or_else(|_| variables.to_string())
                                )
                            ));
                        }
                        return output;
                    }
                }
            }

            if normalized_mode == "file" || normalized_mode == "binary" {
                if let Some(file_object) = value.as_object() {
                    if let Some(src) = file_object
                        .get("src")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|src| !src.is_empty())
                    {
                        return format!("@{}", normalize_postman_placeholders(src));
                    }
                }
                if let Some(path) = value
                    .as_str()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                {
                    return format!("@{}", normalize_postman_placeholders(path));
                }
            }

            normalize_postman_placeholders(
                &serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
            )
        }
    }
}

fn postman_path_like_value_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Array(values) => {
            let mut parts = vec![];
            for value in values {
                if let Some(value) = value
                    .as_str()
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                {
                    parts.push(value.to_owned());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("/"))
            }
        }
        _ => None,
    }
}

fn environment_from_cache_object(
    object: &serde_json::Map<String, serde_json::Value>,
    import_context: &WorkspaceImportContext,
) -> Option<ImportedEnvironment> {
    let model_name = object
        .get("meta")
        .and_then(|meta| meta.get("model"))
        .and_then(serde_json::Value::as_str)?;
    if model_name != "environment" {
        return None;
    }

    let data = object.get("data")?.as_object()?;
    let workspace_id = data
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            object
                .get("id")
                .and_then(serde_json::Value::as_str)
                .and_then(workspace_id_from_model_path)
        });

    if !import_context.workspace_ids.is_empty() {
        let Some(workspace_id_value) = workspace_id else {
            return None;
        };
        if !import_context.workspace_ids.contains(workspace_id_value) {
            return None;
        }
    }

    let values = data.get("values")?.as_array()?;
    let mut variables = vec![];
    for value in values {
        let Some(value_object) = value.as_object() else {
            continue;
        };
        let enabled = value_object
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            continue;
        }

        let key = value_object
            .get("key")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let value_string = value_object
            .get("value")
            .cloned()
            .map(json_value_to_string)
            .unwrap_or_default();

        variables.push(KeyValue {
            key: key.to_owned(),
            value: normalize_postman_placeholders(&value_string),
        });
    }

    if variables.is_empty() {
        return None;
    }

    let name = data
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            object
                .get("model_id")
                .and_then(serde_json::Value::as_str)
                .map(|model_id| format!("postman-{model_id}"))
                .unwrap_or_else(|| "postman-environment".to_owned())
        });

    Some(ImportedEnvironment { name, variables })
}

fn workspace_id_from_model_path(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    let _model_type = parts.next()?;
    let _model_id = parts.next()?;
    parts.next()
}

pub(crate) fn non_empty_trimmed(input: &str) -> Option<&str> {
    let value = input.trim();
    if value.is_empty() { None } else { Some(value) }
}

pub(crate) fn resolve_folder_path_from_lookup(
    folder_id: &str,
    folders_by_id: &BTreeMap<String, ImportedFolderMeta>,
) -> Option<String> {
    let mut cursor = non_empty_trimmed(folder_id)?.to_owned();
    let mut parts = vec![];
    let mut guard = 0usize;

    while guard < 32 {
        guard += 1;
        let Some(meta) = folders_by_id.get(&cursor) else {
            break;
        };
        if let Some(name) = non_empty_trimmed(&meta.name) {
            parts.push(name.to_owned());
        }

        let Some(parent_id) = meta.parent_folder_id.as_ref() else {
            break;
        };
        let Some(parent_id) = non_empty_trimmed(parent_id) else {
            break;
        };
        cursor = parent_id.to_owned();
    }

    if parts.is_empty() {
        None
    } else {
        parts.reverse();
        Some(parts.join(" / "))
    }
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

fn slugify_workspace_name(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            output.push('-');
            previous_dash = true;
        }
    }

    output.trim_matches('-').to_owned()
}

fn parse_postman_collection(raw: &str) -> Option<Vec<Endpoint>> {
    let parsed = serde_json::from_str::<PostmanCollectionExport>(raw).ok()?;
    let collection_name = parsed
        .info
        .and_then(|info| info.name)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Imported Collection".to_owned());
    let items = parsed.item?;

    let mut endpoints = vec![];
    flatten_postman_collection_items(&items, &[], &collection_name, &mut endpoints);

    if endpoints.is_empty() {
        None
    } else {
        Some(endpoints)
    }
}

fn flatten_postman_collection_items(
    items: &[PostmanCollectionItem],
    folder_parts: &[String],
    collection_name: &str,
    output: &mut Vec<Endpoint>,
) {
    for item in items {
        let item_name = item
            .name
            .as_ref()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .unwrap_or("Imported Request")
            .to_owned();

        if let Some(request) = &item.request {
            if let Some(endpoint) = endpoint_from_postman_request(
                &item_name,
                request,
                collection_name,
                &folder_parts.join(" / "),
            ) {
                output.push(endpoint);
            }
        }

        if let Some(children) = &item.item {
            let mut next_parts = folder_parts.to_vec();
            next_parts.push(item_name);
            flatten_postman_collection_items(children, &next_parts, collection_name, output);
        }
    }
}

fn postman_request_url(request: &PostmanRequest) -> Option<String> {
    let mut url = match &request.url {
        Some(PostmanUrl::RawString(url)) => url.trim().to_owned(),
        Some(PostmanUrl::UrlObject(url_object)) => {
            if let Some(raw) = url_object
                .raw
                .as_ref()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
            {
                raw.to_owned()
            } else {
                let protocol = url_object
                    .protocol
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("https");
                let host = postman_path_like_value_to_string(url_object.host.as_ref()?)?;
                let host = host.trim().trim_matches('/');
                if host.is_empty() {
                    return None;
                }

                let mut built = format!("{protocol}://{host}");
                if let Some(port) = url_object
                    .port
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    built.push(':');
                    built.push_str(port);
                }
                if let Some(path) = url_object
                    .path
                    .as_ref()
                    .and_then(postman_path_like_value_to_string)
                    .map(|path| path.trim_matches('/').to_owned())
                    .filter(|path| !path.is_empty())
                {
                    built.push('/');
                    built.push_str(&path);
                }
                built
            }
        }
        None => return None,
    };

    if !url.contains('?') {
        let mut query_pairs = vec![];
        if let Some(PostmanUrl::UrlObject(url_object)) = &request.url {
            if let Some(query) = &url_object.query {
                query_pairs.extend(query.iter().filter_map(|item| {
                    if item.disabled == Some(true) || item.enabled == Some(false) {
                        return None;
                    }

                    let key = item
                        .key
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())?;
                    let value = item
                        .value
                        .as_ref()
                        .cloned()
                        .map(json_value_to_string)
                        .unwrap_or_default();
                    Some(KeyValue {
                        key: normalize_postman_placeholders(key),
                        value: normalize_postman_placeholders(value.trim()),
                    })
                }));
            }
        }

        if let Some(auth_query) = request
            .auth
            .as_ref()
            .map(auth_query_pairs_from_value)
            .filter(|pairs| !pairs.is_empty())
        {
            query_pairs.extend(auth_query);
        }

        let query_text = query_pairs
            .into_iter()
            .map(|query| {
                if query.value.trim().is_empty() {
                    query.key
                } else {
                    format!("{}={}", query.key, query.value)
                }
            })
            .collect::<Vec<_>>()
            .join("&");
        if !query_text.is_empty() {
            url.push('?');
            url.push_str(&query_text);
        }
    }

    Some(url)
}

fn postman_request_headers(request: &PostmanRequest) -> Vec<KeyValue> {
    let mut headers = request
        .header
        .as_ref()
        .map(|headers| {
            headers
                .iter()
                .filter(|header| header.disabled != Some(true) && header.enabled != Some(false))
                .filter_map(|header| {
                    let key = header
                        .key
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())?;
                    let value = header
                        .value
                        .as_ref()
                        .cloned()
                        .map(json_value_to_string)
                        .unwrap_or_default();

                    Some(KeyValue {
                        key: normalize_postman_placeholders(key),
                        value: normalize_postman_placeholders(value.trim()),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if let Some(auth_headers) = request
        .auth
        .as_ref()
        .map(auth_headers_from_value)
        .filter(|headers| !headers.is_empty())
    {
        merge_header_pairs(&mut headers, auth_headers);
    }

    headers
}

fn postman_request_body(request: &PostmanRequest) -> String {
    let Some(body) = request.body.as_ref() else {
        return String::new();
    };

    if let Some(raw) = body
        .raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_postman_placeholders(raw);
    }

    let body_mode = body
        .mode
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase());
    if let Some(urlencoded) = body.urlencoded.as_ref() {
        let value =
            serde_json::to_value(urlencoded).unwrap_or(serde_json::Value::Array(Vec::new()));
        let rendered =
            render_postman_body_payload(&value, body_mode.as_deref().or(Some("urlencoded")));
        if !rendered.trim().is_empty() {
            return rendered;
        }
    }
    if let Some(formdata) = body.formdata.as_ref() {
        let rendered_formdata = render_postman_formdata_fields(formdata);
        if !rendered_formdata.trim().is_empty() {
            return rendered_formdata;
        }
        let value = serde_json::to_value(formdata).unwrap_or(serde_json::Value::Array(Vec::new()));
        let rendered =
            render_postman_body_payload(&value, body_mode.as_deref().or(Some("formdata")));
        if !rendered.trim().is_empty() {
            return rendered;
        }
    }
    if let Some(file) = body.file.as_ref() {
        let rendered = render_postman_body_payload(file, body_mode.as_deref().or(Some("file")));
        if !rendered.trim().is_empty() {
            return rendered;
        }
    }
    if let Some(graphql) = body.graphql.as_ref() {
        let value = serde_json::to_value(graphql).unwrap_or(serde_json::Value::Null);
        let rendered =
            render_postman_body_payload(&value, body_mode.as_deref().or(Some("graphql")));
        if !rendered.trim().is_empty() {
            return rendered;
        }
    }

    String::new()
}

pub(crate) fn render_postman_formdata_fields(fields: &[PostmanField]) -> String {
    let mut output = vec![];
    for field in fields {
        if field.disabled == Some(true) || field.enabled == Some(false) {
            continue;
        }

        let Some(key) = field
            .key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };

        let is_file = field
            .field_type
            .as_deref()
            .map(|value| value.trim().eq_ignore_ascii_case("file"))
            .unwrap_or(false);

        if is_file {
            let path = field
                .src
                .as_ref()
                .map(postman_file_src_to_string)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_default();
            if !path.is_empty() {
                output.push(format!(
                    "{}=@{}",
                    normalize_postman_placeholders(key),
                    normalize_postman_placeholders(path.trim())
                ));
            }
            continue;
        }

        let value = field
            .value
            .as_ref()
            .cloned()
            .map(json_value_to_string)
            .unwrap_or_default();
        output.push(format!(
            "{}={}",
            normalize_postman_placeholders(key),
            normalize_postman_placeholders(value.trim())
        ));
    }

    output.join("\n")
}

fn postman_file_src_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(path) => path.to_owned(),
        serde_json::Value::Array(paths) => paths
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|path| !path.trim().is_empty())
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

fn endpoint_from_postman_request(
    name: &str,
    request: &PostmanRequest,
    collection_name: &str,
    folder_path: &str,
) -> Option<Endpoint> {
    let raw_url = postman_request_url(request)?;

    if raw_url.trim().is_empty() {
        return None;
    }

    let method = request
        .method
        .as_ref()
        .map(|method| method.trim().to_uppercase())
        .filter(|method| !method.is_empty())
        .unwrap_or_else(|| "GET".to_owned());

    let headers = postman_request_headers(request);
    let body_mode = postman_request_body_mode(request);
    let body = postman_request_body(request);

    Some(Endpoint {
        id: create_id("ep"),
        source_request_id: request
            .id
            .as_ref()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default(),
        source_collection_id: String::new(),
        source_folder_id: String::new(),
        name: name.to_owned(),
        collection: collection_name.to_owned(),
        folder_path: folder_path.to_owned(),
        method,
        url: normalize_postman_placeholders(&raw_url),
        headers,
        body_mode,
        body,
    })
}

fn parse_postman_environment(raw: &str, source_path: &Path) -> Option<ImportedEnvironment> {
    let parsed = serde_json::from_str::<PostmanEnvironmentExport>(raw).ok()?;
    let values = parsed.values?;

    let mut variables = vec![];
    for value in values {
        if value.enabled == Some(false) {
            continue;
        }

        let key = value.key.unwrap_or_default().trim().to_owned();
        if key.is_empty() {
            continue;
        }

        let value_string = value
            .value
            .map(json_value_to_string)
            .map(|item| normalize_postman_placeholders(&item))
            .unwrap_or_default();

        variables.push(KeyValue {
            key,
            value: value_string,
        });
    }

    if variables.is_empty() {
        return None;
    }

    let name = parsed
        .name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            source_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_owned())
                .unwrap_or_else(|| "imported-environment".to_owned())
        });

    Some(ImportedEnvironment { name, variables })
}

fn json_value_to_string(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => text,
        complex => complex.to_string(),
    }
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
