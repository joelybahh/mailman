use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use directories::{BaseDirs, ProjectDirs};
use eframe::egui::{self, Color32, RichText, TextEdit};
use flate2::read::GzDecoder;
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

mod request_body;
use request_body::{
    PreparedRequestBody, build_prepared_request_body, computed_default_content_length,
    default_content_type_for_mode, normalize_body_mode, normalize_body_mode_owned,
    parse_body_fields, should_add_default_content_type,
};

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Mail Man",
        native_options,
        Box::new(|_cc| Ok(Box::new(PostmanCloneApp::new()))),
    )
}

static PLACEHOLDER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("placeholder regex should always compile")
});

static POSTMAN_PLACEHOLDER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}")
        .expect("postman placeholder regex should always compile")
});

static WORKSPACE_ROUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"workspace/[A-Za-z0-9._-]+~([0-9a-fA-F-]{36})")
        .expect("workspace route regex should compile")
});

static LAST_ACTIVE_WORKSPACE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"lastActiveWorkspaceData.*?"id":"([0-9a-fA-F-]{36})","name":"([^"]+)""#)
        .expect("last active workspace regex should compile")
});

static WORKSPACE_COLLECTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"collection/([0-9A-Za-z-]{8,80})/([0-9a-fA-F-]{36})")
        .expect("workspace collection regex should compile")
});
static REQUESTER_CONFLICT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"BaseEditorModel~conflictState:\s*(?:latestResource|currentResource):","((?:\\.|[^"])*)""#)
        .expect("requester conflict regex should compile")
});

const GZIP_MAGIC: &[u8] = b"\x1f\x8b\x08";

const METHOD_OPTIONS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
];
const BODY_MODE_OPTIONS: [&str; 5] = ["none", "raw", "urlencoded", "form-data", "binary"];

const VERIFIER_PLAINTEXT: &[u8] = b"delivery-man-unlock-verifier-v1";

