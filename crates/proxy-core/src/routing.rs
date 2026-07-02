use crate::{
    config::{Provider, RouteProfileConfig, RoutingConfig, SessionRoutingPolicy},
    error::{ProxyError, Result},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteSnapshot {
    pub id: String,
    pub provider: Provider,
    pub primary_model: String,
    pub small_model: String,
    pub context_window: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteStatus {
    pub active_profile: String,
    pub active_provider: Provider,
    pub session_policy: SessionRoutingPolicy,
    pub pinned_session_count: usize,
    pub routes: Vec<RouteSnapshot>,
}

#[derive(Debug, Clone)]
pub struct RouteManager {
    inner: Arc<RouteManagerInner>,
}

#[derive(Debug)]
struct RouteManagerInner {
    active_profile: RwLock<String>,
    profiles: RwLock<BTreeMap<String, RouteProfileConfig>>,
    session_routes: RwLock<HashMap<String, String>>,
    session_policy: SessionRoutingPolicy,
}

impl RouteManager {
    pub fn from_config(config: &RoutingConfig) -> Result<Self> {
        let profiles = config
            .profiles
            .iter()
            .cloned()
            .map(|profile| (profile.id.clone(), profile))
            .collect::<BTreeMap<_, _>>();
        if profiles.is_empty() {
            return Err(ProxyError::Config(
                "routing requires at least one route profile".into(),
            ));
        }
        if !profiles.contains_key(&config.active_profile) {
            return Err(ProxyError::Config(format!(
                "active route profile \"{}\" is not configured",
                config.active_profile
            )));
        }
        Ok(Self {
            inner: Arc::new(RouteManagerInner {
                active_profile: RwLock::new(config.active_profile.clone()),
                profiles: RwLock::new(profiles),
                session_routes: RwLock::new(HashMap::new()),
                session_policy: config.session_policy,
            }),
        })
    }

    pub async fn resolve_for_request(&self, session_id: Option<&str>) -> Result<RouteSnapshot> {
        match (self.inner.session_policy, session_id) {
            (SessionRoutingPolicy::PinOnFirstRequest, Some(session_id)) => {
                if let Some(profile_id) = self
                    .inner
                    .session_routes
                    .read()
                    .await
                    .get(session_id)
                    .cloned()
                {
                    return self.snapshot_for_profile(&profile_id).await;
                }

                let active_profile = self.active_profile_id().await;
                let profile_id = {
                    let mut sessions = self.inner.session_routes.write().await;
                    sessions
                        .entry(session_id.to_string())
                        .or_insert_with(|| active_profile.clone())
                        .clone()
                };
                self.snapshot_for_profile(&profile_id).await
            }
            _ => {
                let active_profile = self.active_profile_id().await;
                self.snapshot_for_profile(&active_profile).await
            }
        }
    }

    pub async fn active_route(&self) -> Result<RouteSnapshot> {
        let active_profile = self.active_profile_id().await;
        self.snapshot_for_profile(&active_profile).await
    }

    pub async fn set_active_profile(&self, profile_id: &str) -> Result<RouteSnapshot> {
        let snapshot = self.snapshot_for_profile(profile_id).await?;
        *self.inner.active_profile.write().await = profile_id.to_string();
        Ok(snapshot)
    }

    pub async fn status(&self) -> Result<RouteStatus> {
        let active_profile = self.active_profile_id().await;
        let active = self.snapshot_for_profile(&active_profile).await?;
        let mut routes = self
            .inner
            .profiles
            .read()
            .await
            .values()
            .cloned()
            .map(RouteSnapshot::from)
            .collect::<Vec<_>>();
        routes.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(RouteStatus {
            active_profile,
            active_provider: active.provider,
            session_policy: self.inner.session_policy,
            pinned_session_count: self.inner.session_routes.read().await.len(),
            routes,
        })
    }

    async fn active_profile_id(&self) -> String {
        self.inner.active_profile.read().await.clone()
    }

    async fn snapshot_for_profile(&self, profile_id: &str) -> Result<RouteSnapshot> {
        let profiles = self.inner.profiles.read().await;
        profiles
            .get(profile_id)
            .cloned()
            .map(RouteSnapshot::from)
            .ok_or_else(|| {
                ProxyError::Config(format!("route profile \"{profile_id}\" is not configured"))
            })
    }
}

impl From<RouteProfileConfig> for RouteSnapshot {
    fn from(profile: RouteProfileConfig) -> Self {
        Self {
            id: profile.id,
            provider: profile.provider,
            primary_model: profile.primary_model,
            small_model: profile.small_model,
            context_window: profile.context_window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_route_profiles;

    #[tokio::test]
    async fn pins_session_to_first_route() {
        let manager = RouteManager::from_config(&RoutingConfig {
            active_profile: "codex".into(),
            session_policy: SessionRoutingPolicy::PinOnFirstRequest,
            profiles: default_route_profiles(),
        })
        .unwrap();

        let first = manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        assert_eq!(first.provider, Provider::Codex);

        manager.set_active_profile("deepseek").await.unwrap();
        let pinned = manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        assert_eq!(pinned.provider, Provider::Codex);
        let fresh = manager
            .resolve_for_request(Some("session-b"))
            .await
            .unwrap();
        assert_eq!(fresh.provider, Provider::DeepSeek);
    }

    #[tokio::test]
    async fn immediate_policy_uses_current_route() {
        let manager = RouteManager::from_config(&RoutingConfig {
            active_profile: "codex".into(),
            session_policy: SessionRoutingPolicy::Immediate,
            profiles: default_route_profiles(),
        })
        .unwrap();

        assert_eq!(
            manager
                .resolve_for_request(Some("session-a"))
                .await
                .unwrap()
                .provider,
            Provider::Codex
        );
        manager.set_active_profile("deepseek").await.unwrap();
        assert_eq!(
            manager
                .resolve_for_request(Some("session-a"))
                .await
                .unwrap()
                .provider,
            Provider::DeepSeek
        );
    }
}
