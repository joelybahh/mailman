use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub(crate) static PLACEHOLDER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("placeholder regex should always compile")
});

pub(crate) static POSTMAN_PLACEHOLDER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}")
        .expect("postman placeholder regex should always compile")
});

pub(crate) static WORKSPACE_ROUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"workspace/[A-Za-z0-9._-]+~([0-9a-fA-F-]{36})")
        .expect("workspace route regex should compile")
});

pub(crate) static LAST_ACTIVE_WORKSPACE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"lastActiveWorkspaceData.*?"id":"([0-9a-fA-F-]{36})","name":"([^"]+)""#)
        .expect("last active workspace regex should compile")
});

pub(crate) static WORKSPACE_COLLECTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"collection/([0-9A-Za-z-]{8,80})/([0-9a-fA-F-]{36})")
        .expect("workspace collection regex should compile")
});

pub(crate) static REQUESTER_CONFLICT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"BaseEditorModel~conflictState:\s*(?:latestResource|currentResource):","((?:\\.|[^"])*)""#,
    )
    .expect("requester conflict regex should compile")
});

pub(crate) const GZIP_MAGIC: &[u8] = b"\x1f\x8b\x08";

pub(crate) const METHOD_OPTIONS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
];

pub(crate) const BODY_MODE_OPTIONS: [&str; 5] =
    ["none", "raw", "urlencoded", "form-data", "binary"];

// has to stay incorrectly named 'delivery-man...' because reasons.
pub(crate) const VERIFIER_PLAINTEXT: &[u8] = b"delivery-man-unlock-verifier-v1";

pub(crate) type KeyMaterial = [u8; 32];

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub(crate) struct KeyValue {
    pub(crate) key: String,
    pub(crate) value: String,
}

/// A single post-response script rule: extract a value from the JSON response
/// body and write it into an environment variable.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub(crate) struct ResponseScript {
    /// Dot-notation path into the JSON body, e.g. `access_token` or `data.token`.
    #[serde(default)]
    pub(crate) extract_key: String,
    /// Name of the environment variable to override, e.g. `token`.
    #[serde(default)]
    pub(crate) env_var: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Endpoint {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) source_request_id: String,
    #[serde(default)]
    pub(crate) source_collection_id: String,
    #[serde(default)]
    pub(crate) source_folder_id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) collection: String,
    #[serde(default)]
    pub(crate) folder_path: String,
    pub(crate) method: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) query_params: Vec<KeyValue>,
    pub(crate) headers: Vec<KeyValue>,
    #[serde(default = "default_endpoint_body_mode")]
    pub(crate) body_mode: String,
    pub(crate) body: String,
    /// Rules executed after a 2xx response to extract values from the JSON
    /// body and write them into environment variables.
    #[serde(default)]
    pub(crate) scripts: Vec<ResponseScript>,
}

impl Endpoint {
    pub(crate) fn with_defaults(id: String, name: &str, method: &str, url: &str) -> Self {
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
            query_params: vec![],
            headers: vec![],
            body_mode: "none".to_owned(),
            body: String::new(),
            scripts: vec![],
        }
    }
}

fn default_endpoint_body_mode() -> String {
    "raw".to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EnvironmentIndexEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) file_name: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub(crate) struct EnvironmentFile {
    pub(crate) variables: Vec<KeyValue>,
}

#[derive(Clone, Debug)]
pub(crate) struct Environment {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) file_name: String,
    pub(crate) variables: Vec<KeyValue>,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    pub(crate) selected_endpoint_id: Option<String>,
    pub(crate) selected_environment_id: Option<String>,
    pub(crate) postman_preseed_done: bool,
    #[serde(default)]
    pub(crate) window_width: Option<u32>,
    #[serde(default)]
    pub(crate) window_height: Option<u32>,
    /// How long a saved session should last after a successful unlock.
    /// `None`    = always ask for password (default).
    /// `Some(0)` = keep forever (no expiry).
    /// `Some(n)` = keep for n days, extended on each launch.
    #[serde(default)]
    pub(crate) session_duration_days: Option<u32>,
    /// Unix timestamp (seconds) when the current cached session expires.
    /// `None`         = no active session.
    /// `Some(u64::MAX)` = never expires.
    /// `Some(ts)`     = expires at `ts`.
    #[serde(default)]
    pub(crate) session_expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EncryptedBlob {
    pub(crate) version: u8,
    pub(crate) nonce_b64: String,
    pub(crate) ciphertext_b64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SecurityMetadata {
    pub(crate) version: u8,
    pub(crate) salt_b64: String,
    pub(crate) verifier: EncryptedBlob,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SharedWorkspaceBundleFile {
    pub(crate) version: u8,
    pub(crate) salt_b64: String,
    pub(crate) encrypted: EncryptedBlob,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SharedWorkspacePayload {
    pub(crate) version: u8,
    pub(crate) endpoints: Vec<Endpoint>,
    pub(crate) environments: Vec<SharedEnvironment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SharedEnvironment {
    pub(crate) name: String,
    pub(crate) variables: Vec<KeyValue>,
}

#[derive(Default, Debug)]
pub(crate) struct ResponseState {
    pub(crate) status_code: Option<u16>,
    pub(crate) status_text: String,
    pub(crate) duration_ms: Option<u128>,
    pub(crate) headers: Vec<KeyValue>,
    pub(crate) body: String,
    pub(crate) error: Option<String>,
}

impl ResponseState {
    pub(crate) fn clear_for_request(&mut self) {
        self.status_code = None;
        self.status_text = "Sending request...".to_owned();
        self.duration_ms = None;
        self.headers.clear();
        self.body.clear();
        self.error = None;
    }
}
