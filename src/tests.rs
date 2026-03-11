use std::collections::BTreeMap;

use reqwest::Method;

use crate::domain::{
    ImportScanResult, PostmanField, WorkspaceImportContext, build_curl_command, decrypt_bytes,
    deserialize_workspace_bundle, encrypt_bytes, endpoint_from_cache_object, execute_request,
    extract_import_entities_from_leveldb_binary, normalize_endpoint_url_and_query_params,
    normalize_postman_placeholders, render_postman_formdata_fields, request_body_from_data,
    request_body_mode_from_data, request_headers_from_data, request_url_from_data,
    resolve_endpoint_url, resolve_placeholders, serialize_workspace_bundle,
};
use crate::models::{
    Endpoint, KeyValue, PersistedRequestTab, RequestEditorTab, ResponseState, ResponseViewTab,
    SharedEnvironment, SharedWorkspacePayload, WorkspaceUiState,
};
use crate::request_body::{
    computed_default_content_length, default_content_type_for_mode, normalize_body_mode,
    parse_body_fields, should_add_default_content_type,
};

#[test]
fn placeholder_substitution_works() {
    let mut vars = BTreeMap::new();
    vars.insert("api_host".to_owned(), "localhost:8080".to_owned());
    vars.insert("token".to_owned(), "abc123".to_owned());

    let output = resolve_placeholders("https://${api_host}/x?token=${token}", &vars);
    assert_eq!(output, "https://localhost:8080/x?token=abc123");
}

#[test]
fn normalize_endpoint_url_and_query_params_splits_existing_url_query() {
    let mut endpoint = Endpoint {
        id: "ep-query".to_owned(),
        source_request_id: String::new(),
        source_collection_id: String::new(),
        source_folder_id: String::new(),
        name: "Query".to_owned(),
        collection: "General".to_owned(),
        folder_path: String::new(),
        method: "GET".to_owned(),
        url: "https://api.example.com/items?limit=25&token=${token}".to_owned(),
        query_params: vec![],
        headers: vec![],
        body_mode: "none".to_owned(),
        body: String::new(),
        scripts: vec![],
    };

    normalize_endpoint_url_and_query_params(&mut endpoint);

    assert_eq!(endpoint.url, "https://api.example.com/items");
    assert_eq!(endpoint.query_params.len(), 2);
    assert_eq!(endpoint.query_params[0].key, "limit");
    assert_eq!(endpoint.query_params[0].value, "25");
    assert_eq!(endpoint.query_params[1].key, "token");
    assert_eq!(endpoint.query_params[1].value, "${token}");
}

#[test]
fn resolve_endpoint_url_appends_params_with_placeholders() {
    let endpoint = Endpoint {
        id: "ep-query-resolve".to_owned(),
        source_request_id: String::new(),
        source_collection_id: String::new(),
        source_folder_id: String::new(),
        name: "Query".to_owned(),
        collection: "General".to_owned(),
        folder_path: String::new(),
        method: "GET".to_owned(),
        url: "https://${api_host}/items".to_owned(),
        query_params: vec![
            KeyValue {
                key: "limit".to_owned(),
                value: "10".to_owned(),
            },
            KeyValue {
                key: "token".to_owned(),
                value: "${token}".to_owned(),
            },
        ],
        headers: vec![],
        body_mode: "none".to_owned(),
        body: String::new(),
        scripts: vec![],
    };

    let mut env = BTreeMap::new();
    env.insert("api_host".to_owned(), "api.example.com".to_owned());
    env.insert("token".to_owned(), "abc".to_owned());

    let resolved_url = resolve_endpoint_url(&endpoint, &env);
    assert_eq!(
        resolved_url,
        "https://api.example.com/items?limit=10&token=abc"
    );
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

    let body = request_body_from_data(payload.as_object().expect("payload should be an object"));

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
        headers
            .iter()
            .any(|header| header.key == "Authorization" && header.value == "Bearer ${api_token}")
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
        query_params: vec![],
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
        scripts: vec![],
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
        query_params: vec![],
        headers: vec![KeyValue {
            key: "Bad Header".to_owned(),
            value: "Bearer abc".to_owned(),
        }],
        body_mode: "none".to_owned(),
        body: String::new(),
        scripts: vec![],
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
        query_params: vec![],
        headers: vec![],
        body_mode: "form-data".to_owned(),
        body: "name=joel\nfile=@/tmp/payload.bin".to_owned(),
        scripts: vec![],
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
    let endpoint = endpoint_from_cache_object(object.as_object().expect("object"), &import_context)
        .expect("endpoint should be parsed");

    assert_eq!(endpoint.name, "No Collection");
}

