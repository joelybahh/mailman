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
    AppConfig, Endpoint, Environment, EnvironmentIndexEntry, KeyMaterial, KeyValue,
    PersistedRequestTab, RequestEditorTab, ResponseState, ResponseViewTab, SecurityMetadata,
    SharedEnvironment, SharedWorkspacePayload, WorkspaceUiState,
};
use crate::request_body::normalize_body_mode_owned;
use crate::storage::{AppStorage, CoreData};

pub(in crate::app) enum AuthResult {
    SetupOk {
        metadata: SecurityMetadata,
        key: KeyMaterial,
    },
    UnlockOk(KeyMaterial),
    Err(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum AppPhase {
    SetupPassword,
    UnlockPassword,
    Ready,
}

#[derive(Clone, Debug)]
pub(in crate::app) struct RequestTab {
    pub(in crate::app) id: String,
    pub(in crate::app) saved_endpoint_id: Option<String>,
    pub(in crate::app) draft: Endpoint,
    pub(in crate::app) is_dirty: bool,
    pub(in crate::app) editor_tab: RequestEditorTab,
    pub(in crate::app) response_view_tab: ResponseViewTab,
    pub(in crate::app) response: ResponseState,
    pub(in crate::app) response_raw_chunks: Vec<String>,
    pub(in crate::app) parsed_response_json: Option<serde_json::Value>,
    pub(in crate::app) parsed_response_json_error: Option<String>,
    pub(in crate::app) scripts_ran: usize,
    pub(in crate::app) inflight_environment_id: Option<String>,
}

impl RequestTab {
    fn from_saved(endpoint: Endpoint) -> Self {
        Self::from_persisted(PersistedRequestTab {
            id: create_id("tab"),
            saved_endpoint_id: Some(endpoint.id.clone()),
            draft: endpoint,
            is_dirty: false,
            editor_tab: RequestEditorTab::Params,
            response_view_tab: ResponseViewTab::Pretty,
            response: ResponseState::default(),
            scripts_ran: 0,
        })
    }

    fn from_persisted(tab: PersistedRequestTab) -> Self {
        let mut runtime = Self {
            id: tab.id,
            saved_endpoint_id: tab.saved_endpoint_id,
            draft: tab.draft,
            is_dirty: tab.is_dirty,
            editor_tab: tab.editor_tab,
            response_view_tab: tab.response_view_tab,
            response: tab.response,
            response_raw_chunks: Vec::new(),
            parsed_response_json: None,
            parsed_response_json_error: None,
            scripts_ran: tab.scripts_ran,
            inflight_environment_id: None,
        };
        runtime.rebuild_response_caches();
        runtime
    }

    fn to_persisted(&self) -> PersistedRequestTab {
        PersistedRequestTab {
            id: self.id.clone(),
            saved_endpoint_id: self.saved_endpoint_id.clone(),
            draft: self.draft.clone(),
            is_dirty: self.is_dirty,
            editor_tab: self.editor_tab,
            response_view_tab: self.response_view_tab,
            response: self.response.clone(),
            scripts_ran: self.scripts_ran,
        }
    }

    fn rebuild_response_caches(&mut self) {
        self.response_raw_chunks = chunk_response_body(&self.response.body);
        if self.response.body.trim().is_empty() {
            self.parsed_response_json = None;
            self.parsed_response_json_error = None;
            return;
        }

        match serde_json::from_str::<serde_json::Value>(self.response.body.trim()) {
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

    fn clear_for_request_switch(&mut self) {
        self.response = ResponseState::default();
        self.response_raw_chunks.clear();
        self.parsed_response_json = None;
        self.parsed_response_json_error = None;
        self.scripts_ran = 0;
        self.inflight_environment_id = None;
    }
}

#[derive(Clone, Debug)]
pub(in crate::app) enum PendingRequestAction {
    CloseTab { tab_id: String },
    CloseAll,
    DeleteActive,
    DeleteAll,
}

pub(crate) struct MailmanApp {
    pub(in crate::app) storage: AppStorage,
    pub(in crate::app) phase: AppPhase,
    pub(in crate::app) security_metadata: Option<SecurityMetadata>,
    pub(in crate::app) key_material: Option<KeyMaterial>,

    pub(in crate::app) saved_endpoints: Vec<Endpoint>,
    pub(in crate::app) open_request_tabs: Vec<RequestTab>,
    pub(in crate::app) pending_environment_index: Vec<EnvironmentIndexEntry>,
    pub(in crate::app) environments: Vec<Environment>,

    pub(in crate::app) active_request_tab_id: Option<String>,
    pub(in crate::app) selected_environment_id: Option<String>,
    pub(in crate::app) config: AppConfig,

    pub(in crate::app) setup_password: String,
    pub(in crate::app) setup_password_confirm: String,
    pub(in crate::app) unlock_password: String,
    pub(in crate::app) auth_message: String,

    pub(in crate::app) status_line: String,
    pub(in crate::app) dirty: bool,
    pub(in crate::app) last_mutation: Instant,
    pub(in crate::app) workspace_ui_dirty: bool,
    pub(in crate::app) workspace_ui_last_mutation: Instant,
    pub(in crate::app) in_flight_tab_id: Option<String>,
    pub(in crate::app) response_rx: Option<Receiver<(String, ResponseState)>>,

    pub(in crate::app) auth_pending: bool,
    pub(in crate::app) auth_rx: Option<Receiver<AuthResult>>,

    pub(in crate::app) new_environment_name: String,
    pub(in crate::app) postman_import_path: String,
    pub(in crate::app) postman_workspace_filter: String,
    pub(in crate::app) show_environment_panel: bool,
    pub(in crate::app) show_export_bundle_dialog: bool,
    pub(in crate::app) export_bundle_password: String,
    pub(in crate::app) export_bundle_password_confirm: String,
    pub(in crate::app) show_postman_import_dialog: bool,
    pub(in crate::app) show_import_bundle_dialog: bool,
    pub(in crate::app) import_bundle_path: Option<PathBuf>,
    pub(in crate::app) import_bundle_password: String,
    pub(in crate::app) logo_texture: Option<egui::TextureHandle>,
    /// Set to `true` on startup; the sidebar uses it to auto-expand the
    /// collection/folder containing the selected endpoint on first render,
    /// then resets it to `false`.
    pub(in crate::app) expand_to_selection: bool,
    pub(in crate::app) dragging_tab_id: Option<String>,
    pub(in crate::app) rename_tab_id: Option<String>,
    pub(in crate::app) rename_buffer: String,
    pub(in crate::app) pending_request_action: Option<PendingRequestAction>,
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

        let saved_endpoints = core_data.endpoints;
        let workspace_ui = match storage.load_workspace_ui() {
            Ok(workspace_ui) => workspace_ui,
            Err(err) => {
                if status_line.is_empty() {
                    status_line = format!("Failed to read tab workspace: {err}");
                }
                None
            }
        };
        let (open_request_tabs, active_request_tab_id) = Self::restore_request_tabs(
            &saved_endpoints,
            workspace_ui,
            core_data.config.selected_endpoint_id.clone(),
        );

        let mut app = Self {
            storage,
            phase,
            security_metadata,
            key_material: None,
            saved_endpoints,
            open_request_tabs,
            pending_environment_index: core_data.environment_index,
            environments: vec![],
            active_request_tab_id,
            selected_environment_id: core_data.config.selected_environment_id.clone(),
            config: core_data.config,
            setup_password: String::new(),
            setup_password_confirm: String::new(),
            unlock_password: String::new(),
            auth_message: String::new(),
            status_line,
            dirty: false,
            last_mutation: Instant::now(),
            workspace_ui_dirty: false,
            workspace_ui_last_mutation: Instant::now(),
            in_flight_tab_id: None,
            response_rx: None,
            auth_pending: false,
            auth_rx: None,
            new_environment_name: String::new(),
            postman_import_path: String::new(),
            postman_workspace_filter: String::new(),
            show_environment_panel: false,
            show_export_bundle_dialog: false,
            export_bundle_password: String::new(),
            export_bundle_password_confirm: String::new(),
            show_postman_import_dialog: false,
            show_import_bundle_dialog: false,
            import_bundle_path: None,
            import_bundle_password: String::new(),
            logo_texture: None,
            expand_to_selection: true,
            dragging_tab_id: None,
            rename_tab_id: None,
            rename_buffer: String::new(),
            pending_request_action: None,
        };

        app.try_auto_session_unlock();
        app
    }

    fn restore_request_tabs(
        saved_endpoints: &[Endpoint],
        workspace_ui: Option<WorkspaceUiState>,
        legacy_selected_endpoint_id: Option<String>,
    ) -> (Vec<RequestTab>, Option<String>) {
        let saved_by_id = saved_endpoints
            .iter()
            .map(|endpoint| (endpoint.id.clone(), endpoint))
            .collect::<BTreeMap<_, _>>();

        if let Some(workspace_ui) = workspace_ui {
            let mut tabs = Vec::new();
            for persisted in workspace_ui.open_tabs {
                if !persisted.is_dirty
                    && let Some(saved_id) = persisted.saved_endpoint_id.as_ref()
                    && !saved_by_id.contains_key(saved_id)
                {
                    continue;
                }

                let mut persisted = persisted;
                if !persisted.is_dirty
                    && let Some(saved_id) = persisted.saved_endpoint_id.as_ref()
                    && let Some(saved_endpoint) = saved_by_id.get(saved_id)
                {
                    persisted.draft = (*saved_endpoint).clone();
                }
                tabs.push(RequestTab::from_persisted(persisted));
            }

            if tabs.is_empty() {
                return Self::fallback_request_tabs(saved_endpoints, legacy_selected_endpoint_id);
            }

            let active_tab_id = workspace_ui
                .active_tab_id
                .filter(|id| tabs.iter().any(|tab| &tab.id == id))
                .or_else(|| tabs.first().map(|tab| tab.id.clone()));
            return (tabs, active_tab_id);
        }

        Self::fallback_request_tabs(saved_endpoints, legacy_selected_endpoint_id)
    }

    fn fallback_request_tabs(
        saved_endpoints: &[Endpoint],
        legacy_selected_endpoint_id: Option<String>,
    ) -> (Vec<RequestTab>, Option<String>) {
        let fallback_endpoint = legacy_selected_endpoint_id
            .as_ref()
            .and_then(|id| saved_endpoints.iter().find(|endpoint| &endpoint.id == id))
            .cloned()
            .or_else(|| saved_endpoints.first().cloned());

        let tabs = fallback_endpoint
            .map(RequestTab::from_saved)
            .into_iter()
            .collect::<Vec<_>>();
        let active_tab_id = tabs.first().map(|tab| tab.id.clone());
        (tabs, active_tab_id)
    }

    pub(in crate::app) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_mutation = Instant::now();
    }

    pub(in crate::app) fn mark_workspace_ui_dirty(&mut self) {
        self.workspace_ui_dirty = true;
        self.workspace_ui_last_mutation = Instant::now();
    }

    pub(in crate::app) fn lock_workspace(&mut self) {
        // Wipe the key and decrypted environments from memory.
        self.key_material = None;
        self.environments.clear();

        // Drop the in-flight request channel so the worker response is never
        // consumed after locking. This prevents a pre-lock response (and any
        // scripts it would trigger) from mutating state in the next session.
        self.response_rx = None;
        self.in_flight_tab_id = None;
        for tab in &mut self.open_request_tabs {
            tab.clear_for_request_switch();
        }

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
            .active_request_tab_id
            .as_ref()
            .and_then(|id| self.open_request_tabs.iter().find(|tab| &tab.id == id))
            .is_none()
        {
            self.active_request_tab_id = self.open_request_tabs.first().map(|tab| tab.id.clone());
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

        self.config.selected_environment_id = self.selected_environment_id.clone();

        match self.storage.save_config(&self.config) {
            Ok(_) => {
                if let Some(key) = self.key_material.as_ref() {
                    if let Err(err) = self.storage.save_environments(&self.environments, key) {
                        self.status_line = format!("Save failed: {err}");
                        return;
                    }
                }
                self.dirty = false;
            }
            Err(err) => {
                self.status_line = format!("Save failed: {err}");
            }
        }
    }

    pub(in crate::app) fn try_auto_save_workspace_ui(&mut self) {
        if self.phase != AppPhase::Ready || !self.workspace_ui_dirty {
            return;
        }
        if self.workspace_ui_last_mutation.elapsed() < Duration::from_millis(350) {
            return;
        }

        if let Err(err) = self
            .storage
            .save_workspace_ui(&self.current_workspace_ui_state())
        {
            self.status_line = format!("Failed to persist tabs: {err}");
            return;
        }

        self.workspace_ui_dirty = false;
    }

    pub(in crate::app) fn current_workspace_ui_state(&self) -> WorkspaceUiState {
        WorkspaceUiState {
            active_tab_id: self.active_request_tab_id.clone(),
            open_tabs: self
                .open_request_tabs
                .iter()
                .map(RequestTab::to_persisted)
                .collect(),
        }
    }

    pub(in crate::app) fn active_request_tab_index(&self) -> Option<usize> {
        let active = self.active_request_tab_id.as_ref()?;
        self.open_request_tabs
            .iter()
            .position(|tab| &tab.id == active)
    }

    pub(in crate::app) fn active_request_tab(&self) -> Option<&RequestTab> {
        self.active_request_tab_index()
            .and_then(|index| self.open_request_tabs.get(index))
    }

    pub(in crate::app) fn active_request_tab_mut(&mut self) -> Option<&mut RequestTab> {
        let index = self.active_request_tab_index()?;
        self.open_request_tabs.get_mut(index)
    }

    pub(in crate::app) fn active_endpoint_id(&self) -> Option<&str> {
        self.active_request_tab().map(|tab| tab.draft.id.as_str())
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

    pub(in crate::app) fn is_request_in_flight(&self, tab_id: &str) -> bool {
        self.in_flight_tab_id.as_deref() == Some(tab_id)
    }

    pub(in crate::app) fn activate_request_tab(&mut self, tab_id: Option<String>) {
        if self.active_request_tab_id == tab_id {
            return;
        }

        self.active_request_tab_id = tab_id;
        self.expand_to_selection = true;
        self.mark_workspace_ui_dirty();
    }

    pub(in crate::app) fn open_saved_request_in_tab(&mut self, endpoint_id: &str) {
        if let Some(tab) = self
            .open_request_tabs
            .iter()
            .find(|tab| tab.saved_endpoint_id.as_deref() == Some(endpoint_id))
        {
            self.activate_request_tab(Some(tab.id.clone()));
            return;
        }

        let Some(endpoint) = self
            .saved_endpoints
            .iter()
            .find(|endpoint| endpoint.id == endpoint_id)
            .cloned()
        else {
            return;
        };

        let tab = RequestTab::from_saved(endpoint);
        let tab_id = tab.id.clone();
        self.open_request_tabs.push(tab);
        self.activate_request_tab(Some(tab_id));
        self.mark_workspace_ui_dirty();
    }

    pub(in crate::app) fn mark_active_request_dirty(&mut self) {
        if let Some(tab) = self.active_request_tab_mut() {
            tab.is_dirty = true;
            normalize_endpoint_url_and_query_params(&mut tab.draft);
        }
        self.mark_workspace_ui_dirty();
    }

    pub(in crate::app) fn sync_clean_tabs_from_saved_endpoints(&mut self) {
        let saved_by_id = self
            .saved_endpoints
            .iter()
            .map(|endpoint| (endpoint.id.clone(), endpoint.clone()))
            .collect::<BTreeMap<_, _>>();
        self.open_request_tabs.retain(|tab| {
            tab.is_dirty
                || tab
                    .saved_endpoint_id
                    .as_ref()
                    .map(|saved_id| saved_by_id.contains_key(saved_id))
                    .unwrap_or(true)
        });
        for tab in &mut self.open_request_tabs {
            if tab.is_dirty {
                continue;
            }
            if let Some(saved_id) = tab.saved_endpoint_id.as_ref()
                && let Some(saved_endpoint) = saved_by_id.get(saved_id)
            {
                tab.draft = saved_endpoint.clone();
            }
        }
        self.ensure_selected_ids();
    }

    pub(in crate::app) fn save_request_tabs(
        &mut self,
        tab_ids: &[String],
    ) -> Result<usize, String> {
        if tab_ids.is_empty() {
            return Ok(0);
        }

        let mut changed = 0usize;
        let mut saved_by_id = self
            .saved_endpoints
            .iter()
            .enumerate()
            .map(|(index, endpoint)| (endpoint.id.clone(), index))
            .collect::<BTreeMap<_, _>>();

        for tab_id in tab_ids {
            let Some(tab_index) = self
                .open_request_tabs
                .iter()
                .position(|tab| &tab.id == tab_id)
            else {
                continue;
            };

            let tab = &mut self.open_request_tabs[tab_index];
            normalize_endpoint_url_and_query_params(&mut tab.draft);

            let endpoint = tab.draft.clone();
            if let Some(saved_index) = saved_by_id.get(&endpoint.id).copied() {
                self.saved_endpoints[saved_index] = endpoint.clone();
            } else {
                saved_by_id.insert(endpoint.id.clone(), self.saved_endpoints.len());
                self.saved_endpoints.push(endpoint.clone());
            }

            tab.saved_endpoint_id = Some(endpoint.id.clone());
            tab.is_dirty = false;
            changed += 1;
        }

        self.saved_endpoints.sort_by(|left, right| {
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
        self.sync_clean_tabs_from_saved_endpoints();
        self.mark_workspace_ui_dirty();
        self.storage
            .save_requests(&self.saved_endpoints)
            .map_err(|err| format!("Failed to save requests: {err}"))?;
        self.storage
            .save_workspace_ui(&self.current_workspace_ui_state())
            .map_err(|err| format!("Failed to save tab workspace: {err}"))?;
        self.workspace_ui_dirty = false;
        Ok(changed)
    }

    pub(in crate::app) fn save_all_dirty_request_tabs(&mut self) -> Result<usize, String> {
        let dirty_tab_ids = self
            .open_request_tabs
            .iter()
            .filter(|tab| tab.is_dirty)
            .map(|tab| tab.id.clone())
            .collect::<Vec<_>>();
        self.save_request_tabs(&dirty_tab_ids)
    }

    pub(in crate::app) fn move_request_tab(&mut self, tab_id: &str, target_index: usize) {
        let Some(current_index) = self
            .open_request_tabs
            .iter()
            .position(|tab| tab.id == tab_id)
        else {
            return;
        };
        let bounded_target = target_index.min(self.open_request_tabs.len().saturating_sub(1));
        if current_index == bounded_target {
            return;
        }

        let tab = self.open_request_tabs.remove(current_index);
        self.open_request_tabs.insert(bounded_target, tab);
        self.mark_workspace_ui_dirty();
    }

    pub(in crate::app) fn close_all_saved_tabs(&mut self) {
        let before = self.open_request_tabs.len();
        self.open_request_tabs.retain(|tab| tab.is_dirty);
        let closed = before.saturating_sub(self.open_request_tabs.len());
        self.ensure_selected_ids();
        self.mark_workspace_ui_dirty();
        self.status_line = format!("Closed {closed} saved tabs.");
    }

    fn pending_request_action_target_tab_ids(&self) -> Vec<String> {
        match self.pending_request_action.as_ref() {
            Some(PendingRequestAction::CloseTab { tab_id }) => vec![tab_id.clone()],
            Some(PendingRequestAction::CloseAll) | Some(PendingRequestAction::DeleteAll) => self
                .open_request_tabs
                .iter()
                .map(|tab| tab.id.clone())
                .collect(),
            Some(PendingRequestAction::DeleteActive) => {
                self.active_request_tab_id.clone().into_iter().collect()
            }
            None => Vec::new(),
        }
    }

    pub(in crate::app) fn pending_request_action_dirty_tab_ids(&self) -> Vec<String> {
        let target_ids = self.pending_request_action_target_tab_ids();
        self.open_request_tabs
            .iter()
            .filter(|tab| target_ids.iter().any(|id| id == &tab.id) && tab.is_dirty)
            .map(|tab| tab.id.clone())
            .collect()
    }

    fn persist_request_workspace_snapshot(&mut self) -> Result<(), String> {
        self.storage
            .save_requests(&self.saved_endpoints)
            .map_err(|err| format!("Failed to save requests: {err}"))?;
        self.storage
            .save_workspace_ui(&self.current_workspace_ui_state())
            .map_err(|err| format!("Failed to save tab workspace: {err}"))?;
        self.workspace_ui_dirty = false;
        Ok(())
    }

    fn close_tabs_by_id_set(&mut self, tab_ids: &BTreeSet<String>) {
        self.open_request_tabs
            .retain(|tab| !tab_ids.contains(&tab.id));
        if let Some(active_tab_id) = self.active_request_tab_id.as_ref()
            && tab_ids.contains(active_tab_id)
        {
            self.active_request_tab_id = self.open_request_tabs.first().map(|tab| tab.id.clone());
        }
    }

    pub(in crate::app) fn resolve_pending_request_action(
        &mut self,
        save_dirty: bool,
    ) -> Result<(), String> {
        let Some(action) = self.pending_request_action.clone() else {
            return Ok(());
        };

        let target_tab_ids = self.pending_request_action_target_tab_ids();
        let dirty_tab_ids = self.pending_request_action_dirty_tab_ids();
        if save_dirty && !dirty_tab_ids.is_empty() {
            self.save_request_tabs(&dirty_tab_ids)?;
        }

        let target_set = target_tab_ids.into_iter().collect::<BTreeSet<_>>();

        match action {
            PendingRequestAction::CloseTab { .. } | PendingRequestAction::CloseAll => {
                self.close_tabs_by_id_set(&target_set);
            }
            PendingRequestAction::DeleteActive => {
                let endpoint_ids = self
                    .open_request_tabs
                    .iter()
                    .filter(|tab| target_set.contains(&tab.id))
                    .filter_map(|tab| tab.saved_endpoint_id.clone())
                    .collect::<BTreeSet<_>>();
                self.saved_endpoints
                    .retain(|endpoint| !endpoint_ids.contains(&endpoint.id));
                self.close_tabs_by_id_set(&target_set);
            }
            PendingRequestAction::DeleteAll => {
                self.saved_endpoints.clear();
                self.open_request_tabs.clear();
                self.active_request_tab_id = None;
            }
        }

        self.sync_clean_tabs_from_saved_endpoints();
        self.ensure_selected_ids();
        self.mark_workspace_ui_dirty();
        self.persist_request_workspace_snapshot()?;
        self.pending_request_action = None;
        Ok(())
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
        let endpoint = Endpoint {
            id: create_id("ep"),
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
        };
        let tab = RequestTab {
            id: create_id("tab"),
            saved_endpoint_id: None,
            draft: endpoint,
            is_dirty: true,
            editor_tab: RequestEditorTab::Params,
            response_view_tab: ResponseViewTab::Pretty,
            response: ResponseState::default(),
            response_raw_chunks: Vec::new(),
            parsed_response_json: None,
            parsed_response_json_error: None,
            scripts_ran: 0,
            inflight_environment_id: None,
        };
        let tab_id = tab.id.clone();
        self.open_request_tabs.push(tab);
        self.activate_request_tab(Some(tab_id));
        self.mark_workspace_ui_dirty();
    }

    pub(in crate::app) fn delete_selected_endpoint(&mut self) {
        if self.active_request_tab().is_none() {
            return;
        }
        self.pending_request_action = Some(PendingRequestAction::DeleteActive);
    }

    pub(in crate::app) fn delete_all_requests(&mut self) {
        self.pending_request_action = Some(PendingRequestAction::DeleteAll);
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
        if self.in_flight_tab_id.is_some() {
            return;
        }

        let Some(tab_index) = self.active_request_tab_index() else {
            self.status_line = "Select an endpoint first.".to_owned();
            return;
        };

        let tab_id = self.open_request_tabs[tab_index].id.clone();
        let endpoint = self.open_request_tabs[tab_index].draft.clone();
        let env_vars = self.selected_environment_variables();
        let (tx, rx) = mpsc::channel();

        self.in_flight_tab_id = Some(tab_id.clone());
        self.open_request_tabs[tab_index].scripts_ran = 0;
        self.open_request_tabs[tab_index].inflight_environment_id =
            self.selected_environment_id.clone();
        self.open_request_tabs[tab_index]
            .response
            .clear_for_request();
        self.open_request_tabs[tab_index]
            .response_raw_chunks
            .clear();
        self.open_request_tabs[tab_index].parsed_response_json = None;
        self.open_request_tabs[tab_index].parsed_response_json_error = None;
        self.response_rx = Some(rx);
        self.status_line = format!("Sending {} {}", endpoint.method, endpoint.url);
        self.mark_workspace_ui_dirty();

        thread::spawn(move || {
            let state = execute_request(endpoint, env_vars);
            let _ = tx.send((tab_id, state));
        });
    }

    pub(in crate::app) fn copy_curl_for_selected_request(&mut self, ctx: &egui::Context) {
        let Some(tab_index) = self.active_request_tab_index() else {
            self.status_line = "Select an endpoint first.".to_owned();
            return;
        };

        let endpoint = self.open_request_tabs[tab_index].draft.clone();
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
            Ok((tab_id, response_state)) => {
                if let Some(tab_index) = self
                    .open_request_tabs
                    .iter()
                    .position(|tab| tab.id == tab_id)
                {
                    self.open_request_tabs[tab_index].response = response_state;
                    self.open_request_tabs[tab_index].rebuild_response_caches();
                    self.run_response_scripts(&tab_id);
                }
                self.in_flight_tab_id = None;
                self.response_rx = None;
                self.mark_workspace_ui_dirty();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let in_flight_tab_id = self.in_flight_tab_id.clone();
                self.in_flight_tab_id = None;
                self.response_rx = None;
                if let Some(tab_id) = in_flight_tab_id
                    && let Some(tab) = self
                        .open_request_tabs
                        .iter_mut()
                        .find(|tab| tab.id == tab_id)
                {
                    tab.response.error = Some("Request worker disconnected.".to_owned());
                    tab.response_raw_chunks.clear();
                    tab.parsed_response_json = None;
                    tab.parsed_response_json_error = None;
                }
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

    pub(in crate::app) fn run_response_scripts(&mut self, tab_id: &str) {
        let Some(tab_index) = self
            .open_request_tabs
            .iter()
            .position(|tab| tab.id == tab_id)
        else {
            return;
        };
        let tab = &self.open_request_tabs[tab_index];

        // Only fire on 2xx responses.
        let Some(status) = tab.response.status_code else {
            return;
        };
        if !(200..300).contains(&status) {
            return;
        }

        let scripts = tab.draft.scripts.clone();
        if scripts.is_empty() {
            return;
        }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(&tab.response.body) else {
            return;
        };

        // Resolve against the environment that was active at send time, not
        // the current selection — the user may have switched mid-flight.
        let Some(target_id) = tab.inflight_environment_id.clone() else {
            return;
        };
        let Some(env_idx) = self.environments.iter().position(|e| e.id == target_id) else {
            return;
        };

        let mut ran = 0usize;
        for script in &scripts {
            let path = script.extract_key.trim();
            let var = script.env_var.trim();
            if path.is_empty() || var.is_empty() {
                continue;
            }

            let Some(extracted) = json_path_extract(&json, path) else {
                continue;
            };

            let env = &mut self.environments[env_idx];
            if let Some(kv) = env.variables.iter_mut().find(|kv| kv.key == var) {
                kv.value = extracted;
            } else {
                env.variables.push(KeyValue {
                    key: var.to_owned(),
                    value: extracted,
                });
            }
            ran += 1;
        }

        if ran > 0 {
            if let Some(tab) = self
                .open_request_tabs
                .iter_mut()
                .find(|tab| tab.id == tab_id)
            {
                tab.scripts_ran = ran;
            }
            self.mark_dirty();
            self.mark_workspace_ui_dirty();
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
            .saved_endpoints
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
                if merge_endpoint_details(&mut self.saved_endpoints[existing_index], endpoint) {
                    summary.endpoints_updated += 1;
                }
                continue;
            }
            endpoint_key_to_index.insert(key, self.saved_endpoints.len());
            self.saved_endpoints.push(endpoint);
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
            self.sync_clean_tabs_from_saved_endpoints();
            self.ensure_selected_ids();
            self.mark_dirty();
            if summary.endpoints_added > 0 || summary.endpoints_updated > 0 {
                self.mark_workspace_ui_dirty();
                let _ = self.storage.save_requests(&self.saved_endpoints);
            }
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
            endpoints: self.saved_endpoints.clone(),
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
            .saved_endpoints
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
        if matches!(
            prev,
            b' ' | b'\t' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'&' | b'='
        ) {
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
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_owned(),
        other => other.to_string(),
    })
}
