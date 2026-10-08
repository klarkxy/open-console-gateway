//! Immutable discovered config paths and explicit target resolution.
use super::fs::{canonical_lexical_path, reject_symlink_ancestors};
use super::{ByokError, ByokResult};
use crate::byok_application::ByokClient;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub client: ByokClient,
    pub path: PathBuf,
    pub discovery_source: String,
    pub store_identity_hint: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPaths {
    #[allow(dead_code)]
    pub user_home: PathBuf,
    pub codex: ResolvedTarget,
    pub kimi: ResolvedTarget,
    pub minimax: ResolvedTarget,
    pub zcode: ResolvedTarget,
    pub copilot: ResolvedTarget,
}

impl DiscoveredPaths {
    pub fn from_env(user_home: PathBuf) -> Self {
        Self {
            codex: discover_codex(&user_home),
            kimi: discover_kimi(&user_home),
            minimax: discover_minimax(&user_home),
            zcode: discover_zcode(&user_home),
            copilot: discover_copilot(&user_home),
            user_home,
        }
    }

    pub fn default_for(&self, client: ByokClient) -> &ResolvedTarget {
        match client {
            ByokClient::Codex => &self.codex,
            ByokClient::Kimi => &self.kimi,
            ByokClient::Minimax => &self.minimax,
            ByokClient::Zcode => &self.zcode,
            ByokClient::Copilot => &self.copilot,
        }
    }

    pub fn resolve(
        &self,
        client: ByokClient,
        target_path: Option<&str>,
    ) -> ByokResult<ResolvedTarget> {
        match target_path.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(self.default_for(client).clone()),
            Some(raw) => {
                let path = PathBuf::from(raw);
                if !path.is_absolute() {
                    return Err(ByokError::invalid(
                        "Explicit BYOK target path must be absolute",
                    ));
                }
                let path = canonical_lexical_path(&path)?;
                constrain_filename(client, &path)?;
                Ok(ResolvedTarget {
                    client,
                    path,
                    discovery_source: "explicit".into(),
                    store_identity_hint: None,
                })
            }
        }
    }
}

fn discover_codex(user_home: &Path) -> ResolvedTarget {
    match nonempty_env("CODEX_HOME") {
        Some(home) => ResolvedTarget {
            client: ByokClient::Codex,
            path: PathBuf::from(home).join("config.toml"),
            discovery_source: "CODEX_HOME".into(),
            store_identity_hint: None,
        },
        None => ResolvedTarget {
            client: ByokClient::Codex,
            path: user_home.join(".codex").join("config.toml"),
            discovery_source: "default".into(),
            store_identity_hint: None,
        },
    }
}

fn discover_kimi(user_home: &Path) -> ResolvedTarget {
    match nonempty_env("KIMI_CODE_HOME") {
        Some(home) => ResolvedTarget {
            client: ByokClient::Kimi,
            path: PathBuf::from(home).join("config.toml"),
            discovery_source: "KIMI_CODE_HOME".into(),
            store_identity_hint: None,
        },
        None => ResolvedTarget {
            client: ByokClient::Kimi,
            path: user_home.join(".kimi-code").join("config.toml"),
            discovery_source: "default".into(),
            store_identity_hint: None,
        },
    }
}

fn discover_minimax(user_home: &Path) -> ResolvedTarget {
    if let Some(dir) = nonempty_env("MINIMAX_DATA_DIR") {
        return ResolvedTarget {
            client: ByokClient::Minimax,
            path: PathBuf::from(dir).join("config.yaml"),
            discovery_source: "MINIMAX_DATA_DIR".into(),
            store_identity_hint: None,
        };
    }
    if let Some(dir) = nonempty_env("MAVIS_DATA_DIR") {
        return ResolvedTarget {
            client: ByokClient::Minimax,
            path: PathBuf::from(dir).join("config.yaml"),
            discovery_source: "MAVIS_DATA_DIR".into(),
            store_identity_hint: None,
        };
    }
    ResolvedTarget {
        client: ByokClient::Minimax,
        path: user_home.join(".minimax").join("config.yaml"),
        discovery_source: "default".into(),
        store_identity_hint: None,
    }
}

