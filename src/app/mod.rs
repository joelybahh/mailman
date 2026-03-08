pub(crate) mod ui;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::domain::{
    ImportScanResult, ImportSummary, ImportedEnvironment, build_curl_command, clear_session_key,
    create_id, create_security_metadata, default_endpoints, default_environment_index,
    default_environments, default_postman_directories, deserialize_workspace_bundle,
    endpoint_dedup_key, execute_request, load_session_key, merge_endpoint_details,
    normalize_endpoint_url_and_query_params, normalize_folder_path,
    resolve_folder_path_from_lookup, save_session_key, scan_postman_directory,
    serialize_workspace_bundle, verify_password,
};
use crate::models::{
    AppConfig, Endpoint, Environment, EnvironmentIndexEntry, KeyMaterial, KeyValue, ResponseState,
    SecurityMetadata, SharedEnvironment, SharedWorkspacePayload,
};
use crate::request_body::normalize_body_mode_owned;
use crate::storage::{AppStorage, CoreData};

pub(in crate::app) enum AuthResult {
    SetupOk { metadata: SecurityMetadata, key: KeyMaterial },
    UnlockOk(KeyMaterial),
    Err(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum AppPhase {
    SetupPassword,
    UnlockPassword,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum RequestEditorTab {
    Params,
    Headers,
    Body,
    Scripts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ResponseViewTab {
    Raw,
    Pretty,
}

pub(crate) struct MailmanApp {
    pub(in crate::app) storage: AppStorage,
    pub(in crate::app) phase: AppPhase,
    pub(in crate::app) security_metadata: Option<SecurityMetadata>,
    pub(in crate::app) key_material: Option<KeyMaterial>,

    pub(in crate::app) endpoints: Vec<Endpoint>,
    pub(in crate::app) pending_environment_index: Vec<EnvironmentIndexEntry>,
    pub(in crate::app) environments: Vec<Environment>,

    pub(in crate::app) selected_endpoint_id: Option<String>,
    pub(in crate::app) selected_environment_id: Option<String>,
    pub(in crate::app) config: AppConfig,

    pub(in crate::app) setup_password: String,
    pub(in crate::app) setup_password_confirm: String,
    pub(in crate::app) unlock_password: String,
    pub(in crate::app) auth_message: String,

    pub(in crate::app) response: ResponseState,
    pub(in crate::app) response_body_view: String,
    /// Pre-chunked lines for the Raw view. Long lines are split at token
    /// boundaries so each label stays under ~400 chars, making egui's
    /// double-click word-boundary scan instant regardless of response size.
    pub(in crate::app) response_raw_chunks: Vec<String>,
    pub(in crate::app) parsed_response_json: Option<serde_json::Value>,
    pub(in crate::app) parsed_response_json_error: Option<String>,
    pub(in crate::app) response_view_tab: ResponseViewTab,
    pub(in crate::app) status_line: String,
    pub(in crate::app) dirty: bool,
    pub(in crate::app) last_mutation: Instant,
    pub(in crate::app) in_flight: bool,
    pub(in crate::app) response_rx: Option<Receiver<ResponseState>>,

    pub(in crate::app) auth_pending: bool,
    pub(in crate::app) auth_rx: Option<Receiver<AuthResult>>,

    pub(in crate::app) new_environment_name: String,
    pub(in crate::app) postman_import_path: String,
    pub(in crate::app) postman_workspace_filter: String,
    pub(in crate::app) show_environment_panel: bool,
    pub(in crate::app) confirm_delete_all_requests: bool,
    pub(in crate::app) show_export_bundle_dialog: bool,
    pub(in crate::app) export_bundle_password: String,
    pub(in crate::app) export_bundle_password_confirm: String,
    pub(in crate::app) show_postman_import_dialog: bool,
    pub(in crate::app) show_import_bundle_dialog: bool,
    pub(in crate::app) import_bundle_path: Option<PathBuf>,
    pub(in crate::app) import_bundle_password: String,
    pub(in crate::app) request_editor_tab: RequestEditorTab,
    pub(in crate::app) logo_texture: Option<egui::TextureHandle>,
    /// Set to `true` on startup; the sidebar uses it to auto-expand the
    /// collection/folder containing the selected endpoint on first render,
    /// then resets it to `false`.
    pub(in crate::app) expand_to_selection: bool,
    /// Number of script rules that successfully ran against the last response.
    /// Reset to 0 on each new request.
    pub(in crate::app) scripts_ran: usize,
    /// The environment ID that was active when the current request was sent.
    /// Scripts always write to this environment, regardless of any mid-flight
    /// environment switch in the toolbar.
    pub(in crate::app) inflight_environment_id: Option<String>,
}

impl MailmanApp {
    pub(crate) fn new() -> Self {
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

        let mut app = Self {
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
            response_raw_chunks: Vec::new(),
            parsed_response_json: None,
            parsed_response_json_error: None,
            response_view_tab: ResponseViewTab::Pretty,
            status_line,
            dirty: false,
            last_mutation: Instant::now(),
            in_flight: false,
            response_rx: None,
            auth_pending: false,
            auth_rx: None,
            new_environment_name: String::new(),
            postman_import_path: String::new(),
            postman_workspace_filter: String::new(),
            show_environment_panel: false,
            confirm_delete_all_requests: false,
            show_export_bundle_dialog: false,
            export_bundle_password: String::new(),
            export_bundle_password_confirm: String::new(),
            show_postman_import_dialog: false,
            show_import_bundle_dialog: false,
            import_bundle_path: None,
            import_bundle_password: String::new(),
            request_editor_tab: RequestEditorTab::Params,
            logo_texture: None,
            expand_to_selection: true,
            scripts_ran: 0,
            inflight_environment_id: None,
        };

        app.try_auto_session_unlock();
        app
    }

    pub(in crate::app) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_mutation = Instant::now();
    }

    pub(in crate::app) fn lock_workspace(&mut self) {
        // Wipe the key and decrypted environments from memory.
        self.key_material = None;
        self.environments.clear();

        // Drop the in-flight request channel so the worker response is never
        // consumed after locking. This prevents a pre-lock response (and any
        // scripts it would trigger) from mutating state in the next session.
        self.response_rx = None;
        self.in_flight = false;
        self.response = ResponseState::default();
        self.response_body_view.clear();
        self.response_raw_chunks.clear();
        self.parsed_response_json = None;
        self.parsed_response_json_error = None;
        self.scripts_ran = 0;
        self.inflight_environment_id = None;

        // Remove the cached session key from the OS keychain and clear the
        // recorded expiry so the user must unlock again.
        clear_session_key();
        self.config.session_expires_at = None;
        let _ = self.storage.save_config(&self.config);

        self.phase = AppPhase::UnlockPassword;
    }

    pub(in crate::app) fn sync_window_resolution(&mut self, ctx: &egui::Context) {
        let Some(inner_rect) = ctx.input(|input| input.viewport().inner_rect) else {
            return;
        };

        let width = inner_rect.width().max(1.0).round() as u32;
        let height = inner_rect.height().max(1.0).round() as u32;
        if self.config.window_width == Some(width) && self.config.window_height == Some(height) {
            return;
        }

        self.config.window_width = Some(width);
        self.config.window_height = Some(height);

        if self.phase == AppPhase::Ready {
            self.mark_dirty();
        }
    }

    pub(in crate::app) fn ensure_selected_ids(&mut self) {
        if self
            .selected_endpoint_id
            .as_ref()
            .and_then(|id| self.endpoints.iter().find(|item| &item.id == id))
            .is_none()
        {
            self.set_selected_endpoint(self.endpoints.first().map(|item| item.id.clone()));
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

    pub(in crate::app) fn try_auto_save(&mut self) {
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

    pub(in crate::app) fn selected_endpoint_index(&self) -> Option<usize> {
        let selected = self.selected_endpoint_id.as_ref()?;
        self.endpoints.iter().position(|item| &item.id == selected)
    }

    pub(in crate::app) fn selected_environment_index(&self) -> Option<usize> {
        let selected = self.selected_environment_id.as_ref()?;
        self.environments
            .iter()
            .position(|item| &item.id == selected)
    }

    pub(in crate::app) fn selected_environment_variables(&self) -> BTreeMap<String, String> {
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

    pub(in crate::app) fn reset_response_ui_for_request_switch(&mut self) {
        self.response = ResponseState::default();
        self.response_body_view.clear();
        self.response_raw_chunks.clear();
        self.parsed_response_json = None;
        self.parsed_response_json_error = None;
        self.in_flight = false;
        self.response_rx = None;
    }

    pub(in crate::app) fn set_selected_endpoint(&mut self, endpoint_id: Option<String>) {
        if self.selected_endpoint_id == endpoint_id {
            return;
        }

        self.selected_endpoint_id = endpoint_id;
        self.reset_response_ui_for_request_switch();
    }

    pub(in crate::app) fn handle_setup_password_submission(&mut self) {
        if self.auth_pending {
            return;
        }

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

        let (tx, rx): (Sender<AuthResult>, Receiver<AuthResult>) = mpsc::channel();

        self.auth_pending = true;
        self.auth_rx = Some(rx);
        self.auth_message = String::new();

        thread::spawn(move || {
            let result = match create_security_metadata(&password) {
                Ok((metadata, key)) => AuthResult::SetupOk { metadata, key },
                Err(err) => AuthResult::Err(format!("Failed to configure encryption: {err}")),
            };
            let _ = tx.send(result);
        });
    }

    pub(in crate::app) fn handle_unlock_password_submission(&mut self) {
        if self.auth_pending {
            return;
        }

        let Some(metadata) = self.security_metadata.clone() else {
            self.auth_message = "Missing security metadata; restart app.".to_owned();
            return;
        };

        let password = self.unlock_password.clone();
        let (tx, rx): (Sender<AuthResult>, Receiver<AuthResult>) = mpsc::channel();

        self.auth_pending = true;
        self.auth_rx = Some(rx);
        self.auth_message = String::new();

        thread::spawn(move || {
            let result = match verify_password(&password, &metadata) {
                Ok(key) => AuthResult::UnlockOk(key),
                Err(err) => AuthResult::Err(format!("Invalid password: {err}")),
            };
            let _ = tx.send(result);
        });
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

        // Save or clear the session key in the OS keychain based on the user's
        // current session-duration preference.
        match self.config.session_duration_days {
            None => {
                // "Always ask" — remove any previously cached key.
                clear_session_key();
                self.config.session_expires_at = None;
            }
            Some(duration_days) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let expires_at = if duration_days == 0 {
                    u64::MAX // forever
                } else {
                    now.saturating_add(duration_days as u64 * 86_400)
                };
                if let Err(err) = save_session_key(&key) {
                    eprintln!("Failed to save session key to keychain: {err}");
                } else {
                    self.config.session_expires_at = Some(expires_at);
                    let _ = self.storage.save_config(&self.config);
                }
            }
        }

        Ok(())
    }

    /// Called once during `new()`. If a valid, unexpired session key exists in
    /// the OS keychain, unlock the workspace automatically so the user skips the
    /// password prompt.
    fn try_auto_session_unlock(&mut self) {
        if self.phase != AppPhase::UnlockPassword {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match self.config.session_expires_at {
            None => return, // No active session recorded.
            Some(expires_at) if expires_at != u64::MAX && expires_at < now => {
                // Session has expired — discard it.
                self.config.session_expires_at = None;
                clear_session_key();
                let _ = self.storage.save_config(&self.config);
                return;
            }
            _ => {} // Valid session (forever or not yet expired).
        }

        match load_session_key() {
            Ok(key) => {
                if let Err(err) = self.complete_unlock(key) {
                    // Key was stale or environments changed — fall back gracefully.
                    eprintln!("Cached session key rejected: {err}");
                    self.config.session_expires_at = None;
                    clear_session_key();
                    let _ = self.storage.save_config(&self.config);
                } else {
                    self.phase = AppPhase::Ready;
                }
            }
            Err(_) => {
                // Entry not in keychain (e.g. user deleted it) — clear stale metadata.
                self.config.session_expires_at = None;
                let _ = self.storage.save_config(&self.config);
            }
        }
    }

    pub(in crate::app) fn add_endpoint(&mut self) {
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
            query_params: vec![],
            headers: vec![],
            body_mode: "none".to_owned(),
            body: String::new(),
            scripts: vec![],
        });
        self.set_selected_endpoint(Some(id));
        self.mark_dirty();
    }

    pub(in crate::app) fn delete_selected_endpoint(&mut self) {
        let Some(index) = self.selected_endpoint_index() else {
            return;
        };
        self.endpoints.remove(index);
        self.set_selected_endpoint(self.endpoints.first().map(|item| item.id.clone()));
        self.mark_dirty();
    }

    pub(in crate::app) fn delete_all_requests(&mut self) {
        let removed = self.endpoints.len();
        self.endpoints.clear();
        self.set_selected_endpoint(None);
        self.config.selected_endpoint_id = None;
        self.mark_dirty();
        self.status_line = format!("Cleared {removed} requests.");
    }

    pub(in crate::app) fn add_environment(&mut self, name: String) {
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

    pub(in crate::app) fn delete_selected_environment(&mut self) {
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

    pub(in crate::app) fn send_selected_request(&mut self) {
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
        self.scripts_ran = 0;
        self.inflight_environment_id = self.selected_environment_id.clone();
        self.response.clear_for_request();
        self.response_body_view.clear();
        self.response_raw_chunks.clear();
        self.parsed_response_json = None;
        self.parsed_response_json_error = None;
        self.response_rx = Some(rx);
        self.status_line = format!("Sending {} {}", endpoint.method, endpoint.url);

        thread::spawn(move || {
            let state = execute_request(endpoint, env_vars);
            let _ = tx.send(state);
        });
    }

    pub(in crate::app) fn copy_curl_for_selected_request(&mut self, ctx: &egui::Context) {
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

    pub(in crate::app) fn poll_response_channel(&mut self) {
        let Some(rx) = &self.response_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(response_state) => {
                self.response = response_state;
                self.response_body_view = self.response.body.clone();
                self.response_raw_chunks = chunk_response_body(&self.response_body_view);
                self.update_parsed_response_json();
                self.run_response_scripts();
                self.in_flight = false;
                self.response_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.in_flight = false;
                self.response_rx = None;
                self.response.error = Some("Request worker disconnected.".to_owned());
                self.response_body_view.clear();
                self.response_raw_chunks.clear();
                self.parsed_response_json = None;
                self.parsed_response_json_error = None;
            }
        }
    }

    pub(in crate::app) fn poll_auth_channel(&mut self) {
        let Some(rx) = &self.auth_rx else {
            return;
        };

        match rx.try_recv() {
            Ok(result) => {
                self.auth_pending = false;
                self.auth_rx = None;

                match result {
                    AuthResult::UnlockOk(key) => {
                        self.unlock_password.clear();
                        if let Err(err) = self.complete_unlock(key) {
                            self.auth_message = err;
                            return;
                        }
                        self.auth_message = String::new();
                        self.phase = AppPhase::Ready;
                    }
                    AuthResult::SetupOk { metadata, key } => {
                        if let Err(err) = self.storage.save_security_metadata(&metadata) {
                            self.auth_message =
                                format!("Failed to persist security metadata: {err}");
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
                    AuthResult::Err(msg) => {
                        self.auth_message = msg;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.auth_pending = false;
                self.auth_rx = None;
                self.auth_message = "Authentication worker disconnected unexpectedly.".to_owned();
            }
        }
    }

    pub(in crate::app) fn update_parsed_response_json(&mut self) {
        let body = self.response.body.trim();
        if body.is_empty() {
            self.parsed_response_json = None;
            self.parsed_response_json_error = None;
            return;
        }

        match serde_json::from_str::<serde_json::Value>(body) {
            Ok(value) => {
                self.parsed_response_json = Some(value);
                self.parsed_response_json_error = None;
            }
            Err(err) => {
                self.parsed_response_json = None;
                self.parsed_response_json_error = Some(err.to_string());
            }
        }
    }

    pub(in crate::app) fn run_response_scripts(&mut self) {
        // Only fire on 2xx responses.
        let Some(status) = self.response.status_code else { return };
        if !(200..300).contains(&status) { return }

        let scripts = self
            .selected_endpoint_index()
            .map(|i| self.endpoints[i].scripts.clone())
            .unwrap_or_default();
        if scripts.is_empty() { return }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(&self.response.body) else {
            return;
        };

        // Resolve against the environment that was active at send time, not
        // the current selection — the user may have switched mid-flight.
        let Some(target_id) = self.inflight_environment_id.clone() else { return };
        let Some(env_idx) = self.environments.iter().position(|e| e.id == target_id) else {
            return;
        };

        let mut ran = 0usize;
        for script in &scripts {
            let path = script.extract_key.trim();
            let var  = script.env_var.trim();
            if path.is_empty() || var.is_empty() { continue }

            let Some(extracted) = json_path_extract(&json, path) else { continue };

            let env = &mut self.environments[env_idx];
            if let Some(kv) = env.variables.iter_mut().find(|kv| kv.key == var) {
                kv.value = extracted;
            } else {
                env.variables.push(KeyValue { key: var.to_owned(), value: extracted });
            }
            ran += 1;
        }

        if ran > 0 {
            self.scripts_ran = ran;
            self.mark_dirty();
        }
    }

    pub(in crate::app) fn import_postman_from_defaults(
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

    pub(in crate::app) fn import_postman_from_path(
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
            normalize_endpoint_url_and_query_params(&mut endpoint);
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

    pub(in crate::app) fn export_workspace_bundle_to_path(
        &self,
        path: &Path,
        password: &str,
    ) -> Result<(usize, usize), String> {
        let payload = SharedWorkspacePayload {
            version: 1,
            endpoints: self.endpoints.clone(),
            environments: self
                .environments
                .iter()
                .map(|env| SharedEnvironment {
                    name: env.name.clone(),
                    variables: env.variables.clone(),
                })
                .collect(),
        };

        let encoded = serialize_workspace_bundle(&payload, password)?;
        fs::write(path, encoded).map_err(|err| format!("Failed to write bundle: {err}"))?;

        Ok((payload.endpoints.len(), payload.environments.len()))
    }

    pub(in crate::app) fn import_workspace_bundle_from_path(
        &mut self,
        path: &Path,
        password: &str,
    ) -> Result<ImportSummary, String> {
        let raw =
            fs::read_to_string(path).map_err(|err| format!("Failed to read bundle: {err}"))?;
        let payload = deserialize_workspace_bundle(&raw, password)?;

        if payload.endpoints.is_empty() && payload.environments.is_empty() {
            return Err("Bundle contains no endpoints or environments.".to_owned());
        }

        let mut seen_ids = self
            .endpoints
            .iter()
            .map(|endpoint| endpoint.id.clone())
            .collect::<BTreeSet<_>>();

        let mut scan_result = ImportScanResult::default();
        scan_result.endpoints = payload
            .endpoints
            .into_iter()
            .map(|mut endpoint| {
                endpoint.collection = if endpoint.collection.trim().is_empty() {
                    "General".to_owned()
                } else {
                    endpoint.collection.trim().to_owned()
                };
                endpoint.folder_path = normalize_folder_path(&endpoint.folder_path);
                endpoint.body_mode = normalize_body_mode_owned(&endpoint.body_mode);
                normalize_endpoint_url_and_query_params(&mut endpoint);
                if endpoint.body_mode == "raw" && endpoint.body.is_empty() {
                    endpoint.body_mode = "none".to_owned();
                }

                if endpoint.id.trim().is_empty() || seen_ids.contains(endpoint.id.trim()) {
                    endpoint.id = create_id("ep");
                }
                seen_ids.insert(endpoint.id.clone());

                endpoint
            })
            .collect();
        scan_result.environments = payload
            .environments
            .into_iter()
            .map(|environment| ImportedEnvironment {
                name: environment.name,
                variables: environment.variables,
            })
            .collect();

        Ok(self.merge_postman_import(scan_result))
    }
}

/// Split a response body into display chunks so that no single egui label
/// exceeds `MAX_CHUNK` bytes. Short lines pass through unchanged. Long lines
/// (e.g. minified JSON) are broken at token-boundary characters — whitespace,
/// commas, and JSON structural chars — so double-click word selection stays
/// O(chunk_size) rather than O(line_length).
fn chunk_response_body(body: &str) -> Vec<String> {
    const MAX_CHUNK: usize = 400;

    let mut chunks = Vec::new();
    for line in body.lines() {
        if line.len() <= MAX_CHUNK {
            chunks.push(line.to_owned());
        } else {
            let mut remaining = line;
            while remaining.len() > MAX_CHUNK {
                let split = find_chunk_split(remaining, MAX_CHUNK);
                chunks.push(remaining[..split].to_owned());
                remaining = &remaining[split..];
            }
            if !remaining.is_empty() {
                chunks.push(remaining.to_owned());
            }
        }
    }
    chunks
}

/// Return a byte offset at or before `target` where it is safe to split `s`.
/// Prefers breaking after a token-boundary character; falls back to any valid
/// UTF-8 char boundary.
fn find_chunk_split(s: &str, target: usize) -> usize {
    let end = target.min(s.len());
    let bytes = s.as_bytes();

    // Walk backwards from `end` to find a token boundary.
    for i in (1..=end).rev() {
        if !s.is_char_boundary(i) {
            continue;
        }
        let prev = bytes[i - 1];
        if matches!(prev, b' ' | b'\t' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'&' | b'=') {
            return i;
        }
    }

    // No boundary found — split at the last valid char boundary.
    let mut i = end;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i.max(1)
}

/// Walk a dot-notation path (e.g. `"data.access_token"` or `"items.0.id"`)
/// into a `serde_json::Value` and return the leaf as a plain string.
fn json_path_extract(json: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = json;
    for segment in path.split('.') {
        current = if let Ok(idx) = segment.parse::<usize>() {
            current.get(idx)?
        } else {
            current.get(segment)?
        };
    }
    Some(match current {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b)   => b.to_string(),
        serde_json::Value::Null      => "null".to_owned(),
        other                        => other.to_string(),
    })
}
