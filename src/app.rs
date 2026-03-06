use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, TextEdit};

use crate::domain::{
    ImportScanResult, ImportSummary, ImportedEnvironment, build_curl_command, create_id,
    create_security_metadata, default_endpoints, default_environment_index, default_environments,
    default_postman_directories, deserialize_workspace_bundle, endpoint_dedup_key, execute_request,
    merge_endpoint_details, method_color, non_empty_trimmed,
    normalize_endpoint_url_and_query_params, normalize_folder_path, resolve_endpoint_url,
    resolve_folder_path_from_lookup, scan_postman_directory, serialize_workspace_bundle,
    verify_password,
};
use crate::models::{
    AppConfig, BODY_MODE_OPTIONS, Endpoint, Environment, EnvironmentIndexEntry, KeyMaterial,
    KeyValue, METHOD_OPTIONS, ResponseState, SecurityMetadata, SharedEnvironment,
    SharedWorkspacePayload,
};
use crate::request_body::{normalize_body_mode, normalize_body_mode_owned};
use crate::storage::{AppStorage, CoreData};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppPhase {
    SetupPassword,
    UnlockPassword,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestEditorTab {
    Params,
    Headers,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseViewTab {
    Raw,
    Pretty,
}

pub(crate) struct MailmanApp {
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
    parsed_response_json: Option<serde_json::Value>,
    parsed_response_json_error: Option<String>,
    response_view_tab: ResponseViewTab,
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
    show_export_bundle_dialog: bool,
    export_bundle_password: String,
    export_bundle_password_confirm: String,
    show_postman_import_dialog: bool,
    show_import_bundle_dialog: bool,
    import_bundle_path: Option<PathBuf>,
    import_bundle_password: String,
    request_editor_tab: RequestEditorTab,
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
            parsed_response_json: None,
            parsed_response_json_error: None,
            response_view_tab: ResponseViewTab::Raw,
            status_line,
            dirty: false,
            last_mutation: Instant::now(),
            in_flight: false,
            response_rx: None,
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
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_mutation = Instant::now();
    }

    fn attach_text_context_menu(response: &egui::Response, current_text: &str, editable: bool) {
        let text_edit_id = response.id;
        let text_char_count = current_text.chars().count();
        let selection_backup_id = text_edit_id.with("selection_backup");

        if let Some(state) = egui::TextEdit::load_state(&response.ctx, text_edit_id)
            && let Some(range) = state.cursor.char_range()
            && !range.is_empty()
            && !response.secondary_clicked()
        {
            response
                .ctx
                .data_mut(|data| data.insert_temp(selection_backup_id, range));
        }

        if response.secondary_clicked()
            && let Some(saved_range) = response
                .ctx
                .data(|data| data.get_temp::<egui::text::CCursorRange>(selection_backup_id))
            && let Some(mut state) = egui::TextEdit::load_state(&response.ctx, text_edit_id)
        {
            state.cursor.set_char_range(Some(saved_range));
            egui::TextEdit::store_state(&response.ctx, text_edit_id, state);
        }

        response.context_menu(move |ui| {
            if editable && ui.button("Cut").clicked() {
                ui.ctx()
                    .memory_mut(|memory| memory.request_focus(text_edit_id));
                ui.ctx()
                    .input_mut(|input| input.events.push(egui::Event::Cut));
                ui.close();
            }

            if ui.button("Copy").clicked() {
                ui.ctx()
                    .memory_mut(|memory| memory.request_focus(text_edit_id));
                ui.ctx()
                    .input_mut(|input| input.events.push(egui::Event::Copy));
                ui.close();
            }

            if editable && ui.button("Paste").clicked() {
                ui.ctx()
                    .memory_mut(|memory| memory.request_focus(text_edit_id));
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                ui.close();
            }

            if ui.button("Select All").clicked() {
                ui.ctx()
                    .memory_mut(|memory| memory.request_focus(text_edit_id));
                let mut state =
                    egui::TextEdit::load_state(ui.ctx(), text_edit_id).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(text_char_count),
                    )));
                egui::TextEdit::store_state(ui.ctx(), text_edit_id, state);
                ui.close();
            }
        });
    }

    fn sync_window_resolution(&mut self, ctx: &egui::Context) {
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

    fn ensure_selected_ids(&mut self) {
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

    fn reset_response_ui_for_request_switch(&mut self) {
        self.response = ResponseState::default();
        self.response_body_view.clear();
        self.parsed_response_json = None;
        self.parsed_response_json_error = None;
        self.in_flight = false;
        self.response_rx = None;
    }

    fn set_selected_endpoint(&mut self, endpoint_id: Option<String>) {
        if self.selected_endpoint_id == endpoint_id {
            return;
        }

        self.selected_endpoint_id = endpoint_id;
        self.reset_response_ui_for_request_switch();
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
            query_params: vec![],
            headers: vec![],
            body_mode: "none".to_owned(),
            body: String::new(),
        });
        self.set_selected_endpoint(Some(id));
        self.mark_dirty();
    }

    fn delete_selected_endpoint(&mut self) {
        let Some(index) = self.selected_endpoint_index() else {
            return;
        };
        self.endpoints.remove(index);
        self.set_selected_endpoint(self.endpoints.first().map(|item| item.id.clone()));
        self.mark_dirty();
    }

    fn delete_all_requests(&mut self) {
        let removed = self.endpoints.len();
        self.endpoints.clear();
        self.set_selected_endpoint(None);
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
        self.parsed_response_json = None;
        self.parsed_response_json_error = None;
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
                self.update_parsed_response_json();
                self.in_flight = false;
                self.response_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.in_flight = false;
                self.response_rx = None;
                self.response.error = Some("Request worker disconnected.".to_owned());
                self.response_body_view.clear();
                self.parsed_response_json = None;
                self.parsed_response_json_error = None;
            }
        }
    }

    fn update_parsed_response_json(&mut self) {
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

    fn render_json_leaf(
        ui: &mut egui::Ui,
        key: Option<&str>,
        path: &str,
        value_text: impl Into<String>,
        value_color: Color32,
    ) {
        let rendered_text = value_text.into();
        ui.horizontal_wrapped(|ui| {
            if let Some(key) = key {
                ui.label(RichText::new(key).strong());
                ui.label(":");
            }
            let response = ui.add(
                egui::Label::new(
                    RichText::new(rendered_text.clone())
                        .color(value_color)
                        .monospace(),
                )
                .sense(egui::Sense::click()),
            );

            if response.double_clicked() {
                ui.ctx().copy_text(rendered_text.clone());
            }
            response.context_menu(|ui| {
                if ui.button("Copy Value").clicked() {
                    ui.ctx().copy_text(rendered_text.clone());
                    ui.close();
                }
                if ui.button("Copy JSON Path").clicked() {
                    ui.ctx().copy_text(path.to_owned());
                    ui.close();
                }
            });
        });
    }

    fn render_json_tree(
        ui: &mut egui::Ui,
        key: Option<&str>,
        value: &serde_json::Value,
        path: &str,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                let label = match key {
                    Some(key) => format!("{key}: Object ({})", map.len()),
                    None => format!("Object ({})", map.len()),
                };
                egui::CollapsingHeader::new(label)
                    .id_salt(path)
                    .default_open(path == "$")
                    .show(ui, |ui| {
                        for (child_key, child_value) in map {
                            let child_path = format!("{path}.{child_key}");
                            Self::render_json_tree(ui, Some(child_key), child_value, &child_path);
                        }
                    });
            }
            serde_json::Value::Array(items) => {
                let label = match key {
                    Some(key) => format!("{key}: Array ({})", items.len()),
                    None => format!("Array ({})", items.len()),
                };
                egui::CollapsingHeader::new(label)
                    .id_salt(path)
                    .default_open(path == "$")
                    .show(ui, |ui| {
                        for (index, item) in items.iter().enumerate() {
                            let child_key = format!("[{index}]");
                            let child_path = format!("{path}[{index}]");
                            Self::render_json_tree(ui, Some(&child_key), item, &child_path);
                        }
                    });
            }
            serde_json::Value::String(text) => {
                Self::render_json_leaf(
                    ui,
                    key,
                    path,
                    format!("\"{text}\""),
                    Color32::from_rgb(120, 210, 170),
                );
            }
            serde_json::Value::Number(number) => {
                Self::render_json_leaf(
                    ui,
                    key,
                    path,
                    number.to_string(),
                    Color32::from_rgb(240, 200, 120),
                );
            }
            serde_json::Value::Bool(value) => {
                Self::render_json_leaf(
                    ui,
                    key,
                    path,
                    value.to_string(),
                    Color32::from_rgb(130, 180, 255),
                );
            }
            serde_json::Value::Null => {
                Self::render_json_leaf(ui, key, path, "null", Color32::GRAY);
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

    fn export_workspace_bundle_to_path(
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

    fn import_workspace_bundle_from_path(
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

    fn render_share_bundle_dialogs(&mut self, ctx: &egui::Context) {
        if self.show_export_bundle_dialog {
            let mut open = self.show_export_bundle_dialog;
            let mut should_close = false;

            egui::Window::new("Export Workspace Bundle")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Create a password-protected bundle that can be imported on any OS.");
                    let response = ui.add(
                        TextEdit::singleline(&mut self.export_bundle_password)
                            .password(true)
                            .hint_text("Bundle password"),
                    );
                    Self::attach_text_context_menu(&response, &self.export_bundle_password, true);
                    let response = ui.add(
                        TextEdit::singleline(&mut self.export_bundle_password_confirm)
                            .password(true)
                            .hint_text("Confirm bundle password"),
                    );
                    Self::attach_text_context_menu(
                        &response,
                        &self.export_bundle_password_confirm,
                        true,
                    );

                    ui.horizontal(|ui| {
                        if ui.button("Export").clicked() {
                            let password = self.export_bundle_password.trim().to_owned();
                            let confirm = self.export_bundle_password_confirm.trim().to_owned();

                            if password.len() < 8 {
                                self.status_line =
                                    "Bundle password must be at least 8 characters.".to_owned();
                                return;
                            }
                            if password != confirm {
                                self.status_line =
                                    "Bundle password confirmation does not match.".to_owned();
                                return;
                            }

                            let save_target = rfd::FileDialog::new()
                                .set_title("Export Mail Man Bundle")
                                .set_file_name("mailman-workspace.mmbundle")
                                .add_filter("Mail Man Bundle", &["mmbundle", "mailmanbundle"])
                                .save_file();

                            let Some(path) = save_target else {
                                self.status_line = "Bundle export canceled.".to_owned();
                                return;
                            };

                            match self.export_workspace_bundle_to_path(&path, &password) {
                                Ok((endpoint_count, environment_count)) => {
                                    self.status_line = format!(
                                        "Bundle exported: {} endpoints and {} environments to {}",
                                        endpoint_count,
                                        environment_count,
                                        path.display()
                                    );
                                    should_close = true;
                                }
                                Err(err) => {
                                    self.status_line = format!("Bundle export failed: {err}");
                                }
                            }
                        }

                        if ui.button("Cancel").clicked() {
                            should_close = true;
                        }
                    });
                });

            self.show_export_bundle_dialog = open && !should_close;
        }

        if self.show_import_bundle_dialog {
            let mut open = self.show_import_bundle_dialog;
            let mut should_close = false;
            let selected_path = self.import_bundle_path.clone();

            egui::Window::new("Import Workspace Bundle")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    if let Some(path) = selected_path.as_ref() {
                        ui.label(format!("File: {}", path.display()));
                    } else {
                        ui.colored_label(Color32::from_rgb(240, 120, 120), "No bundle file selected.");
                    }

                    let response = ui.add(
                        TextEdit::singleline(&mut self.import_bundle_password)
                            .password(true)
                            .hint_text("Bundle password"),
                    );
                    Self::attach_text_context_menu(&response, &self.import_bundle_password, true);

                    ui.horizontal(|ui| {
                        if ui.button("Import").clicked() {
                            let Some(path) = selected_path.as_ref() else {
                                self.status_line = "Select a bundle file first.".to_owned();
                                return;
                            };

                            let password = self.import_bundle_password.trim().to_owned();
                            match self.import_workspace_bundle_from_path(path, &password) {
                                Ok(summary) => {
                                    self.status_line = format!(
                                        "Bundle import: added {} endpoints, updated {} endpoints, added {} environments, merged {} environment vars.",
                                        summary.endpoints_added,
                                        summary.endpoints_updated,
                                        summary.environments_added,
                                        summary.environment_variables_merged,
                                    );
                                    self.last_mutation = Instant::now() - Duration::from_secs(1);
                                    self.try_auto_save();
                                    should_close = true;
                                }
                                Err(err) => {
                                    self.status_line = format!("Bundle import failed: {err}");
                                }
                            }
                        }

                        if ui.button("Cancel").clicked() {
                            should_close = true;
                        }
                    });
                });

            self.show_import_bundle_dialog = open && !should_close;
            if should_close {
                self.import_bundle_path = None;
                self.import_bundle_password.clear();
            }
        }
    }

    fn render_postman_import_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_postman_import_dialog {
            return;
        }

        let mut open = self.show_postman_import_dialog;
        let mut should_close = false;

        egui::Window::new("Import From Postman")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label("Import requests and environments from Postman local data.");
                let response = ui.add(
                    TextEdit::singleline(&mut self.postman_workspace_filter)
                        .hint_text("Workspace filter (optional)"),
                );
                Self::attach_text_context_menu(&response, &self.postman_workspace_filter, true);

                ui.horizontal(|ui| {
                    let response = ui.add(
                        TextEdit::singleline(&mut self.postman_import_path)
                            .desired_width(360.0)
                            .hint_text("/path/to/Postman"),
                    );
                    Self::attach_text_context_menu(&response, &self.postman_import_path, true);
                    if ui.button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Select Postman Directory")
                            .pick_folder()
                        {
                            self.postman_import_path = path.display().to_string();
                        }
                    }
                });

                ui.horizontal(|ui| {
                    if ui.button("Auto Detect").clicked() {
                        let workspace_filter =
                            non_empty_trimmed(&self.postman_workspace_filter).map(str::to_owned);
                        let summary =
                            self.import_postman_from_defaults(workspace_filter.as_deref());
                        self.status_line = summary.to_message();
                        should_close = true;
                    }

                    if ui.button("Import Path").clicked() {
                        let raw_path = self.postman_import_path.trim();
                        if raw_path.is_empty() {
                            self.status_line = "Enter a Postman path first.".to_owned();
                            return;
                        }

                        let path = PathBuf::from(raw_path);
                        let workspace_filter =
                            non_empty_trimmed(&self.postman_workspace_filter).map(str::to_owned);
                        let summary =
                            self.import_postman_from_path(&path, workspace_filter.as_deref());
                        self.status_line = summary.to_message();
                        should_close = true;
                    }

                    if ui.button("Cancel").clicked() {
                        should_close = true;
                    }
                });
            });

        self.show_postman_import_dialog = open && !should_close;
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

                        let response = ui.add(
                            TextEdit::singleline(&mut self.setup_password)
                                .password(true)
                                .hint_text("Master password"),
                        );
                        Self::attach_text_context_menu(&response, &self.setup_password, true);
                        let response = ui.add(
                            TextEdit::singleline(&mut self.setup_password_confirm)
                                .password(true)
                                .hint_text("Confirm password"),
                        );
                        Self::attach_text_context_menu(
                            &response,
                            &self.setup_password_confirm,
                            true,
                        );

                        if ui.button("Configure Encryption and Open").clicked() {
                            self.handle_setup_password_submission();
                        }
                    }
                    AppPhase::UnlockPassword => {
                        ui.label("Enter your master password to decrypt environment variables.");
                        ui.add_space(12.0);
                        let response = ui.add(
                            TextEdit::singleline(&mut self.unlock_password)
                                .password(true)
                                .hint_text("Master password"),
                        );
                        Self::attach_text_context_menu(&response, &self.unlock_password, true);

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
            ui.horizontal(|ui| {
                ui.label(RichText::new("Mail Man").strong().size(18.0));
                ui.separator();

                ui.menu_button("Import", |ui| {
                    if ui.button("Postman...").clicked() {
                        self.show_postman_import_dialog = true;
                        ui.close();
                    }

                    if ui.button("Bundle...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Import Mail Man Bundle")
                            .add_filter("Mail Man Bundle", &["mmbundle", "mailmanbundle", "json"])
                            .pick_file()
                        {
                            self.import_bundle_path = Some(path);
                            self.import_bundle_password.clear();
                            self.show_import_bundle_dialog = true;
                        }
                        ui.close();
                    }
                });

                if ui.button("Export Bundle").clicked() {
                    self.show_export_bundle_dialog = true;
                    self.export_bundle_password.clear();
                    self.export_bundle_password_confirm.clear();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.show_environment_panel && ui.button("Env Settings").clicked() {
                        self.show_environment_panel = true;
                    }

                    let selected_name = self
                        .selected_environment_index()
                        .and_then(|idx| self.environments.get(idx))
                        .map(|env| env.name.as_str())
                        .unwrap_or("None");

                    let mut selection_changed = false;
                    egui::ComboBox::from_id_salt("environment-switcher")
                        .selected_text(selected_name)
                        .show_ui(ui, |ui| {
                            for env in &self.environments {
                                let selected = self.selected_environment_id.as_deref()
                                    == Some(env.id.as_str());
                                if ui.selectable_label(selected, &env.name).clicked() {
                                    self.selected_environment_id = Some(env.id.clone());
                                    selection_changed = true;
                                }
                            }
                        });
                    if selection_changed {
                        self.mark_dirty();
                    }
                    ui.label("Environment:");
                });
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
                    ui.separator();
                    if !self.confirm_delete_all_requests {
                        if ui.button("Delete All").clicked() {
                            self.confirm_delete_all_requests = true;
                        }
                    } else {
                        ui.colored_label(Color32::from_rgb(240, 120, 120), "Confirm clear all?");
                        if ui.button("Yes").clicked() {
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
                                        let endpoint_id = endpoint.id.clone();
                                        let endpoint_method = endpoint.method.clone();
                                        let endpoint_name = endpoint.name.clone();
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                method_color(&endpoint_method),
                                                &endpoint_method,
                                            );
                                            if ui
                                                .selectable_label(is_selected, &endpoint_name)
                                                .clicked()
                                            {
                                                self.set_selected_endpoint(Some(
                                                    endpoint_id.clone(),
                                                ));
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
                                            let endpoint_id = endpoint.id.clone();
                                            let endpoint_method = endpoint.method.clone();
                                            let endpoint_name = endpoint.name.clone();
                                            ui.horizontal(|ui| {
                                                ui.colored_label(
                                                    method_color(&endpoint_method),
                                                    &endpoint_method,
                                                );
                                                if ui
                                                    .selectable_label(is_selected, &endpoint_name)
                                                    .clicked()
                                                {
                                                    self.set_selected_endpoint(Some(
                                                        endpoint_id.clone(),
                                                    ));
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
                ui.horizontal(|ui| {
                    ui.heading("Environment Variables");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_environment_panel = false;
                        }
                    });
                });
                ui.label("Each environment is stored in its own encrypted offline file.");
                ui.separator();

                ui.horizontal(|ui| {
                    let response = ui.add(
                        TextEdit::singleline(&mut self.new_environment_name)
                            .desired_width(160.0)
                            .hint_text("new env (qa, sandbox)"),
                    );
                    Self::attach_text_context_menu(&response, &self.new_environment_name, true);
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
                });
                ui.separator();

                let Some(index) = self.selected_environment_index() else {
                    ui.label("No environment selected.");
                    return;
                };

                let mut changed = false;
                let mut remove_index: Option<usize> = None;

                {
                    let env = &mut self.environments[index];

                    let response = ui.text_edit_singleline(&mut env.name);
                    Self::attach_text_context_menu(&response, &env.name, true);
                    if response.changed() {
                        changed = true;
                    }
                    ui.label(format!("File: {}", env.file_name));
                    ui.separator();

                    for (variable_index, variable) in env.variables.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let response =
                                ui.add(TextEdit::singleline(&mut variable.key).hint_text("key"));
                            Self::attach_text_context_menu(&response, &variable.key, true);
                            if response.changed() {
                                changed = true;
                            }
                            let response = ui
                                .add(TextEdit::singleline(&mut variable.value).hint_text("value"));
                            Self::attach_text_context_menu(&response, &variable.value, true);
                            if response.changed() {
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
                    ui.horizontal(|ui| {
                        ui.heading("Request Builder");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Copy cURL").clicked() {
                                self.copy_curl_for_selected_request(ctx);
                            }

                            let send_button = ui.add_enabled(
                                !self.in_flight,
                                egui::Button::new(if self.in_flight { "Sending..." } else { "Send" }),
                            );
                            if send_button.clicked() {
                                self.send_selected_request();
                            }
                        });
                    });
                    ui.separator();

                    let Some(index) = self.selected_endpoint_index() else {
                        ui.label("Select a request from the left panel.");
                        return;
                    };

                    let mut changed = false;
                    let mut remove_param_index: Option<usize> = None;
                    let mut remove_header_index: Option<usize> = None;
                    let mut request_editor_tab = self.request_editor_tab;

                    {
                        let endpoint = &mut self.endpoints[index];

                        ui.horizontal(|ui| {
                            ui.label("Name");
                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.name).desired_width(f32::INFINITY),
                            );
                            Self::attach_text_context_menu(&response, &endpoint.name, true);
                            if response.changed() {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Collection");
                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.collection)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Inspace API V3"),
                            );
                            Self::attach_text_context_menu(&response, &endpoint.collection, true);
                            if response.changed() {
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Folder");
                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.folder_path)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Micro / Query / Native"),
                            );
                            Self::attach_text_context_menu(&response, &endpoint.folder_path, true);
                            if response.changed() {
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

                            let response = ui.add(
                                TextEdit::singleline(&mut endpoint.url)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("https://${api_host}/resource"),
                            );
                            Self::attach_text_context_menu(&response, &endpoint.url, true);
                            if response.changed() {
                                normalize_endpoint_url_and_query_params(endpoint);
                                changed = true;
                            }
                        });

                        ui.separator();
                        let non_empty_param_count = endpoint
                            .query_params
                            .iter()
                            .filter(|param| !param.key.trim().is_empty())
                            .count();
                        let non_empty_header_count = endpoint
                            .headers
                            .iter()
                            .filter(|header| !header.key.trim().is_empty())
                            .count();
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Params,
                                format!("Params ({non_empty_param_count})"),
                            );
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Headers,
                                format!("Headers ({non_empty_header_count})"),
                            );
                            ui.selectable_value(
                                &mut request_editor_tab,
                                RequestEditorTab::Body,
                                "Body",
                            );
                        });
                        ui.separator();

                        match request_editor_tab {
                            RequestEditorTab::Params => {
                                ui.small("Query params are appended to the URL when sending.");
                                for (param_index, param) in endpoint.query_params.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        let row_width = ui.available_width();
                                        let spacing = ui.spacing().item_spacing.x;
                                        let remove_width = 24.0_f32;
                                        let key_width = (row_width * 0.35).clamp(120.0, 260.0);
                                        let value_width =
                                            (row_width - key_width - remove_width - (spacing * 2.0))
                                                .max(120.0);

                                        let response = ui.add_sized(
                                            [key_width, 0.0],
                                            TextEdit::singleline(&mut param.key).hint_text("Param key"),
                                        );
                                        Self::attach_text_context_menu(&response, &param.key, true);
                                        if response.changed() {
                                            changed = true;
                                        }
                                        let response = ui.add_sized(
                                            [value_width, 0.0],
                                            TextEdit::singleline(&mut param.value).hint_text("Param value"),
                                        );
                                        Self::attach_text_context_menu(&response, &param.value, true);
                                        if response.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add_sized([remove_width, 0.0], egui::Button::new("x"))
                                            .clicked()
                                        {
                                            remove_param_index = Some(param_index);
                                        }
                                    });
                                }

                                if let Some(param_index) = remove_param_index {
                                    endpoint.query_params.remove(param_index);
                                    changed = true;
                                }
                                if ui.button("+ Add Param").clicked() {
                                    endpoint.query_params.push(KeyValue::default());
                                    changed = true;
                                }
                            }
                            RequestEditorTab::Headers => {
                                ui.small(
                                    "Name + value (example: Authorization = Bearer ${token})",
                                );
                                for (header_index, header) in endpoint.headers.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        let row_width = ui.available_width();
                                        let spacing = ui.spacing().item_spacing.x;
                                        let remove_width = 24.0_f32;
                                        let key_width = (row_width * 0.35).clamp(120.0, 260.0);
                                        let value_width =
                                            (row_width - key_width - remove_width - (spacing * 2.0))
                                                .max(120.0);

                                        let response = ui.add_sized(
                                            [key_width, 0.0],
                                            TextEdit::singleline(&mut header.key).hint_text("Header name"),
                                        );
                                        Self::attach_text_context_menu(&response, &header.key, true);
                                        if response.changed() {
                                            changed = true;
                                        }
                                        let response = ui.add_sized(
                                            [value_width, 0.0],
                                            TextEdit::singleline(&mut header.value).hint_text("Header value"),
                                        );
                                        Self::attach_text_context_menu(&response, &header.value, true);
                                        if response.changed() {
                                            changed = true;
                                        }
                                        if ui
                                            .add_sized([remove_width, 0.0], egui::Button::new("x"))
                                            .clicked()
                                        {
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
                            }
                            RequestEditorTab::Body => {
                                ui.horizontal(|ui| {
                                    ui.label("Mode");
                                    egui::ComboBox::from_id_salt("body-mode-picker")
                                        .selected_text(normalize_body_mode(&endpoint.body_mode))
                                        .show_ui(ui, |ui| {
                                            for mode in BODY_MODE_OPTIONS {
                                                if ui
                                                    .selectable_label(
                                                        normalize_body_mode(&endpoint.body_mode)
                                                            == mode,
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
                                let response = ui.add(
                                    TextEdit::multiline(&mut endpoint.body)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(12)
                                        .hint_text(
                                            "{\n  \"token\": \"${token}\"\n}\nOR\nkey=value\nkey2=value2",
                                        ),
                                );
                                Self::attach_text_context_menu(&response, &endpoint.body, true);
                                if response.changed() {
                                    changed = true;
                                }
                            }
                        }
                    }
                    self.request_editor_tab = request_editor_tab;

                    if changed {
                        self.mark_dirty();
                    }

                    ui.separator();
                    let env_vars = self.selected_environment_variables();
                    let resolved_url = resolve_endpoint_url(&self.endpoints[index], &env_vars);
                    ui.label(format!("Resolved URL (preview): {resolved_url}"));
                    ui.label(
                        "Use ${variable_name} placeholders in URL, params, headers, and body.",
                    );
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
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.response_view_tab, ResponseViewTab::Raw, "Raw");
                    ui.selectable_value(
                        &mut self.response_view_tab,
                        ResponseViewTab::Pretty,
                        "Pretty",
                    );
                });
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
                        match self.response_view_tab {
                            ResponseViewTab::Raw => {
                                let body_height = ui.available_height().max(120.0);
                                let response = ui.add_sized(
                                    [ui.available_width(), body_height],
                                    TextEdit::multiline(&mut self.response_body_view)
                                        .desired_width(f32::INFINITY)
                                        .code_editor(),
                                );
                                Self::attach_text_context_menu(
                                    &response,
                                    &self.response_body_view,
                                    true,
                                );
                            }
                            ResponseViewTab::Pretty => {
                                if let Some(value) = &self.parsed_response_json {
                                    Self::render_json_tree(ui, None, value, "$");
                                } else if let Some(err) = &self.parsed_response_json_error {
                                    ui.colored_label(
                                        Color32::from_rgb(240, 120, 120),
                                        format!("Response body is not valid JSON: {err}"),
                                    );
                                } else if self.response.body.trim().is_empty() {
                                    ui.label("No response body.");
                                } else {
                                    ui.label(
                                        "Response body is not available in pretty view for this payload.",
                                    );
                                }
                            }
                        }
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

impl eframe::App for MailmanApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_window_resolution(ctx);
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
        self.render_postman_import_dialog(ctx);
        self.render_share_bundle_dialogs(ctx);
        self.render_status_line(ctx);

        self.try_auto_save();
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(err) = self.storage.save_config(&self.config) {
            eprintln!("Failed to persist app config on exit: {err}");
        }
    }
}