#[test]
fn workspace_bundle_roundtrip_preserves_requests_and_environments() {
    let payload = SharedWorkspacePayload {
        version: 1,
        endpoints: vec![Endpoint {
            id: "ep-share".to_owned(),
            source_request_id: String::new(),
            source_collection_id: String::new(),
            source_folder_id: String::new(),
            name: "Health".to_owned(),
            collection: "General".to_owned(),
            folder_path: String::new(),
            method: "GET".to_owned(),
            url: "https://example.com/health".to_owned(),
            query_params: vec![],
            headers: vec![],
            body_mode: "none".to_owned(),
            body: String::new(),
            scripts: vec![],
        }],
        environments: vec![SharedEnvironment {
            name: "dev".to_owned(),
            variables: vec![KeyValue {
                key: "api_host".to_owned(),
                value: "localhost:8080".to_owned(),
            }],
        }],
    };

    let encoded =
        serialize_workspace_bundle(&payload, "super-secret").expect("bundle should encode");
    let decoded =
        deserialize_workspace_bundle(&encoded, "super-secret").expect("bundle should decode");

    assert_eq!(decoded.endpoints.len(), 1);
    assert_eq!(decoded.endpoints[0].url, "https://example.com/health");
    assert_eq!(decoded.environments.len(), 1);
    assert_eq!(decoded.environments[0].name, "dev");
    assert_eq!(decoded.environments[0].variables[0].key, "api_host");
}

#[test]
fn workspace_bundle_rejects_wrong_password() {
    let payload = SharedWorkspacePayload {
        version: 1,
        endpoints: vec![],
        environments: vec![],
    };

    let encoded =
        serialize_workspace_bundle(&payload, "correct-horse").expect("bundle should encode");
    let err = deserialize_workspace_bundle(&encoded, "wrong-password")
        .expect_err("wrong password should fail");

    assert!(
        err.contains("decryption failed") || err.contains("password"),
        "unexpected error: {err}"
    );
}

#[test]
fn workspace_ui_state_roundtrip_preserves_open_tabs() {
    let state = WorkspaceUiState {
        active_tab_id: Some("tab-1".to_owned()),
        open_tabs: vec![PersistedRequestTab {
            id: "tab-1".to_owned(),
            saved_endpoint_id: Some("ep-1".to_owned()),
            draft: Endpoint {
                id: "ep-1".to_owned(),
                source_request_id: String::new(),
                source_collection_id: String::new(),
                source_folder_id: String::new(),
                name: "Users".to_owned(),
                collection: "Core".to_owned(),
                folder_path: "Admin".to_owned(),
                method: "GET".to_owned(),
                url: "https://example.com/users".to_owned(),
                query_params: vec![],
                headers: vec![],
                body_mode: "none".to_owned(),
                body: String::new(),
                scripts: vec![],
            },
            is_dirty: true,
            editor_tab: RequestEditorTab::Headers,
            response_view_tab: ResponseViewTab::Pretty,
            response: ResponseState {
                status_code: Some(200),
                status_text: "OK".to_owned(),
                duration_ms: Some(12),
                headers: vec![KeyValue {
                    key: "Content-Type".to_owned(),
                    value: "application/json".to_owned(),
                }],
                body: "{\"ok\":true}".to_owned(),
                error: None,
            },
            scripts_ran: 1,
        }],
    };

    let encoded = serde_json::to_string(&state).expect("workspace ui should serialize");
    let decoded: WorkspaceUiState =
        serde_json::from_str(&encoded).expect("workspace ui should deserialize");

    assert_eq!(decoded.active_tab_id.as_deref(), Some("tab-1"));
    assert_eq!(decoded.open_tabs.len(), 1);
    assert!(decoded.open_tabs[0].is_dirty);
    assert_eq!(decoded.open_tabs[0].editor_tab, RequestEditorTab::Headers);
    assert_eq!(decoded.open_tabs[0].response.status_code, Some(200));
    assert_eq!(decoded.open_tabs[0].scripts_ran, 1);
}