type KeyMaterial = [u8; 32];

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct KeyValue {
    key: String,
    value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Endpoint {
    id: String,
    #[serde(default)]
    source_request_id: String,
    #[serde(default)]
    source_collection_id: String,
    #[serde(default)]
    source_folder_id: String,
    name: String,
    #[serde(default)]
    collection: String,
    #[serde(default)]
    folder_path: String,
    method: String,
    url: String,
    headers: Vec<KeyValue>,
    #[serde(default = "default_endpoint_body_mode")]
    body_mode: String,
    body: String,
}

impl Endpoint {
    fn with_defaults(id: String, name: &str, method: &str, url: &str) -> Self {
        Self {
            id,
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: name.to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: method.to_owned(),
            url: url.to_owned(),
            headers: vec![],
            body_mode: "none".to_owned(),
            body: String::new(),
        }
    }
}

fn default_endpoint_body_mode() -> String {
    "raw".to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EnvironmentIndexEntry {
    id: String,
    name: String,
    file_name: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct EnvironmentFile {
    variables: Vec<KeyValue>,
}

#[derive(Clone, Debug)]
struct Environment {
    id: String,
    name: String,
    file_name: String,
    variables: Vec<KeyValue>,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct AppConfig {
    selected_endpoint_id: Option<String>,
    selected_environment_id: Option<String>,
    postman_preseed_done: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EncryptedBlob {
    version: u8,
    nonce_b64: String,
    ciphertext_b64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SecurityMetadata {
    version: u8,
    salt_b64: String,
    verifier: EncryptedBlob,
}

#[derive(Default, Debug)]
struct ResponseState {
    status_code: Option<u16>,
    status_text: String,
    duration_ms: Option<u128>,
    headers: Vec<KeyValue>,
    body: String,
    error: Option<String>,
}

impl ResponseState {
    fn clear_for_request(&mut self) {
        self.status_code = None;
        self.status_text = "Sending request...".to_owned();
        self.duration_ms = None;
        self.headers.clear();
        self.body.clear();
        self.error = None;
    }
}

#[derive(Debug)]
struct AppStorage {
    base_dir: PathBuf,
    endpoints_path: PathBuf,
    requests_dir: PathBuf,
    environments_index_path: PathBuf,
    environments_dir: PathBuf,
    config_path: PathBuf,
    security_path: PathBuf,
}

#[derive(Debug)]
struct CoreData {
    endpoints: Vec<Endpoint>,
    environment_index: Vec<EnvironmentIndexEntry>,
    config: AppConfig,
}

impl AppStorage {
    fn new() -> Self {
        let fallback_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("delivery-man-data");
        let base_dir = ProjectDirs::from("com", "deliveryman", "delivery-man")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or(fallback_dir);
        let environments_dir = base_dir.join("envs");

        Self {
            endpoints_path: base_dir.join("endpoints.json"),
            requests_dir: base_dir.join("requests"),
            environments_index_path: base_dir.join("environments.json"),
            config_path: base_dir.join("config.json"),
            security_path: base_dir.join("security.json"),
            base_dir,
            environments_dir,
        }
    }

    fn ensure_directories(&self) -> io::Result<()> {
        fs::create_dir_all(&self.base_dir)?;
        fs::create_dir_all(&self.environments_dir)?;
        fs::create_dir_all(&self.requests_dir)?;
        Ok(())
    }

    fn load_core_data(&self) -> io::Result<CoreData> {
        self.ensure_directories()?;

        let mut endpoints = self.load_endpoints_tree()?;
        if endpoints.is_empty() {
            endpoints =
                read_json_or_default::<Vec<Endpoint>>(&self.endpoints_path)?.unwrap_or_default();
        }
        if endpoints.is_empty() {
            endpoints = default_endpoints();
        }
        for endpoint in &mut endpoints {
            if endpoint.collection.trim().is_empty() {
                endpoint.collection = "General".to_owned();
            }
            endpoint.folder_path = normalize_folder_path(&endpoint.folder_path);
        }

        let mut config: AppConfig =
            read_json_or_default::<AppConfig>(&self.config_path)?.unwrap_or_default();

        let mut environment_index: Vec<EnvironmentIndexEntry> =
            read_json_or_default::<Vec<EnvironmentIndexEntry>>(&self.environments_index_path)?
                .unwrap_or_default();
        if environment_index.is_empty() {
            environment_index = default_environment_index();
        }

        if config.selected_endpoint_id.is_none() {
            config.selected_endpoint_id = endpoints.first().map(|item| item.id.clone());
        }
        if config.selected_environment_id.is_none() {
            config.selected_environment_id = environment_index.first().map(|item| item.id.clone());
        }

        Ok(CoreData {
            endpoints,
            environment_index,
            config,
        })
    }

    fn load_security_metadata(&self) -> io::Result<Option<SecurityMetadata>> {
        self.ensure_directories()?;
        read_json_or_default::<SecurityMetadata>(&self.security_path)
    }

    fn save_security_metadata(&self, metadata: &SecurityMetadata) -> io::Result<()> {
        self.ensure_directories()?;
        write_json_pretty(&self.security_path, metadata)
    }

    fn load_environments(
        &self,
        index_entries: &[EnvironmentIndexEntry],
        key: &KeyMaterial,
    ) -> io::Result<(Vec<Environment>, bool)> {
        self.ensure_directories()?;

        let mut environments = Vec::with_capacity(index_entries.len());
        let mut found_legacy_plaintext = false;

        for entry in index_entries {
            let env_path = self.environments_dir.join(&entry.file_name);
            let variables = if env_path.exists() {
                let raw = fs::read_to_string(&env_path)?;
                if let Ok(encrypted_blob) = serde_json::from_str::<EncryptedBlob>(&raw) {
                    let plaintext = decrypt_bytes(key, &encrypted_blob).map_err(|err| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("failed to decrypt {}: {err}", env_path.display()),
                        )
                    })?;
                    let env_file =
                        serde_json::from_slice::<EnvironmentFile>(&plaintext).map_err(|err| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "invalid decrypted env payload in {}: {err}",
                                    env_path.display()
                                ),
                            )
                        })?;
                    env_file.variables
                } else if let Ok(legacy_plaintext) = serde_json::from_str::<EnvironmentFile>(&raw) {
                    found_legacy_plaintext = true;
                    legacy_plaintext.variables
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "environment file {} is neither encrypted nor valid legacy JSON",
                            env_path.display()
                        ),
                    ));
                }
            } else {
                default_variables_for_environment_name(&entry.name)
            };

            environments.push(Environment {
                id: entry.id.clone(),
                name: entry.name.clone(),
                file_name: entry.file_name.clone(),
                variables,
            });
        }

        if environments.is_empty() {
            environments = default_environments();
        }

        Ok((environments, found_legacy_plaintext))
    }

    fn save_all(
        &self,
        endpoints: &[Endpoint],
        environments: &[Environment],
        config: &AppConfig,
        key: &KeyMaterial,
    ) -> io::Result<()> {
        self.ensure_directories()?;

        self.save_endpoints_tree(endpoints)?;
        write_json_pretty(&self.config_path, config)?;

        let index_entries = environments
            .iter()
            .map(|env| EnvironmentIndexEntry {
                id: env.id.clone(),
                name: env.name.clone(),
                file_name: env.file_name.clone(),
            })
            .collect::<Vec<_>>();
        write_json_pretty(&self.environments_index_path, &index_entries)?;

        for env in environments {
            let env_file = EnvironmentFile {
                variables: env.variables.clone(),
            };
            let serialized = serde_json::to_vec(&env_file).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to serialize env {}: {err}", env.name),
                )
            })?;
            let encrypted = encrypt_bytes(key, &serialized).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to encrypt env {}: {err}", env.name),
                )
            })?;

            let env_path = self.environments_dir.join(&env.file_name);
            write_json_pretty(&env_path, &encrypted)?;
        }

        Ok(())
    }

    fn load_endpoints_tree(&self) -> io::Result<Vec<Endpoint>> {
        let mut endpoints = vec![];
        if !self.requests_dir.exists() {
            return Ok(endpoints);
        }

        for entry in WalkDir::new(&self.requests_dir)
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
                .map(|ext| !ext.eq_ignore_ascii_case("json"))
                .unwrap_or(true)
            {
                continue;
            }

            let raw = fs::read_to_string(entry.path())?;
            let mut endpoint = serde_json::from_str::<Endpoint>(&raw).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid endpoint file {}: {err}", entry.path().display()),
                )
            })?;
            endpoint.body_mode = normalize_body_mode_owned(&endpoint.body_mode);
            if endpoint.body_mode == "raw" && endpoint.body.is_empty() {
                endpoint.body_mode = "none".to_owned();
            }
            endpoints.push(endpoint);
        }

        endpoints.sort_by(|left, right| {
            (
                left.collection.to_lowercase(),
                left.folder_path.to_lowercase(),
                left.name.to_lowercase(),
            )
                .cmp(&(
                    right.collection.to_lowercase(),
                    right.folder_path.to_lowercase(),
                    right.name.to_lowercase(),
                ))
        });
        Ok(endpoints)
    }

    fn save_endpoints_tree(&self, endpoints: &[Endpoint]) -> io::Result<()> {
        if self.requests_dir.exists() {
            fs::remove_dir_all(&self.requests_dir)?;
        }
        fs::create_dir_all(&self.requests_dir)?;

        for endpoint in endpoints {
            let collection = if endpoint.collection.trim().is_empty() {
                "General"
            } else {
                endpoint.collection.trim()
            };
            let mut endpoint_dir = self.requests_dir.join(safe_path_segment(collection));
            for folder in split_folder_path(&endpoint.folder_path) {
                endpoint_dir = endpoint_dir.join(safe_path_segment(folder));
            }

            fs::create_dir_all(&endpoint_dir)?;
            let file_name = format!(
                "{}__{}.json",
                safe_path_segment(&endpoint.method),
                safe_path_segment(&endpoint.id)
            );
            let file_path = endpoint_dir.join(file_name);
            write_json_pretty(&file_path, endpoint)?;
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppPhase {
    SetupPassword,
    UnlockPassword,
    Ready,
}

struct PostmanCloneApp {
    storage: AppStorage,
    phase: AppPhase,
    security_metadata: Option<SecurityMetadata>,
    key_material: Option<KeyMaterial>,

    endpoints: Vec<Endpoint>,
    pending_environment_index: Vec<EnvironmentIndexEntry>,
    environments: Vec<Environment>,

    selected_endpoint_id: Option<String>,
    selected_environment_id: Option<String>,
    config: AppConfig,

    setup_password: String,
    setup_password_confirm: String,
    unlock_password: String,
    auth_message: String,

    response: ResponseState,
    response_body_view: String,
    status_line: String,
    dirty: bool,
    last_mutation: Instant,
    in_flight: bool,
    response_rx: Option<Receiver<ResponseState>>,

    new_environment_name: String,
    postman_import_path: String,
    postman_workspace_filter: String,
    show_environment_panel: bool,
    confirm_delete_all_requests: bool,
}

impl PostmanCloneApp {
    fn new() -> Self {
        let storage = AppStorage::new();

        let (core_data, mut status_line) = match storage.load_core_data() {
            Ok(data) => (data, String::new()),
            Err(err) => (
                CoreData {
                    endpoints: default_endpoints(),
                    environment_index: default_environment_index(),
                    config: AppConfig::default(),
                },
                format!("Failed to load persisted data, using defaults: {err}"),
            ),
        };

        let security_metadata = match storage.load_security_metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                if status_line.is_empty() {
                    status_line = format!("Failed to read security metadata: {err}");
                }
                None
            }
        };

        let phase = if security_metadata.is_some() {
            AppPhase::UnlockPassword
        } else {
            AppPhase::SetupPassword
        };

        Self {
            storage,
            phase,
            security_metadata,
            key_material: None,
            endpoints: core_data.endpoints,
            pending_environment_index: core_data.environment_index,
            environments: vec![],
            selected_endpoint_id: core_data.config.selected_endpoint_id.clone(),
            selected_environment_id: core_data.config.selected_environment_id.clone(),
            config: core_data.config,
            setup_password: String::new(),
            setup_password_confirm: String::new(),
            unlock_password: String::new(),
            auth_message: String::new(),
            response: ResponseState::default(),
            response_body_view: String::new(),
            status_line,
            dirty: false,
            last_mutation: Instant::now(),
            in_flight: false,
            response_rx: None,
            new_environment_name: String::new(),
            postman_import_path: String::new(),
            postman_workspace_filter: String::new(),
            show_environment_panel: true,
            confirm_delete_all_requests: false,
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_mutation = Instant::now();
    }

    fn ensure_selected_ids(&mut self) {
        if self
            .selected_endpoint_id
            .as_ref()
            .and_then(|id| self.endpoints.iter().find(|item| &item.id == id))
            .is_none()
        {
            self.selected_endpoint_id = self.endpoints.first().map(|item| item.id.clone());
        }

        if self
            .selected_environment_id
            .as_ref()
            .and_then(|id| self.environments.iter().find(|item| &item.id == id))
            .is_none()
        {
            self.selected_environment_id = self.environments.first().map(|item| item.id.clone());
        }
    }

    fn try_auto_save(&mut self) {
        if self.phase != AppPhase::Ready || !self.dirty {
            return;
        }
        if self.last_mutation.elapsed() < Duration::from_millis(350) {
            return;
        }

        let Some(key) = self.key_material.as_ref() else {
            self.status_line = "Cannot save: app is locked.".to_owned();
            return;
        };

        self.config.selected_endpoint_id = self.selected_endpoint_id.clone();
        self.config.selected_environment_id = self.selected_environment_id.clone();

        match self
            .storage
            .save_all(&self.endpoints, &self.environments, &self.config, key)
        {
            Ok(_) => {
                self.status_line = format!("Saved to {}", self.storage.base_dir.display());
                self.dirty = false;
            }
            Err(err) => {
                self.status_line = format!("Save failed: {err}");
            }
        }
    }

    fn selected_endpoint_index(&self) -> Option<usize> {
        let selected = self.selected_endpoint_id.as_ref()?;
        self.endpoints.iter().position(|item| &item.id == selected)
    }

    fn selected_environment_index(&self) -> Option<usize> {
        let selected = self.selected_environment_id.as_ref()?;
        self.environments
            .iter()
            .position(|item| &item.id == selected)
    }

    fn selected_environment_variables(&self) -> BTreeMap<String, String> {
        let mut variables = BTreeMap::new();
        let Some(index) = self.selected_environment_index() else {
            return variables;
        };

        for kv in &self.environments[index].variables {
            let key = kv.key.trim();
            if key.is_empty() {
                continue;
            }
            variables.insert(key.to_owned(), kv.value.clone());
        }

        variables
    }

    fn handle_setup_password_submission(&mut self) {
        let password = self.setup_password.clone();
        let confirm = self.setup_password_confirm.clone();

        if password.chars().count() < 12 {
            self.auth_message = "Password must be at least 12 characters.".to_owned();
            return;
        }
        if password != confirm {
            self.auth_message = "Password confirmation does not match.".to_owned();
            return;
        }

        let (metadata, key) = match create_security_metadata(&password) {
            Ok(result) => result,
            Err(err) => {
                self.auth_message = format!("Failed to configure encryption: {err}");
                return;
            }
        };

        if let Err(err) = self.storage.save_security_metadata(&metadata) {
            self.auth_message = format!("Failed to persist security metadata: {err}");
            return;
        }

        self.security_metadata = Some(metadata);
        self.setup_password.clear();
        self.setup_password_confirm.clear();

        if let Err(err) = self.complete_unlock(key) {
            self.auth_message = err;
            return;
        }

        self.auth_message = String::new();
        self.phase = AppPhase::Ready;
    }

    fn handle_unlock_password_submission(&mut self) {
        let Some(metadata) = self.security_metadata.as_ref() else {
            self.auth_message = "Missing security metadata; restart app.".to_owned();
            return;
        };

        let key = match verify_password(&self.unlock_password, metadata) {
            Ok(key) => key,
            Err(err) => {
                self.auth_message = format!("Invalid password: {err}");
                return;
            }
        };

        self.unlock_password.clear();
        if let Err(err) = self.complete_unlock(key) {
            self.auth_message = err;
            return;
        }

        self.auth_message = String::new();
        self.phase = AppPhase::Ready;
    }

    fn complete_unlock(&mut self, key: KeyMaterial) -> Result<(), String> {
        let (mut environments, found_legacy_plaintext) = self
            .storage
            .load_environments(&self.pending_environment_index, &key)
            .map_err(|err| format!("Failed to load encrypted environments: {err}"))?;

        if environments.is_empty() {
            environments = default_environments();
            self.mark_dirty();
        }

        self.environments = environments;
        self.key_material = Some(key);
        self.ensure_selected_ids();

        if found_legacy_plaintext {
            self.status_line =
                "Detected legacy plaintext environment files. They will be re-encrypted."
                    .to_owned();
            self.mark_dirty();
        }

        if !self.config.postman_preseed_done {
            let summary = self.import_postman_from_defaults(None);
            self.config.postman_preseed_done = true;
            self.mark_dirty();
            if summary.files_scanned > 0 {
                self.status_line = summary.to_message();
            } else {
                self.status_line =
                    "No Postman data found for auto-import in default folders.".to_owned();
            }
        }

        Ok(())
    }

    fn add_endpoint(&mut self) {
        let id = create_id("ep");
        self.endpoints.push(Endpoint {
            id: id.clone(),
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: "New Request".to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: "GET".to_owned(),
            url: "https://${api_host}/v1/health".to_owned(),
            headers: vec![],
            body_mode: "none".to_owned(),
            body: String::new(),
        });
        self.selected_endpoint_id = Some(id);
        self.mark_dirty();
    }

    fn delete_selected_endpoint(&mut self) {
        let Some(index) = self.selected_endpoint_index() else {
            return;
        };
        self.endpoints.remove(index);
        self.selected_endpoint_id = self.endpoints.first().map(|item| item.id.clone());
        self.mark_dirty();
    }

    fn delete_all_requests(&mut self) {
        let removed = self.endpoints.len();
        self.endpoints.clear();
        self.selected_endpoint_id = None;
        self.config.selected_endpoint_id = None;
        self.mark_dirty();
        self.status_line = format!("Cleared {removed} requests.");
    }

    fn add_environment(&mut self, name: String) {
        let id = create_id("env");
        self.environments.push(Environment {
            id: id.clone(),
            name,
            file_name: format!("{id}.json"),
            variables: vec![
                KeyValue {
                    key: "api_host".to_owned(),
                    value: "localhost:8080".to_owned(),
                },
                KeyValue {
                    key: "token".to_owned(),
                    value: "replace-me".to_owned(),
                },
            ],
        });
        self.selected_environment_id = Some(id);
        self.mark_dirty();
    }

    fn delete_selected_environment(&mut self) {
        if self.environments.len() <= 1 {
            self.status_line = "At least one environment must exist.".to_owned();
            return;
        }

        let Some(index) = self.selected_environment_index() else {
            return;
        };
        self.environments.remove(index);
        self.selected_environment_id = self.environments.first().map(|item| item.id.clone());
        self.mark_dirty();
    }

    fn send_selected_request(&mut self) {
        if self.in_flight {
            return;
        }

        let Some(endpoint_index) = self.selected_endpoint_index() else {
            self.status_line = "Select an endpoint first.".to_owned();
            return;
        };

        let endpoint = self.endpoints[endpoint_index].clone();
        let env_vars = self.selected_environment_variables();
        let (tx, rx) = mpsc::channel();

        self.in_flight = true;
        self.response.clear_for_request();
        self.response_body_view.clear();
        self.response_rx = Some(rx);
        self.status_line = format!("Sending {} {}", endpoint.method, endpoint.url);

        thread::spawn(move || {
            let state = execute_request(endpoint, env_vars);
            let _ = tx.send(state);
        });
    }

    fn copy_curl_for_selected_request(&mut self, ctx: &egui::Context) {
        let Some(endpoint_index) = self.selected_endpoint_index() else {
            self.status_line = "Select an endpoint first.".to_owned();
            return;
        };

        let endpoint = self.endpoints[endpoint_index].clone();
        let env_vars = self.selected_environment_variables();
        let curl = build_curl_command(&endpoint, &env_vars);
        ctx.copy_text(curl);
        self.status_line = format!("Copied cURL for {} {}", endpoint.method, endpoint.name);
    }

    fn poll_response_channel(&mut self) {
        let Some(rx) = &self.response_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(response_state) => {
                self.response = response_state;
                self.response_body_view = self.response.body.clone();
                self.in_flight = false;
                self.response_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.in_flight = false;
                self.response_rx = None;
                self.response.error = Some("Request worker disconnected.".to_owned());
                self.response_body_view.clear();
            }
        }
    }

    fn import_postman_from_defaults(
        &mut self,
        workspace_name_filter: Option<&str>,
    ) -> ImportSummary {
        let mut summary = ImportSummary::default();

        for dir in default_postman_directories() {
            let scan_result = scan_postman_directory(&dir, workspace_name_filter);
            let merge_summary = self.merge_postman_import(scan_result);
            summary.directories_scanned += 1;
            summary.files_scanned += merge_summary.files_scanned;
            summary.endpoints_added += merge_summary.endpoints_added;
            summary.endpoints_updated += merge_summary.endpoints_updated;
            summary.environments_added += merge_summary.environments_added;
            summary.environment_variables_merged += merge_summary.environment_variables_merged;
        }

        summary
    }

    fn import_postman_from_path(
        &mut self,
        path: &Path,
        workspace_name_filter: Option<&str>,
    ) -> ImportSummary {
        if !path.exists() {
            return ImportSummary {
                files_scanned: 0,
                endpoints_added: 0,
                endpoints_updated: 0,
                environments_added: 0,
                environment_variables_merged: 0,
                directories_scanned: 1,
            };
        }

        let scan_result = scan_postman_directory(path, workspace_name_filter);
        let mut summary = self.merge_postman_import(scan_result);

        if workspace_name_filter.is_some()
            && summary.endpoints_added == 0
            && summary.endpoints_updated == 0
            && summary.environments_added == 0
            && summary.environment_variables_merged == 0
        {
            let fallback_scan_result = scan_postman_directory(path, None);
            let fallback_summary = self.merge_postman_import(fallback_scan_result);

            summary.files_scanned += fallback_summary.files_scanned;
            summary.endpoints_added += fallback_summary.endpoints_added;
            summary.endpoints_updated += fallback_summary.endpoints_updated;
            summary.environments_added += fallback_summary.environments_added;
            summary.environment_variables_merged += fallback_summary.environment_variables_merged;
        }

        summary.directories_scanned = 1;
        summary
    }

    fn merge_postman_import(&mut self, mut scan_result: ImportScanResult) -> ImportSummary {
        let mut summary = ImportSummary {
            files_scanned: scan_result.files_scanned,
            ..ImportSummary::default()
        };

        let mut endpoint_key_to_index = self
            .endpoints
            .iter()
            .enumerate()
            .map(|(index, endpoint)| (endpoint_dedup_key(endpoint), index))
            .collect::<BTreeMap<_, _>>();

        for endpoint in &mut scan_result.endpoints {
            if endpoint.collection.trim().is_empty() || endpoint.collection.trim() == "General" {
                if let Some(collection_name) = scan_result
                    .collection_names_by_id
                    .get(endpoint.source_collection_id.trim())
                {
                    endpoint.collection = collection_name.clone();
                }
            }

            if endpoint.folder_path.trim().is_empty() {
                if let Some(folder_path) = resolve_folder_path_from_lookup(
                    endpoint.source_folder_id.trim(),
                    &scan_result.folders_by_id,
                ) {
                    endpoint.folder_path = folder_path;
                }
            }
        }

        for mut endpoint in scan_result.endpoints {
            if endpoint.collection.trim().is_empty() {
                endpoint.collection = "General".to_owned();
            }
            endpoint.folder_path = normalize_folder_path(&endpoint.folder_path);
            let key = endpoint_dedup_key(&endpoint);
            if let Some(existing_index) = endpoint_key_to_index.get(&key).copied() {
                if merge_endpoint_details(&mut self.endpoints[existing_index], endpoint) {
                    summary.endpoints_updated += 1;
                }
                continue;
            }
            endpoint_key_to_index.insert(key, self.endpoints.len());
            self.endpoints.push(endpoint);
            summary.endpoints_added += 1;
        }

        for imported_environment in scan_result.environments {
            let imported_name = imported_environment.name.trim();
            if imported_name.is_empty() {
                continue;
            }

            let existing_index = self
                .environments
                .iter()
                .position(|env| env.name.eq_ignore_ascii_case(imported_name));

            match existing_index {
                Some(index) => {
                    let mut existing_keys = self.environments[index]
                        .variables
                        .iter()
                        .map(|kv| kv.key.to_lowercase())
                        .collect::<BTreeSet<_>>();

                    let mut merged_count = 0usize;
                    for variable in imported_environment.variables {
                        let key = variable.key.trim().to_owned();
                        if key.is_empty() {
                            continue;
                        }

                        let lower = key.to_lowercase();
                        if existing_keys.contains(&lower) {
                            continue;
                        }
                        existing_keys.insert(lower);
                        self.environments[index].variables.push(variable);
                        merged_count += 1;
                    }

                    summary.environment_variables_merged += merged_count;
                }
                None => {
                    let env_id = create_id("env");
                    self.environments.push(Environment {
                        id: env_id.clone(),
                        name: imported_name.to_owned(),
                        file_name: format!("{env_id}.json"),
                        variables: imported_environment.variables,
                    });
                    summary.environments_added += 1;
                }
            }
        }

        if summary.endpoints_added > 0
            || summary.endpoints_updated > 0
            || summary.environments_added > 0
            || summary.environment_variables_merged > 0
        {
            self.ensure_selected_ids();
            self.mark_dirty();
        }

        summary
    }

    fn render_auth_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("Mail Man");
                ui.add_space(8.0);

                match self.phase {
                    AppPhase::SetupPassword => {
                        ui.label(
                            "Set a master password once. Environment variable files are encrypted at rest with Argon2id + XChaCha20-Poly1305.",
                        );
                        ui.label(
                            "The password is never stored and cannot be reversed. If lost, encrypted env files are unrecoverable.",
                        );
                        ui.add_space(12.0);

                        ui.add(
                            TextEdit::singleline(&mut self.setup_password)
                                .password(true)
                                .hint_text("Master password"),
                        );
                        ui.add(
                            TextEdit::singleline(&mut self.setup_password_confirm)
                                .password(true)
                                .hint_text("Confirm password"),
                        );

                        if ui.button("Configure Encryption and Open").clicked() {
                            self.handle_setup_password_submission();
                        }
                    }
                    AppPhase::UnlockPassword => {
                        ui.label("Enter your master password to decrypt environment variables.");
                        ui.add_space(12.0);
                        ui.add(
                            TextEdit::singleline(&mut self.unlock_password)
                                .password(true)
                                .hint_text("Master password"),
                        );

                        if ui.button("Unlock").clicked() {
                            self.handle_unlock_password_submission();
                        }
                    }
                    AppPhase::Ready => {}
                }

                if !self.auth_message.is_empty() {
                    ui.add_space(10.0);
                    ui.colored_label(Color32::from_rgb(240, 120, 120), &self.auth_message);
                }

                if !self.status_line.is_empty() {
                    ui.add_space(10.0);
                    ui.label(&self.status_line);
                }
            });
        });
    }

    fn render_toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Mail Man").strong().size(18.0));
                ui.separator();
                ui.label("Environment:");

                let selected_name = self
                    .selected_environment_index()
                    .and_then(|idx| self.environments.get(idx))
                    .map(|env| env.name.as_str())
                    .unwrap_or("None");

                egui::ComboBox::from_id_salt("environment-switcher")
                    .selected_text(selected_name)
                    .show_ui(ui, |ui| {
                        let mut selection_changed = false;
                        for env in &self.environments {
                            let selected =
                                self.selected_environment_id.as_deref() == Some(env.id.as_str());
                            if ui.selectable_label(selected, &env.name).clicked() {
                                self.selected_environment_id = Some(env.id.clone());
                                selection_changed = true;
                            }
                        }
                        if selection_changed {
                            self.mark_dirty();
                        }
                    });

                ui.add(
                    TextEdit::singleline(&mut self.new_environment_name)
                        .hint_text("new env (qa, sandbox)"),
                );
                if ui.button("Add Env").clicked() {
                    let name = self.new_environment_name.trim().to_owned();
                    if !name.is_empty() {
                        self.add_environment(name);
                        self.new_environment_name.clear();
                    }
                }
                if ui.button("Delete Env").clicked() {
                    self.delete_selected_environment();
                }

                ui.separator();
                let send_button = ui.add_enabled(
                    !self.in_flight,
                    egui::Button::new(if self.in_flight { "Sending..." } else { "Send" }),
                );
                if send_button.clicked() {
                    self.send_selected_request();
                }
                if ui.button("Copy cURL").clicked() {
                    self.copy_curl_for_selected_request(ctx);
                }
                if ui
                    .button(if self.show_environment_panel {
                        "Hide Env Panel"
                    } else {
                        "Show Env Panel"
                    })
                    .clicked()
                {
                    self.show_environment_panel = !self.show_environment_panel;
                }

                ui.separator();
                if ui.button("Import Postman (auto)").clicked() {
                    let workspace_filter =
                        non_empty_trimmed(&self.postman_workspace_filter).map(str::to_owned);
                    let summary = self.import_postman_from_defaults(workspace_filter.as_deref());
                    self.status_line = summary.to_message();
                }

                ui.add(
                    TextEdit::singleline(&mut self.postman_workspace_filter)
                        .hint_text("Workspace (e.g. Inspace Workspace)"),
                );
                ui.add(
                    TextEdit::singleline(&mut self.postman_import_path)
                        .hint_text("/path/to/Postman"),
                );
                if ui.button("Import Path").clicked() {
                    let path = PathBuf::from(self.postman_import_path.trim());
                    let workspace_filter =
                        non_empty_trimmed(&self.postman_workspace_filter).map(str::to_owned);
                    let summary = self.import_postman_from_path(&path, workspace_filter.as_deref());
                    self.status_line = summary.to_message();
                }

                if !self.confirm_delete_all_requests {
                    if ui.button("Delete All Requests").clicked() {
                        self.confirm_delete_all_requests = true;
                    }
                } else {
                    ui.colored_label(Color32::from_rgb(240, 120, 120), "Confirm clear all?");
                    if ui.button("Yes, Delete All").clicked() {
                        self.delete_all_requests();
                        self.confirm_delete_all_requests = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete_all_requests = false;
                    }
                }

                if ui.button("Save Now").clicked() {
                    self.last_mutation = Instant::now() - Duration::from_secs(1);
                    self.try_auto_save();
                }
            });
        });
    }

    fn render_endpoints_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("endpoints")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Requests");
                    if ui.button("+").clicked() {
                        self.add_endpoint();
                    }
                    if ui.button("-").clicked() {
                        self.delete_selected_endpoint();
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut selection_changed = false;
                    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<usize>>> =
                        BTreeMap::new();
                    for (index, endpoint) in self.endpoints.iter().enumerate() {
                        let collection = non_empty_trimmed(&endpoint.collection)
                            .unwrap_or("General")
                            .to_owned();
                        let folder = endpoint.folder_path.trim().to_owned();
                        grouped
                            .entry(collection)
                            .or_default()
                            .entry(folder)
                            .or_default()
                            .push(index);
                    }

                    for (collection, folders) in grouped {
                        ui.collapsing(collection, |ui| {
                            for (folder, indexes) in folders {
                                if folder.is_empty() {
                                    for endpoint_index in indexes {
                                        let endpoint = &self.endpoints[endpoint_index];
                                        let is_selected = self.selected_endpoint_id.as_deref()
                                            == Some(endpoint.id.as_str());
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                method_color(&endpoint.method),
                                                &endpoint.method,
                                            );
                                            if ui
                                                .selectable_label(is_selected, &endpoint.name)
                                                .clicked()
                                            {
                                                self.selected_endpoint_id =
                                                    Some(endpoint.id.clone());
                                                selection_changed = true;
                                            }
                                        });
                                    }
                                } else {
                                    ui.collapsing(folder, |ui| {
                                        for endpoint_index in indexes {
                                            let endpoint = &self.endpoints[endpoint_index];
                                            let is_selected = self.selected_endpoint_id.as_deref()
                                                == Some(endpoint.id.as_str());
                                            ui.horizontal(|ui| {
                                                ui.colored_label(
                                                    method_color(&endpoint.method),
                                                    &endpoint.method,
                                                );
                                                if ui
                                                    .selectable_label(is_selected, &endpoint.name)
                                                    .clicked()
                                                {
                                                    self.selected_endpoint_id =
                                                        Some(endpoint.id.clone());
                                                    selection_changed = true;
                                                }
                                            });
                                        }
                                    });
                                }
                            }
                        });
                    }

                    if selection_changed {
                        self.mark_dirty();
                    }
                });
            });
    }

    fn render_environment_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("environments")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("Environment Variables");
                ui.label("Each environment is stored in its own encrypted offline file.");
                ui.separator();

                let Some(index) = self.selected_environment_index() else {
                    ui.label("No environment selected.");
                    return;
                };

                let mut changed = false;
                let mut remove_index: Option<usize> = None;

                {
                    let env = &mut self.environments[index];

                    if ui.text_edit_singleline(&mut env.name).changed() {
                        changed = true;
                    }
                    ui.label(format!("File: {}", env.file_name));
                    ui.separator();

                    for (variable_index, variable) in env.variables.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .add(TextEdit::singleline(&mut variable.key).hint_text("key"))
                                .changed()
                            {
                                changed = true;
                            }
                            if ui
                                .add(TextEdit::singleline(&mut variable.value).hint_text("value"))
                                .changed()
                            {
                                changed = true;
                            }
                            if ui.button("x").clicked() {
                                remove_index = Some(variable_index);
                            }
                        });
                    }

                    if let Some(variable_index) = remove_index {
                        env.variables.remove(variable_index);
                        changed = true;
                    }

                    if ui.button("+ Add Variable").clicked() {
                        env.variables.push(KeyValue::default());
                        changed = true;
                    }
                }

                if changed {
                    self.mark_dirty();
                }
            });
    }

    fn render_request_editor(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading("Request Builder");
                    ui.separator();

                    let Some(index) = self.selected_endpoint_index() else {
                        ui.label("Select a request from the left panel.");
                        return;
                    };

                    let mut changed = false;
                    let mut remove_header_index: Option<usize> = None;
                    let preview_url;

                    {
                        let endpoint = &mut self.endpoints[index];

                        ui.horizontal(|ui| {
                            ui.label("Name");
                            if ui
                                .add(
                                    TextEdit::singleline(&mut endpoint.name)
                                        .desired_width(f32::INFINITY),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Collection");
                            if ui
                                .add(
                                    TextEdit::singleline(&mut endpoint.collection)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Inspace API V3"),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Folder");
                            if ui
                                .add(
                                    TextEdit::singleline(&mut endpoint.folder_path)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Micro / Query / Native"),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("method-picker")
                                .selected_text(&endpoint.method)
                                .show_ui(ui, |ui| {
                                    for method in METHOD_OPTIONS {
                                        if ui
                                            .selectable_label(endpoint.method == method, method)
                                            .clicked()
                                        {
                                            endpoint.method = method.to_owned();
                                            changed = true;
                                        }
                                    }
                                });

                            if ui
                                .add(
                                    TextEdit::singleline(&mut endpoint.url)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("https://${api_host}/resource"),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });

                        ui.separator();
                        ui.label("Headers");
                        for (header_index, header) in endpoint.headers.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut header.key)
                                            .desired_width(f32::INFINITY)
                                            .hint_text("Header"),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut header.value)
                                            .desired_width(f32::INFINITY)
                                            .hint_text("Value"),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                                if ui.button("x").clicked() {
                                    remove_header_index = Some(header_index);
                                }
                            });
                        }

                        if let Some(header_index) = remove_header_index {
                            endpoint.headers.remove(header_index);
                            changed = true;
                        }
                        if ui.button("+ Add Header").clicked() {
                            endpoint.headers.push(KeyValue::default());
                            changed = true;
                        }

                        ui.separator();
                        ui.label("Body");
                        ui.horizontal(|ui| {
                            ui.label("Mode");
                            egui::ComboBox::from_id_salt("body-mode-picker")
                                .selected_text(normalize_body_mode(&endpoint.body_mode))
                                .show_ui(ui, |ui| {
                                    for mode in BODY_MODE_OPTIONS {
                                        if ui
                                            .selectable_label(
                                                normalize_body_mode(&endpoint.body_mode) == mode,
                                                mode,
                                            )
                                            .clicked()
                                        {
                                            endpoint.body_mode = mode.to_owned();
                                            changed = true;
                                        }
                                    }
                                });
                        });
                        if ui
                    .add(
                        TextEdit::multiline(&mut endpoint.body)
                            .desired_width(f32::INFINITY)
                            .desired_rows(12)
                            .hint_text(
                                "{\n  \"token\": \"${token}\"\n}\nOR\nkey=value\nkey2=value2",
                            ),
                    )
                    .changed()
                {
                    changed = true;
                }

                        preview_url = endpoint.url.clone();
                    }

                    if changed {
                        self.mark_dirty();
                    }

                    ui.separator();
                    let resolved_url =
                        resolve_placeholders(&preview_url, &self.selected_environment_variables());
                    ui.label(format!("Resolved URL (preview): {resolved_url}"));
                    if ui.button("Copy cURL for Selected Request").clicked() {
                        self.copy_curl_for_selected_request(ctx);
                    }
                    ui.label("Use ${variable_name} placeholders in URL, headers, and body.");
                });
        });
    }

    fn render_response_panel(&mut self, ctx: &egui::Context) {
        let max_response_height = (ctx.content_rect().height() * 0.60).max(180.0);
        egui::TopBottomPanel::bottom("response")
            .resizable(true)
            .default_height(260.0)
            .min_height(140.0)
            .max_height(max_response_height)
            .show(ctx, |ui| {
                ui.heading("Response");
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(error) = &self.response.error {
                            ui.colored_label(Color32::from_rgb(240, 120, 120), error);
                        }

                        if let Some(status_code) = self.response.status_code {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "Status: {status_code} {}",
                                    self.response.status_text
                                ));
                                if let Some(duration) = self.response.duration_ms {
                                    ui.separator();
                                    ui.label(format!("Time: {duration} ms"));
                                }
                            });
                        } else if !self.response.status_text.is_empty() {
                            ui.label(&self.response.status_text);
                        }

                        if !self.response.headers.is_empty() {
                            ui.collapsing("Headers", |ui| {
                                for header in &self.response.headers {
                                    ui.label(format!("{}: {}", header.key, header.value));
                                }
                            });
                        }

                        ui.separator();
                        let body_height = ui.available_height().max(120.0);
                        ui.add_sized(
                            [ui.available_width(), body_height],
                            TextEdit::multiline(&mut self.response_body_view)
                                .desired_width(f32::INFINITY)
                                .code_editor(),
                        );
                    });
            });
    }

    fn render_status_line(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status-line")
            .exact_height(22.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if self.status_line.is_empty() {
                        ui.label("Ready");
                    } else {
                        ui.label(&self.status_line);
                    }
                    ui.separator();
                    ui.label(format!("Storage: {}", self.storage.base_dir.display()));
                });
            });
    }
}

