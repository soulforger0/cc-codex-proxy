use crate::{
    config::{Provider, DEFAULT_PUBLIC_PRIMARY_MODEL, DEFAULT_PUBLIC_SMALL_MODEL},
    error::{ProxyError, Result},
    routing::RouteSnapshot,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ModelProfile {
    pub provider: Provider,
    pub id: String,
    pub upstream_model: String,
    pub context_window: u32,
    pub supports_fast: bool,
    pub default_small_fast: bool,
}

impl Default for ModelProfile {
    fn default() -> Self {
        Self {
            provider: Provider::Codex,
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
    pub provider: Provider,
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
                let mut profiles = serde_json::from_str::<Vec<ModelProfile>>(&raw)?;
                if merge_missing_default_profiles(&mut profiles) {
                    fs::write(path, serde_json::to_string_pretty(&profiles)?)?;
                }
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

    pub fn resolve(&self, provider: Provider, incoming: &str) -> Result<ResolvedModel> {
        let stripped = strip_context_hint(incoming);
        let (base, service_tier) = split_fast_suffix(&stripped);
        if let Some(profile) = self
            .find_profile(provider, &base)
            .or_else(|| self.compatibility_profile(provider, &base))
        {
            if service_tier.is_some() && !profile.supports_fast {
                return Err(ProxyError::InvalidRequest(format!(
                    "Model \"{}\" does not support -fast routing",
                    profile.id
                )));
            }
            return Ok(ResolvedModel {
                provider,
                requested: incoming.to_string(),
                public_id: profile.id.clone(),
                upstream_model: profile.upstream_model.clone(),
                service_tier,
                context_window: profile.context_window,
            });
        }

        if provider == Provider::CustomOpenAI {
            return Ok(ResolvedModel {
                provider,
                requested: incoming.to_string(),
                public_id: stripped.clone(),
                upstream_model: stripped,
                service_tier: None,
                context_window: 128_000,
            });
        }

        Err(ProxyError::InvalidRequest(format!(
            "Unknown {} model \"{incoming}\". Supported: {}.",
            provider.as_str(),
            self.supported_models(provider).join(", ")
        )))
    }

    pub fn resolve_for_route(
        &self,
        route: &RouteSnapshot,
        public_primary_model: &str,
        public_small_model: &str,
        incoming: &str,
    ) -> Result<ResolvedModel> {
        let stripped = strip_context_hint(incoming);
        let (base, service_tier) = split_fast_suffix(&stripped);
        let public_primary = strip_context_hint(public_primary_model);
        let public_small = strip_context_hint(public_small_model);

        if base == public_small || is_claude_small_model_alias(&base) {
            return self.resolve_route_model(
                route,
                incoming,
                public_small_model,
                &route.small_model,
                service_tier,
            );
        }
        if base == public_primary || is_claude_primary_model_alias(&base) {
            return self.resolve_route_model(
                route,
                incoming,
                public_primary_model,
                &route.primary_model,
                service_tier,
            );
        }

        self.resolve(route.provider, incoming)
    }

    fn resolve_route_model(
        &self,
        route: &RouteSnapshot,
        incoming: &str,
        public_id: &str,
        upstream: &str,
        service_tier: Option<String>,
    ) -> Result<ResolvedModel> {
        let upstream = strip_context_hint(upstream);
        let profile = self.find_profile(route.provider, &upstream);
        if service_tier.is_some()
            && !profile
                .map(|profile| profile.supports_fast)
                .unwrap_or(false)
        {
            return Err(ProxyError::InvalidRequest(format!(
                "Model \"{}\" does not support -fast routing",
                strip_context_hint(public_id)
            )));
        }
        Ok(ResolvedModel {
            provider: route.provider,
            requested: incoming.to_string(),
            public_id: strip_context_hint(public_id),
            upstream_model: profile
                .map(|profile| profile.upstream_model.clone())
                .unwrap_or(upstream),
            service_tier,
            context_window: profile
                .map(|profile| profile.context_window)
                .unwrap_or(route.context_window),
        })
    }

    fn find_profile(&self, provider: Provider, model: &str) -> Option<&ModelProfile> {
        self.profiles.iter().find(|profile| {
            profile.provider == provider && (profile.id == model || profile.upstream_model == model)
        })
    }

    fn compatibility_profile(&self, provider: Provider, model: &str) -> Option<&ModelProfile> {
        if provider != Provider::CustomOpenAI {
            let stripped = strip_context_hint(model);
            if is_claude_small_model_alias(&stripped) {
                return self.default_small_fast(provider);
            }
            if is_claude_primary_model_alias(&stripped) {
                return self.default_primary(provider);
            }
        }

        if provider != Provider::DeepSeek || !model.starts_with("gpt-") {
            return None;
        }
        let target = if model.contains("mini") {
            "deepseek-v4-flash"
        } else {
            "deepseek-v4-pro"
        };
        self.find_profile(provider, target)
    }

    pub fn supported_models(&self, provider: Provider) -> Vec<String> {
        let mut out = BTreeSet::new();
        for profile in self
            .profiles
            .iter()
            .filter(|profile| profile.provider == provider)
        {
            out.insert(profile.id.clone());
            if profile.supports_fast {
                out.insert(format!("{}-fast", profile.id));
            }
        }
        out.into_iter().collect()
    }

    pub fn default_small_fast(&self, provider: Provider) -> Option<&ModelProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.provider == provider && profile.default_small_fast)
    }

    fn default_primary(&self, provider: Provider) -> Option<&ModelProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.provider == provider && !profile.default_small_fast)
    }
}