fn discover_zcode(user_home: &Path) -> ResolvedTarget {
    if let Some(file) = nonempty_env("ZCODE_PERSONAL_PROVIDER_CONFIG_FILE") {
        return ResolvedTarget {
            client: ByokClient::Zcode,
            path: PathBuf::from(file),
            discovery_source: "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE".into(),
            store_identity_hint: None,
        };
    }
    let base = match nonempty_env("ZCODE_DATA_BASE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => user_home.to_path_buf(),
    };
    let source = if nonempty_env("ZCODE_DATA_BASE_DIR").is_some() {
        "ZCODE_DATA_BASE_DIR"
    } else {
        "default"
    };
    ResolvedTarget {
        client: ByokClient::Zcode,
        path: base.join(".zcode").join("v2").join("provider_config.json"),
        discovery_source: source.into(),
        store_identity_hint: None,
    }
}

fn discover_copilot(user_home: &Path) -> ResolvedTarget {
    if let Some(portable) = nonempty_env("VSCODE_PORTABLE") {
        return ResolvedTarget {
            client: ByokClient::Copilot,
            path: PathBuf::from(portable).join("user-data/User/chatLanguageModels.json"),
            discovery_source: "VSCODE_PORTABLE".into(),
            store_identity_hint: None,
        };
    }
    if let Some(appdata) = nonempty_env("VSCODE_APPDATA") {
        return copilot_in_config_base(&PathBuf::from(appdata));
    }
    #[cfg(target_os = "windows")]
    let base = nonempty_env("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join("AppData/Roaming"));
    #[cfg(target_os = "macos")]
    let base = user_home.join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = nonempty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".config"));
    copilot_in_config_base(&base)
}

fn copilot_in_config_base(base: &Path) -> ResolvedTarget {
    let stable = base.join("Code/User");
    let insiders = base.join("Code - Insiders/User");
    let (directory, source) = if !stable.is_dir() && insiders.is_dir() {
        (insiders, "vscode-insiders")
    } else {
        (stable, "vscode")
    };
    ResolvedTarget {
        client: ByokClient::Copilot,
        path: directory.join("chatLanguageModels.json"),
        discovery_source: source.into(),
        store_identity_hint: None,
    }
}

#[cfg(test)]
mod tests;

fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn constrain_filename(client: ByokClient, path: &Path) -> ByokResult<()> {
    let canonical = std::fs::canonicalize(path).ok();
    let selected = canonical.as_deref().unwrap_or(path);
    let name = selected
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ByokError::invalid("BYOK target path is missing a file name"))?;
    match client {
        ByokClient::Codex | ByokClient::Kimi => {
            if name != "config.toml" {
                return Err(ByokError::invalid(
                    "This client requires a config.toml target file",
                ));
            }
        }
        ByokClient::Minimax => {
            if name != "config.yaml" {
                return Err(ByokError::invalid(
                    "This client requires a config.yaml target file",
                ));
            }
        }
        ByokClient::Copilot => {
            if name != "chatLanguageModels.json" {
                return Err(ByokError::invalid(
                    "VS Code requires a chatLanguageModels.json target file",
                ));
            }
        }
        ByokClient::Zcode => {
            let ok = name.len() > 5 && name.to_ascii_lowercase().ends_with(".json");
            if !ok {
                return Err(ByokError::invalid(
                    "This client requires a JSON provider config file",
                ));
            }
        }
    }
    Ok(())
}

pub fn catalog_path(target: &ResolvedTarget) -> Option<PathBuf> {
    match target.client {
        ByokClient::Codex => target
            .path
            .parent()
            .map(|parent| parent.join(".ocg-byok").join("model_catalog.json")),
        _ => None,
    }
}

pub fn parent_is_safe(path: &Path) -> bool {
    path.parent().is_some() && reject_symlink_ancestors(path).is_ok()
}
