use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use walkdir::WalkDir;

use crate::domain::{
    decrypt_bytes, default_endpoints, default_environment_index, default_environments,
    default_variables_for_environment_name, encrypt_bytes, normalize_endpoint_url_and_query_params,
    normalize_folder_path, read_json_or_default, safe_path_segment, split_folder_path,
    write_json_pretty,
};
use crate::models::{
    AppConfig, EncryptedBlob, Endpoint, Environment, EnvironmentFile, EnvironmentIndexEntry,
    KeyMaterial, SecurityMetadata,
};
use crate::request_body::normalize_body_mode_owned;

#[derive(Debug)]
pub(crate) struct AppStorage {
    pub(crate) base_dir: PathBuf,
    endpoints_path: PathBuf,
    requests_dir: PathBuf,
    environments_index_path: PathBuf,
    environments_dir: PathBuf,
    config_path: PathBuf,
    security_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CoreData {
    pub(crate) endpoints: Vec<Endpoint>,
    pub(crate) environment_index: Vec<EnvironmentIndexEntry>,
    pub(crate) config: AppConfig,
}

impl AppStorage {
    pub(crate) fn new() -> Self {
        let fallback_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("mailman-data");
        let base_dir = ProjectDirs::from("com", "mailman", "mailman")
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

    pub(crate) fn load_core_data(&self) -> io::Result<CoreData> {
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
            normalize_endpoint_url_and_query_params(endpoint);
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

    pub(crate) fn load_config(&self) -> io::Result<AppConfig> {
        self.ensure_directories()?;
        Ok(read_json_or_default::<AppConfig>(&self.config_path)?.unwrap_or_default())
    }

    pub(crate) fn load_security_metadata(&self) -> io::Result<Option<SecurityMetadata>> {
        self.ensure_directories()?;
        read_json_or_default::<SecurityMetadata>(&self.security_path)
    }

    pub(crate) fn save_security_metadata(&self, metadata: &SecurityMetadata) -> io::Result<()> {
        self.ensure_directories()?;
        write_json_pretty(&self.security_path, metadata)
    }

    pub(crate) fn load_environments(
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

    pub(crate) fn save_all(
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

    pub(crate) fn save_config(&self, config: &AppConfig) -> io::Result<()> {
        self.ensure_directories()?;
        write_json_pretty(&self.config_path, config)
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