pub fn strip_context_hint(model: &str) -> String {
    model.strip_suffix("[1m]").unwrap_or(model).to_string()
}

fn split_fast_suffix(model: &str) -> (String, Option<String>) {
    if let Some(base) = model.strip_suffix("-fast") {
        (base.to_string(), Some("priority".to_string()))
    } else {
        (model.to_string(), None)
    }
}

fn is_claude_model_alias(model: &str) -> bool {
    model
        == DEFAULT_PUBLIC_PRIMARY_MODEL
            .strip_suffix("[1m]")
            .unwrap_or(DEFAULT_PUBLIC_PRIMARY_MODEL)
        || model
            == DEFAULT_PUBLIC_SMALL_MODEL
                .strip_suffix("[1m]")
                .unwrap_or(DEFAULT_PUBLIC_SMALL_MODEL)
        || model.starts_with("claude-")
}

fn is_claude_small_model_alias(model: &str) -> bool {
    is_claude_model_alias(model) && model.contains("haiku")
}

fn is_claude_primary_model_alias(model: &str) -> bool {
    is_claude_model_alias(model) && !is_claude_small_model_alias(model)
}

pub fn default_profiles() -> Vec<ModelProfile> {
    vec![
        ModelProfile {
            provider: Provider::Codex,
            id: "gpt-5.5".into(),
            upstream_model: "gpt-5.5".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::Codex,
            id: "gpt-5.4".into(),
            upstream_model: "gpt-5.4".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::Codex,
            id: "gpt-5.4-mini".into(),
            upstream_model: "gpt-5.4-mini".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: true,
        },
        ModelProfile {
            provider: Provider::Codex,
            id: "gpt-5.3-codex".into(),
            upstream_model: "gpt-5.3-codex".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::Codex,
            id: "gpt-5.3-codex-spark".into(),
            upstream_model: "gpt-5.3-codex-spark".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::Codex,
            id: "gpt-5.2".into(),
            upstream_model: "gpt-5.2".into(),
            context_window: 272_000,
            supports_fast: true,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::DeepSeek,
            id: "deepseek-v4-pro".into(),
            upstream_model: "deepseek-v4-pro".into(),
            context_window: 1_000_000,
            supports_fast: false,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::DeepSeek,
            id: "deepseek-v4-flash".into(),
            upstream_model: "deepseek-v4-flash".into(),
            context_window: 1_000_000,
            supports_fast: false,
            default_small_fast: true,
        },
        ModelProfile {
            provider: Provider::CustomOpenAI,
            id: "gpt-5.4".into(),
            upstream_model: "gpt-5.4".into(),
            context_window: 128_000,
            supports_fast: false,
            default_small_fast: false,
        },
        ModelProfile {
            provider: Provider::CustomOpenAI,
            id: "gpt-5.4-mini".into(),
            upstream_model: "gpt-5.4-mini".into(),
            context_window: 128_000,
            supports_fast: false,
            default_small_fast: true,
        },
    ]
}

