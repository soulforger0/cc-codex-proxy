use crate::error::{ProxyError, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ModelProfile {
    pub id: String,
    pub upstream_model: String,
    pub context_window: u32,
    pub supports_fast: bool,
    pub default_small_fast: bool,
}

impl Default for ModelProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            upstream_model: String::new(),
            context_window: 272_000,
            supports_fast: false,
            default_small_fast: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub requested: String,
    pub public_id: String,
    pub upstream_model: String,
    pub service_tier: Option<String>,
    pub context_window: u32,
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    profiles: Vec<ModelProfile>,
}

impl ModelRegistry {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(raw) => {
                let profiles = serde_json::from_str::<Vec<ModelProfile>>(&raw)?;
                Ok(Self { profiles })
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                let profiles = default_profiles();
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, serde_json::to_string_pretty(&profiles)?)?;
                Ok(Self { profiles })
            }
            Err(err) => Err(err.into()),
        }
    }

    pub fn from_profiles(profiles: Vec<ModelProfile>) -> Self {
        Self { profiles }
    }

    pub fn resolve(&self, incoming: &str) -> Result<ResolvedModel> {
        let stripped = strip_context_hint(incoming);
        let (base, service_tier) = if let Some(base) = stripped.strip_suffix("-fast") {
            (base.to_string(), Some("priority".to_string()))
        } else {
            (stripped, None)
        };
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.id == base || profile.upstream_model == base)
            .ok_or_else(|| {
                ProxyError::InvalidRequest(format!(
                    "Unknown model \"{incoming}\". Supported: {}.",
                    self.supported_models().join(", ")
                ))
            })?;
        if service_tier.is_some() && !profile.supports_fast {
            return Err(ProxyError::InvalidRequest(format!(
                "Model \"{}\" does not support -fast routing",
                profile.id
            )));
        }
        Ok(ResolvedModel {
            requested: incoming.to_string(),
            public_id: profile.id.clone(),
            upstream_model: profile.upstream_model.clone(),
            service_tier,
            context_window: profile.context_window,
        })
    }

    pub fn supported_models(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        for profile in &self.profiles {
            out.insert(profile.id.clone());
            if profile.supports_fast {
                out.insert(format!("{}-fast", profile.id));
            }
        }
        out.into_iter().collect()
    }

    pub fn default_small_fast(&self) -> Option<&ModelProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.default_small_fast)
    }
}

pub fn strip_context_hint(model: &str) -> String {
    model.strip_suffix("[1m]").unwrap_or(model).to_string()
}

pub fn default_profiles() -> Vec<ModelProfile> {
    vec![
        ModelProfile {
            id: "gpt-5.5".into(),
            upstream_model: "gpt-5.5".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            id: "gpt-5.4".into(),
            upstream_model: "gpt-5.4".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            id: "gpt-5.4-mini".into(),
            upstream_model: "gpt-5.4-mini".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: true,
        },
        ModelProfile {
            id: "gpt-5.3-codex".into(),
            upstream_model: "gpt-5.3-codex".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            id: "gpt-5.3-codex-spark".into(),
            upstream_model: "gpt-5.3-codex-spark".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            id: "gpt-5.2".into(),
            upstream_model: "gpt-5.2".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_context_hint_and_fast_suffix() {
        let registry = ModelRegistry::from_profiles(default_profiles());
        let resolved = registry.resolve("gpt-5.4-fast[1m]").unwrap();
        assert_eq!(resolved.upstream_model, "gpt-5.4");
        assert_eq!(resolved.service_tier.as_deref(), Some("priority"));
    }

    #[test]
    fn strips_context_hint() {
        assert_eq!(strip_context_hint("gpt-5.4[1m]"), "gpt-5.4");
    }
}
