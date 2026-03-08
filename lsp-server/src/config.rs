use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};

/// Runtime configuration managed by the LSP server.
/// Updated via `workspace/didChangeConfiguration`.
#[derive(Debug)]
pub struct ConfigManager {
    hide_prereleases: AtomicBool,
    pub settings: tokio::sync::RwLock<Settings>,
}

/// Persistent settings from Zed's `settings.json` under `"update-versions"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub hide_prereleases: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    #[serde(default)]
    pub npm: NpmSettings,
    #[serde(default)]
    pub cargo: CargoSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpmSettings {
    #[serde(default = "default_npm_registry")]
    pub registry: String,
    #[serde(default = "default_npm_dependency_keys")]
    pub dependency_keys: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CargoSettings {
    #[serde(default = "default_cargo_dependency_keys")]
    pub dependency_keys: Vec<String>,
}

fn default_log_level() -> String {
    "error".to_string()
}

fn default_cache_ttl_secs() -> u64 {
    300
}

fn default_npm_registry() -> String {
    "https://registry.npmjs.org".to_string()
}

fn default_npm_dependency_keys() -> Vec<String> {
    vec![
        "dependencies".to_string(),
        "devDependencies".to_string(),
        "peerDependencies".to_string(),
        "optionalDependencies".to_string(),
    ]
}

fn default_cargo_dependency_keys() -> Vec<String> {
    vec![
        "dependencies".to_string(),
        "dev-dependencies".to_string(),
        "build-dependencies".to_string(),
        "workspace.dependencies".to_string(),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hide_prereleases: false,
            log_level: default_log_level(),
            cache_ttl_secs: default_cache_ttl_secs(),
            npm: NpmSettings::default(),
            cargo: CargoSettings::default(),
        }
    }
}

impl Default for NpmSettings {
    fn default() -> Self {
        Self {
            registry: default_npm_registry(),
            dependency_keys: default_npm_dependency_keys(),
        }
    }
}

impl Default for CargoSettings {
    fn default() -> Self {
        Self {
            dependency_keys: default_cargo_dependency_keys(),
        }
    }
}

impl ConfigManager {
    pub fn new() -> Self {
        let settings = Settings::default();
        Self {
            hide_prereleases: AtomicBool::new(settings.hide_prereleases),
            settings: tokio::sync::RwLock::new(settings),
        }
    }

    pub fn hide_prereleases(&self) -> bool {
        self.hide_prereleases.load(Ordering::Relaxed)
    }

    pub async fn update_settings(&self, new_settings: Settings) {
        self.hide_prereleases
            .store(new_settings.hide_prereleases, Ordering::Relaxed);
        *self.settings.write().await = new_settings;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = Settings::default();
        assert!(!settings.hide_prereleases);
        assert_eq!(settings.log_level, "error");
        assert_eq!(settings.cache_ttl_secs, 300);
        assert_eq!(settings.npm.registry, "https://registry.npmjs.org");
        assert_eq!(settings.npm.dependency_keys.len(), 4);
        assert_eq!(settings.cargo.dependency_keys.len(), 4);
    }

    #[tokio::test]
    async fn test_config_manager_update_settings() {
        let config = ConfigManager::new();
        assert!(!config.hide_prereleases());

        let new_settings = Settings {
            hide_prereleases: true,
            ..Settings::default()
        };
        config.update_settings(new_settings).await;

        assert!(config.hide_prereleases());
    }

    #[test]
    fn test_deserialize_settings() {
        let json = r#"{
            "hidePrereleases": false,
            "logLevel": "debug",
            "cacheTtlSecs": 600,
            "npm": {
                "registry": "https://custom.registry.com",
                "dependencyKeys": ["dependencies"]
            }
        }"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert!(!settings.hide_prereleases);
        assert_eq!(settings.log_level, "debug");
        assert_eq!(settings.cache_ttl_secs, 600);
        assert_eq!(settings.npm.registry, "https://custom.registry.com");
        assert_eq!(settings.npm.dependency_keys, vec!["dependencies"]);
    }

    #[test]
    fn test_deserialize_partial_settings() {
        let json = r#"{"hidePrereleases": true}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert!(settings.hide_prereleases);
        assert_eq!(settings.npm.registry, "https://registry.npmjs.org");
    }
}