fn merge_missing_default_profiles(profiles: &mut Vec<ModelProfile>) -> bool {
    let mut changed = false;
    for default in default_profiles() {
        let exists = profiles
            .iter()
            .any(|profile| profile.provider == default.provider && profile.id == default.id);
        if !exists {
            profiles.push(default);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::RouteSnapshot;

    #[test]
    fn resolves_context_hint_and_fast_suffix() {
        let registry = ModelRegistry::from_profiles(default_profiles());
        let resolved = registry
            .resolve(Provider::Codex, "gpt-5.4-fast[1m]")
            .unwrap();
        assert_eq!(resolved.upstream_model, "gpt-5.4");
        assert_eq!(resolved.service_tier.as_deref(), Some("priority"));
    }

    #[test]
    fn strips_context_hint() {
        assert_eq!(strip_context_hint("gpt-5.4[1m]"), "gpt-5.4");
    }

    #[test]
    fn resolves_deepseek_defaults_by_provider() {
        let registry = ModelRegistry::from_profiles(default_profiles());
        let resolved = registry
            .resolve(Provider::DeepSeek, "deepseek-v4-pro[1m]")
            .unwrap();
        assert_eq!(resolved.upstream_model, "deepseek-v4-pro");
        assert_eq!(resolved.context_window, 1_000_000);
        assert_eq!(
            registry.default_small_fast(Provider::DeepSeek).unwrap().id,
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn deepseek_accepts_stale_codex_model_defaults() {
        let registry = ModelRegistry::from_profiles(default_profiles());

        let primary = registry.resolve(Provider::DeepSeek, "gpt-5.5[1m]").unwrap();
        assert_eq!(primary.upstream_model, "deepseek-v4-pro");
        assert_eq!(primary.public_id, "deepseek-v4-pro");

        let small = registry
            .resolve(Provider::DeepSeek, "gpt-5.4-mini[1m]")
            .unwrap();
        assert_eq!(small.upstream_model, "deepseek-v4-flash");
        assert_eq!(small.public_id, "deepseek-v4-flash");
    }

    #[test]
    fn provider_resolve_accepts_claude_public_aliases() {
        let registry = ModelRegistry::from_profiles(default_profiles());

        let codex = registry
            .resolve(Provider::Codex, DEFAULT_PUBLIC_PRIMARY_MODEL)
            .unwrap();
        assert_eq!(codex.upstream_model, "gpt-5.5");

        let deepseek = registry
            .resolve(Provider::DeepSeek, DEFAULT_PUBLIC_SMALL_MODEL)
            .unwrap();
        assert_eq!(deepseek.upstream_model, "deepseek-v4-flash");
    }

    #[test]
    fn route_alias_maps_to_active_upstream_model() {
        let registry = ModelRegistry::from_profiles(default_profiles());
        let route = RouteSnapshot {
            id: "deepseek".into(),
            provider: Provider::DeepSeek,
            primary_model: "deepseek-v4-pro".into(),
            small_model: "deepseek-v4-flash".into(),
            context_window: 1_000_000,
        };

        let primary = registry
            .resolve_for_route(
                &route,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
            )
            .unwrap();
        assert_eq!(primary.upstream_model, "deepseek-v4-pro");
        assert_eq!(primary.public_id, DEFAULT_PUBLIC_PRIMARY_MODEL);

        let small = registry
            .resolve_for_route(
                &route,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
            )
            .unwrap();
        assert_eq!(small.upstream_model, "deepseek-v4-flash");
    }

    #[test]
    fn alternate_claude_aliases_follow_route_model_class() {
        let registry = ModelRegistry::from_profiles(default_profiles());
        let route = RouteSnapshot {
            id: "codex".into(),
            provider: Provider::Codex,
            primary_model: "gpt-5.5".into(),
            small_model: "gpt-5.4-mini".into(),
            context_window: 272_000,
        };

        let sonnet = registry
            .resolve_for_route(
                &route,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
                "claude-sonnet-4-5",
            )
            .unwrap();
        assert_eq!(sonnet.upstream_model, "gpt-5.5");

        let haiku = registry
            .resolve_for_route(
                &route,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
                "claude-haiku-4-5",
            )
            .unwrap();
        assert_eq!(haiku.upstream_model, "gpt-5.4-mini");
    }

    #[test]
    fn custom_route_alias_uses_configured_upstream_model() {
        let registry = ModelRegistry::from_profiles(default_profiles());
        let route = RouteSnapshot {
            id: "custom-local".into(),
            provider: Provider::CustomOpenAI,
            primary_model: "llama-3.3-70b".into(),
            small_model: "llama-3.2-3b".into(),
            context_window: 128_000,
        };

        let resolved = registry
            .resolve_for_route(
                &route,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
            )
            .unwrap();
        assert_eq!(resolved.upstream_model, "llama-3.3-70b");
        assert_eq!(resolved.context_window, 128_000);
    }

    #[test]
    fn migrates_missing_deepseek_profiles() {
        let mut profiles = vec![ModelProfile {
            provider: Provider::Codex,
            id: "custom".into(),
            upstream_model: "custom".into(),
            context_window: 123,
            supports_fast: false,
            default_small_fast: false,
        }];
        assert!(merge_missing_default_profiles(&mut profiles));
        assert!(profiles.iter().any(
            |profile| profile.provider == Provider::DeepSeek && profile.id == "deepseek-v4-pro"
        ));
        assert!(profiles
            .iter()
            .any(|profile| profile.provider == Provider::CustomOpenAI && profile.id == "gpt-5.4"));
        assert!(profiles
            .iter()
            .any(|profile| profile.provider == Provider::Codex && profile.id == "custom"));
    }

    #[test]
    fn custom_openai_accepts_arbitrary_model_names() {
        let registry = ModelRegistry::from_profiles(default_profiles());

        let resolved = registry
            .resolve(Provider::CustomOpenAI, "llama-3.3-70b[1m]")
            .unwrap();

        assert_eq!(resolved.upstream_model, "llama-3.3-70b");
        assert_eq!(resolved.public_id, "llama-3.3-70b");
        assert_eq!(resolved.context_window, 128_000);
    }
}