impl eframe::App for PostmanCloneApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_visuals(egui::Visuals::dark());

        if self.phase != AppPhase::Ready {
            self.render_auth_screen(ctx);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }

        self.poll_response_channel();

        self.render_toolbar(ctx);
        self.render_endpoints_panel(ctx);
        if self.show_environment_panel {
            self.render_environment_panel(ctx);
        }
        self.render_request_editor(ctx);
        self.render_response_panel(ctx);
        self.render_status_line(ctx);

        self.try_auto_save();
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

#[derive(Default, Debug)]
struct ImportSummary {
    directories_scanned: usize,
    files_scanned: usize,
    endpoints_added: usize,
    endpoints_updated: usize,
    environments_added: usize,
    environment_variables_merged: usize,
}

impl ImportSummary {
    fn to_message(&self) -> String {
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
struct ImportScanResult {
    files_scanned: usize,
    endpoints: Vec<Endpoint>,
    environments: Vec<ImportedEnvironment>,
    collection_names_by_id: BTreeMap<String, String>,
    folders_by_id: BTreeMap<String, ImportedFolderMeta>,
}

#[derive(Default, Debug)]
struct WorkspaceImportContext {
    workspace_ids: BTreeSet<String>,
    collection_ids: BTreeSet<String>,
}

#[derive(Debug)]
struct ImportedEnvironment {
    name: String,
    variables: Vec<KeyValue>,
}

#[derive(Default, Debug, Clone)]
struct ImportedFolderMeta {
    name: String,
    parent_folder_id: Option<String>,
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
struct PostmanField {
    key: Option<String>,
    value: Option<serde_json::Value>,
    #[serde(rename = "type")]
    field_type: Option<String>,
    src: Option<serde_json::Value>,
    disabled: Option<bool>,
    enabled: Option<bool>,
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

fn default_endpoints() -> Vec<Endpoint> {
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

fn default_environments() -> Vec<Environment> {
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

fn default_environment_index() -> Vec<EnvironmentIndexEntry> {
    default_environments()
        .into_iter()
        .map(|env| EnvironmentIndexEntry {
            id: env.id,
            name: env.name,
            file_name: env.file_name,
        })
        .collect()
}

fn default_variables_for_environment_name(name: &str) -> Vec<KeyValue> {
    default_environments()
        .into_iter()
        .find(|env| env.name.eq_ignore_ascii_case(name))
        .map(|env| env.variables)
        .unwrap_or_else(Vec::new)
}

fn execute_request(endpoint: Endpoint, env_vars: BTreeMap<String, String>) -> ResponseState {
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

fn request_body_mode_from_data(data: &serde_json::Map<String, serde_json::Value>) -> String {
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

fn build_curl_command(endpoint: &Endpoint, env_vars: &BTreeMap<String, String>) -> String {
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

fn resolve_placeholders(template: &str, env_vars: &BTreeMap<String, String>) -> String {
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

fn normalize_postman_placeholders(input: &str) -> String {
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

fn endpoint_dedup_key(endpoint: &Endpoint) -> String {
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

fn merge_endpoint_details(existing: &mut Endpoint, incoming: Endpoint) -> bool {
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

fn read_json_or_default<T>(path: &Path) -> io::Result<Option<T>>
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

fn write_json_pretty<T>(path: &Path, value: &T) -> io::Result<()>
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

fn create_security_metadata(password: &str) -> Result<(SecurityMetadata, KeyMaterial), String> {
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

fn verify_password(password: &str, metadata: &SecurityMetadata) -> Result<KeyMaterial, String> {
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

fn encrypt_bytes(key: &KeyMaterial, plaintext: &[u8]) -> Result<EncryptedBlob, String> {
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

fn decrypt_bytes(key: &KeyMaterial, blob: &EncryptedBlob) -> Result<Vec<u8>, String> {
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

fn scan_postman_directory(path: &Path, workspace_name_filter: Option<&str>) -> ImportScanResult {
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
    let temp_dir = std::env::temp_dir().join(format!("delivery-man-leveldb-{}", create_id("tmp")));
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

fn extract_import_entities_from_leveldb_binary(
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

fn endpoint_from_cache_object(
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

fn request_url_from_data(data: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
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

fn request_headers_from_data(data: &serde_json::Map<String, serde_json::Value>) -> Vec<KeyValue> {
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

fn request_body_from_data(data: &serde_json::Map<String, serde_json::Value>) -> String {
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

fn non_empty_trimmed(input: &str) -> Option<&str> {
    let value = input.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn resolve_folder_path_from_lookup(
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

fn split_folder_path(input: &str) -> Vec<&str> {
    input
        .split(&['/', '\\'][..])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn safe_path_segment(input: &str) -> String {
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

fn normalize_folder_path(input: &str) -> String {
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

fn render_postman_formdata_fields(fields: &[PostmanField]) -> String {
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

fn default_postman_directories() -> Vec<PathBuf> {
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

fn create_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{prefix}-{}-{}", now.as_secs(), now.subsec_nanos())
}

fn method_color(method: &str) -> Color32 {
    match method {
        "GET" => Color32::from_rgb(97, 175, 239),
        "POST" => Color32::from_rgb(152, 195, 121),
        "PUT" => Color32::from_rgb(229, 192, 123),
        "PATCH" => Color32::from_rgb(198, 120, 221),
        "DELETE" => Color32::from_rgb(224, 108, 117),
        _ => Color32::from_rgb(171, 178, 191),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_substitution_works() {
        let mut vars = BTreeMap::new();
        vars.insert("api_host".to_owned(), "localhost:8080".to_owned());
        vars.insert("token".to_owned(), "abc123".to_owned());

        let output = resolve_placeholders("https://${api_host}/x?token=${token}", &vars);
        assert_eq!(output, "https://localhost:8080/x?token=abc123");
    }

    #[test]
    fn postman_placeholder_conversion_works() {
        let output = normalize_postman_placeholders("https://{{ host }}/x/{{token}}");
        assert_eq!(output, "https://${host}/x/${token}");
    }

    #[test]
    fn encryption_roundtrip_works() {
        let key = [7_u8; 32];
        let payload = b"hello world";

        let encrypted = encrypt_bytes(&key, payload).expect("encryption should succeed");
        let decrypted = decrypt_bytes(&key, &encrypted).expect("decryption should succeed");

        assert_eq!(decrypted, payload);
    }

    #[test]
    fn cache_request_query_params_are_appended_to_url() {
        let payload = serde_json::json!({
            "url": { "raw": "https://api.example.com/v1/items" },
            "queryParams": [
                { "key": "limit", "value": "10" },
                { "key": "token", "value": "{{api_token}}" }
            ]
        });

        let url = request_url_from_data(payload.as_object().expect("payload should be an object"))
            .expect("url should be parsed");

        assert_eq!(
            url,
            "https://api.example.com/v1/items?limit=10&token=${api_token}"
        );
    }

    #[test]
    fn cache_request_body_imports_data_mode_payloads() {
        let payload = serde_json::json!({
            "dataMode": "urlencoded",
            "data": [
                { "key": "email", "value": "dev@example.com" },
                { "key": "password", "value": "{{password}}" }
            ]
        });

        let body =
            request_body_from_data(payload.as_object().expect("payload should be an object"));

        assert_eq!(body, "email=dev@example.com&password=${password}");
    }

    #[test]
    fn cache_request_headers_import_object_shape() {
        let payload = serde_json::json!({
            "headers": {
                "X-Workspace": "{{workspace_id}}",
                "Accept": "application/json"
            }
        });

        let headers =
            request_headers_from_data(payload.as_object().expect("payload should be an object"));

        assert_eq!(headers.len(), 2);
        assert!(
            headers
                .iter()
                .any(|header| header.key == "X-Workspace" && header.value == "${workspace_id}")
        );
        assert!(
            headers
                .iter()
                .any(|header| header.key == "Accept" && header.value == "application/json")
        );
    }

    #[test]
    fn cache_request_auth_bearer_becomes_authorization_header() {
        let payload = serde_json::json!({
            "auth": {
                "type": "bearer",
                "bearer": [
                    { "key": "token", "value": "{{api_token}}" }
                ]
            }
        });

        let headers =
            request_headers_from_data(payload.as_object().expect("payload should be an object"));

        assert!(
            headers.iter().any(
                |header| header.key == "Authorization" && header.value == "Bearer ${api_token}"
            )
        );
    }

    #[test]
    fn cache_request_auth_apikey_query_is_appended_to_url() {
        let payload = serde_json::json!({
            "url": { "raw": "https://api.example.com/search" },
            "auth": {
                "type": "apikey",
                "apikey": [
                    { "key": "key", "value": "api_key" },
                    { "key": "value", "value": "{{token}}" },
                    { "key": "in", "value": "query" }
                ]
            }
        });

        let url = request_url_from_data(payload.as_object().expect("payload should be an object"))
            .expect("url should be parsed");

        assert_eq!(url, "https://api.example.com/search?api_key=${token}");
    }

    #[test]
    fn build_curl_command_resolves_env_and_quotes_values() {
        let endpoint = Endpoint {
            id: "ep-test".to_owned(),
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: "Create".to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: "post".to_owned(),
            url: "https://${api_host}/v1/resource?x=${x}".to_owned(),
            headers: vec![
                KeyValue {
                    key: "Authorization".to_owned(),
                    value: "Bearer ${token}".to_owned(),
                },
                KeyValue {
                    key: "X-Note".to_owned(),
                    value: "it's-live".to_owned(),
                },
            ],
            body_mode: "raw".to_owned(),
            body: "{\"name\":\"${name}\"}".to_owned(),
        };

        let mut vars = BTreeMap::new();
        vars.insert("api_host".to_owned(), "example.com".to_owned());
        vars.insert("x".to_owned(), "1".to_owned());
        vars.insert("token".to_owned(), "abc".to_owned());
        vars.insert("name".to_owned(), "joel".to_owned());

        let curl = build_curl_command(&endpoint, &vars);

        assert!(curl.contains("--request 'POST'"));
        assert!(curl.contains("--url 'https://example.com/v1/resource?x=1'"));
        assert!(curl.contains("--header 'Authorization: Bearer abc'"));
        assert!(curl.contains("--header 'X-Note: it'\\''s-live'"));
        assert!(curl.contains("--data-raw '{\"name\":\"joel\"}'"));
    }

    #[test]
    fn execute_request_rejects_invalid_header_name_with_clear_error() {
        let endpoint = Endpoint {
            id: "ep-test".to_owned(),
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: "Bad Header".to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: "GET".to_owned(),
            url: "https://example.com".to_owned(),
            headers: vec![KeyValue {
                key: "Bad Header".to_owned(),
                value: "Bearer abc".to_owned(),
            }],
            body_mode: "none".to_owned(),
            body: String::new(),
        };

        let output = execute_request(endpoint, BTreeMap::new());
        assert!(
            output
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Invalid header name")
        );
    }

    #[test]
    fn computed_default_content_length_sets_zero_for_empty_post() {
        assert_eq!(
            computed_default_content_length(&Method::POST, false, None, false),
            Some("0".to_owned())
        );
    }

    #[test]
    fn computed_default_content_length_matches_body_size_when_present() {
        let body = "grant_type=client_credentials";
        assert!(
            computed_default_content_length(&Method::POST, false, Some(body.len()), true)
                == Some(body.len().to_string())
        );
    }

    #[test]
    fn default_content_type_is_added_for_body_when_missing() {
        assert!(should_add_default_content_type(true, false));
        assert!(!should_add_default_content_type(true, true));
        assert!(!should_add_default_content_type(false, false));
    }

    #[test]
    fn infer_default_content_type_prefers_json_for_json_bodies() {
        assert_eq!(
            default_content_type_for_mode("raw", "{\"name\":\"joel\"}"),
            Some("application/json")
        );
        assert_eq!(
            default_content_type_for_mode("raw", "   [1,2,3]"),
            Some("application/json")
        );
    }

    #[test]
    fn infer_default_content_type_falls_back_to_text_for_non_json_bodies() {
        assert_eq!(
            default_content_type_for_mode("raw", "grant_type=client_credentials"),
            Some("text/plain")
        );
        assert_eq!(
            default_content_type_for_mode("raw", "plain text"),
            Some("text/plain")
        );
    }

    #[test]
    fn default_content_type_changes_by_body_mode() {
        assert_eq!(
            default_content_type_for_mode("urlencoded", "a=1"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            default_content_type_for_mode("binary", "@/tmp/body.bin"),
            Some("application/octet-stream")
        );
        assert_eq!(default_content_type_for_mode("form-data", "a=1"), None);
    }

    #[test]
    fn normalize_body_mode_handles_aliases() {
        assert_eq!(normalize_body_mode("formdata"), "form-data");
        assert_eq!(normalize_body_mode("multipart/form-data"), "form-data");
        assert_eq!(normalize_body_mode("x-www-form-urlencoded"), "urlencoded");
        assert_eq!(normalize_body_mode("file"), "binary");
        assert_eq!(normalize_body_mode("raw"), "raw");
    }

    #[test]
    fn parse_body_fields_supports_line_and_ampersand_separated_values() {
        let fields = parse_body_fields("a=1&b=2\nc=3");
        assert_eq!(
            fields,
            vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned()),
                ("c".to_owned(), "3".to_owned())
            ]
        );
    }

    #[test]
    fn request_body_mode_from_data_detects_formdata_and_binary() {
        let form_payload = serde_json::json!({
            "body": {
                "mode": "formdata",
                "formdata": [{ "key": "name", "value": "joel" }]
            }
        });
        assert_eq!(
            request_body_mode_from_data(form_payload.as_object().expect("object")),
            "form-data"
        );

        let binary_payload = serde_json::json!({
            "body": {
                "mode": "file",
                "file": { "src": "/tmp/payload.bin" }
            }
        });
        assert_eq!(
            request_body_mode_from_data(binary_payload.as_object().expect("object")),
            "binary"
        );
    }

    #[test]
    fn render_postman_formdata_fields_handles_file_and_text_values() {
        let fields = vec![
            PostmanField {
                key: Some("metadata".to_owned()),
                value: Some(serde_json::Value::String("abc".to_owned())),
                field_type: Some("text".to_owned()),
                src: None,
                disabled: None,
                enabled: None,
            },
            PostmanField {
                key: Some("upload".to_owned()),
                value: None,
                field_type: Some("file".to_owned()),
                src: Some(serde_json::Value::String("/tmp/payload.bin".to_owned())),
                disabled: None,
                enabled: None,
            },
        ];

        assert_eq!(
            render_postman_formdata_fields(&fields),
            "metadata=abc\nupload=@/tmp/payload.bin"
        );
    }

    #[test]
    fn build_curl_command_uses_form_flag_for_form_data_mode() {
        let endpoint = Endpoint {
            id: "ep-test".to_owned(),
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: "Upload".to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: "POST".to_owned(),
            url: "https://example.com/upload".to_owned(),
            headers: vec![],
            body_mode: "form-data".to_owned(),
            body: "name=joel\nfile=@/tmp/payload.bin".to_owned(),
        };
        let curl = build_curl_command(&endpoint, &BTreeMap::new());

        assert!(curl.contains("--form 'name=joel'"));
        assert!(curl.contains("--form 'file=@/tmp/payload.bin'"));
    }

    #[test]
    fn leveldb_binary_extraction_imports_request_model() {
        let payload = serde_json::json!({
            "meta": { "model": "request" },
            "data": {
                "name": "Auth0 Token",
                "method": "POST",
                "url": "https://inspace.au.auth0.com/oauth/token",
                "collection": { "name": "Auth0", "id": "col-1" },
                "headerData": [
                    { "key": "Content-Type", "value": "application/json" }
                ]
            }
        });
        let mut raw = b"binary-prefix".to_vec();
        raw.extend_from_slice(payload.to_string().as_bytes());
        raw.extend_from_slice(b"binary-suffix");

        let mut result = ImportScanResult::default();
        extract_import_entities_from_leveldb_binary(
            &raw,
            &WorkspaceImportContext::default(),
            &mut result,
        );

        assert_eq!(result.endpoints.len(), 1);
        let endpoint = &result.endpoints[0];
        assert_eq!(endpoint.collection, "Auth0");
        assert_eq!(endpoint.method, "POST");
        assert_eq!(endpoint.url, "https://inspace.au.auth0.com/oauth/token");
        assert_eq!(endpoint.headers.len(), 1);
    }

    #[test]
    fn leveldb_binary_extraction_decodes_escaped_json_object() {
        let escaped_payload = r#"{\"meta\":{\"model\":\"request\"},\"data\":{\"name\":\"Escaped\",\"method\":\"GET\",\"url\":\"https://example.com\"}}"#;
        let mut raw = b"prefix".to_vec();
        raw.extend_from_slice(escaped_payload.as_bytes());
        raw.extend_from_slice(b"suffix");

        let mut result = ImportScanResult::default();
        extract_import_entities_from_leveldb_binary(
            &raw,
            &WorkspaceImportContext::default(),
            &mut result,
        );

        assert_eq!(result.endpoints.len(), 1);
        assert_eq!(result.endpoints[0].name, "Escaped");
    }

    #[test]
    fn cache_request_without_collection_id_is_kept_when_filter_is_present() {
        let object = serde_json::json!({
            "meta": { "model": "request" },
            "data": {
                "name": "No Collection",
                "method": "GET",
                "url": "https://example.com/no-collection"
            }
        });

        let mut import_context = WorkspaceImportContext::default();
        import_context.collection_ids.insert("col-keep".to_owned());
        let endpoint =
            endpoint_from_cache_object(object.as_object().expect("object"), &import_context)
                .expect("endpoint should be parsed");

        assert_eq!(endpoint.name, "No Collection");
    }
}
