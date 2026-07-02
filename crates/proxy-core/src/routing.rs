use crate::{
    config::{Provider, RouteProfileConfig, RoutingConfig, SessionRoutingPolicy},
    error::{ProxyError, Result},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{ErrorKind, Write},
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
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
    pub session_pin_ttl_seconds: u64,
    pub max_pinned_sessions: usize,
    pub persist_session_pins: bool,
    pub routes: Vec<RouteSnapshot>,
}

const SESSION_PIN_PERSIST_TOUCH_INTERVAL_MS: i64 = 60_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SessionRoutePin {
    profile_id: String,
    first_seen_ms: i64,
    last_seen_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct StoredRoutePins {
    version: u32,
    sessions: HashMap<String, SessionRoutePin>,
}

#[derive(Debug, Clone)]
struct RoutePinStore {
    path: PathBuf,
}

impl RoutePinStore {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<HashMap<String, SessionRoutePin>> {
        match fs::read_to_string(&self.path) {
            Ok(raw) if raw.trim().is_empty() => Ok(HashMap::new()),
            Ok(raw) => Ok(serde_json::from_str::<StoredRoutePins>(&raw)?.sessions),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(HashMap::new()),
            Err(err) => Err(err.into()),
        }
    }

    fn save(&self, sessions: &HashMap<String, SessionRoutePin>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        let stored = StoredRoutePins {
            version: 1,
            sessions: sessions.clone(),
        };
        writeln!(file, "{}", serde_json::to_string_pretty(&stored)?)?;
        Ok(())
    }
}

trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Debug)]
struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}

#[derive(Debug, Clone)]
pub struct RouteManager {
    inner: Arc<RouteManagerInner>,
}

struct RouteManagerInner {
    active_profile: RwLock<String>,
    profiles: RwLock<BTreeMap<String, RouteProfileConfig>>,
    session_routes: RwLock<HashMap<String, SessionRoutePin>>,
    session_policy: SessionRoutingPolicy,
    session_pin_ttl_seconds: u64,
    max_pinned_sessions: usize,
    persist_session_pins: bool,
    store: Option<RoutePinStore>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for RouteManagerInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteManagerInner")
            .field("session_policy", &self.session_policy)
            .field("session_pin_ttl_seconds", &self.session_pin_ttl_seconds)
            .field("max_pinned_sessions", &self.max_pinned_sessions)
            .field("persist_session_pins", &self.persist_session_pins)
            .finish_non_exhaustive()
    }
}

impl RouteManager {
    pub fn from_config(config: &RoutingConfig) -> Result<Self> {
        Self::from_config_inner(config, None, Arc::new(SystemClock))
    }

    pub fn from_config_and_store(
        config: &RoutingConfig,
        store_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let store = config
            .persist_session_pins
            .then(|| RoutePinStore::new(store_path));
        Self::from_config_inner(config, store, Arc::new(SystemClock))
    }

    fn from_config_inner(
        config: &RoutingConfig,
        store: Option<RoutePinStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
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
        let mut session_routes = store
            .as_ref()
            .map(RoutePinStore::load)
            .transpose()?
            .unwrap_or_default();
        let profile_ids = profiles.keys().cloned().collect::<BTreeSet<_>>();
        prune_sessions(
            &mut session_routes,
            &profile_ids,
            clock.now_ms(),
            config.session_pin_ttl_seconds,
        );
        evict_excess_sessions(&mut session_routes, config.max_pinned_sessions);
        Ok(Self {
            inner: Arc::new(RouteManagerInner {
                active_profile: RwLock::new(config.active_profile.clone()),
                profiles: RwLock::new(profiles),
                session_routes: RwLock::new(session_routes),
                session_policy: config.session_policy,
                session_pin_ttl_seconds: config.session_pin_ttl_seconds,
                max_pinned_sessions: config.max_pinned_sessions,
                persist_session_pins: store.is_some(),
                store,
                clock,
            }),
        })
    }

    pub async fn resolve_for_request(&self, session_id: Option<&str>) -> Result<RouteSnapshot> {
        match (self.inner.session_policy, session_id) {
            (SessionRoutingPolicy::PinOnFirstRequest, Some(session_id)) => {
                let (profile_id, changed) = self.resolve_pinned_profile(session_id).await?;
                if changed {
                    self.persist_session_routes().await?;
                }
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
            session_pin_ttl_seconds: self.inner.session_pin_ttl_seconds,
            max_pinned_sessions: self.inner.max_pinned_sessions,
            persist_session_pins: self.inner.persist_session_pins,
            routes,
        })
    }

    async fn resolve_pinned_profile(&self, session_id: &str) -> Result<(String, bool)> {
        let now = self.inner.clock.now_ms();
        let active_profile = self.active_profile_id().await;
        let profile_ids = self.profile_ids().await;
        let mut changed = false;
        let mut sessions = self.inner.session_routes.write().await;
        changed |= prune_sessions(
            &mut sessions,
            &profile_ids,
            now,
            self.inner.session_pin_ttl_seconds,
        );

        if self.inner.max_pinned_sessions == 0 {
            changed |= !sessions.is_empty();
            sessions.clear();
            return Ok((active_profile, changed));
        }

        if let Some(pin) = sessions.get_mut(session_id) {
            if !pin_is_expired(pin, now, self.inner.session_pin_ttl_seconds)
                && profile_ids.contains(&pin.profile_id)
            {
                let should_persist =
                    now.saturating_sub(pin.last_seen_ms) >= SESSION_PIN_PERSIST_TOUCH_INTERVAL_MS;
                pin.last_seen_ms = now;
                return Ok((pin.profile_id.clone(), changed || should_persist));
            }
        }
        sessions.remove(session_id);

        sessions.insert(
            session_id.to_string(),
            SessionRoutePin {
                profile_id: active_profile.clone(),
                first_seen_ms: now,
                last_seen_ms: now,
            },
        );
        changed = true;
        changed |= evict_excess_sessions(&mut sessions, self.inner.max_pinned_sessions);
        Ok((active_profile, changed))
    }

    async fn active_profile_id(&self) -> String {
        self.inner.active_profile.read().await.clone()
    }

    async fn profile_ids(&self) -> BTreeSet<String> {
        self.inner.profiles.read().await.keys().cloned().collect()
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

    async fn persist_session_routes(&self) -> Result<()> {
        let Some(store) = &self.inner.store else {
            return Ok(());
        };
        let sessions = self.inner.session_routes.read().await.clone();
        store.save(&sessions)
    }
}

fn prune_sessions(
    sessions: &mut HashMap<String, SessionRoutePin>,
    profile_ids: &BTreeSet<String>,
    now_ms: i64,
    ttl_seconds: u64,
) -> bool {
    let before = sessions.len();
    sessions.retain(|_, pin| {
        profile_ids.contains(&pin.profile_id) && !pin_is_expired(pin, now_ms, ttl_seconds)
    });
    sessions.len() != before
}

fn pin_is_expired(pin: &SessionRoutePin, now_ms: i64, ttl_seconds: u64) -> bool {
    ttl_seconds > 0
        && now_ms.saturating_sub(pin.last_seen_ms) > ttl_seconds.saturating_mul(1000) as i64
}

fn evict_excess_sessions(
    sessions: &mut HashMap<String, SessionRoutePin>,
    max_sessions: usize,
) -> bool {
    if sessions.len() <= max_sessions {
        return false;
    }
    let mut by_last_seen = sessions
        .iter()
        .map(|(session_id, pin)| (pin.last_seen_ms, pin.first_seen_ms, session_id.clone()))
        .collect::<Vec<_>>();
    by_last_seen.sort();
    let remove_count = sessions.len() - max_sessions;
    for (_, _, session_id) in by_last_seen.into_iter().take(remove_count) {
        sessions.remove(&session_id);
    }
    true
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
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ManualClock {
        now_ms: Mutex<i64>,
    }

    impl ManualClock {
        fn new(now_ms: i64) -> Self {
            Self {
                now_ms: Mutex::new(now_ms),
            }
        }

        fn set(&self, now_ms: i64) {
            *self.now_ms.lock().unwrap() = now_ms;
        }
    }

    impl Clock for ManualClock {
        fn now_ms(&self) -> i64 {
            *self.now_ms.lock().unwrap()
        }
    }

    fn routing_config() -> RoutingConfig {
        RoutingConfig {
            active_profile: "codex".into(),
            session_policy: SessionRoutingPolicy::PinOnFirstRequest,
            profiles: default_route_profiles(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn pins_session_to_first_route() {
        let manager = RouteManager::from_config(&routing_config()).unwrap();

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
            ..Default::default()
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

    #[tokio::test]
    async fn persisted_pins_load_and_are_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("route-pins.json");
        let clock = Arc::new(ManualClock::new(1_000));
        let manager = RouteManager::from_config_inner(
            &routing_config(),
            Some(RoutePinStore::new(path.clone())),
            clock.clone(),
        )
        .unwrap();
        manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        manager.set_active_profile("deepseek").await.unwrap();
        clock.set(2_000);
        manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();

        let restored = RouteManager::from_config_inner(
            &RoutingConfig {
                active_profile: "deepseek".into(),
                ..routing_config()
            },
            Some(RoutePinStore::new(path)),
            Arc::new(ManualClock::new(3_000)),
        )
        .unwrap();
        let pinned = restored
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        assert_eq!(pinned.provider, Provider::Codex);
    }

    #[tokio::test]
    async fn last_seen_refreshes_on_reuse() {
        let clock = Arc::new(ManualClock::new(1_000));
        let manager =
            RouteManager::from_config_inner(&routing_config(), None, clock.clone()).unwrap();
        manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        clock.set(5_000);
        manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();

        let sessions = manager.inner.session_routes.read().await;
        assert_eq!(sessions["session-a"].last_seen_ms, 5_000);
        assert_eq!(sessions["session-a"].first_seen_ms, 1_000);
    }

    #[tokio::test]
    async fn expired_pin_uses_current_active_profile() {
        let clock = Arc::new(ManualClock::new(1_000));
        let config = RoutingConfig {
            session_pin_ttl_seconds: 1,
            ..routing_config()
        };
        let manager = RouteManager::from_config_inner(&config, None, clock.clone()).unwrap();
        manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        manager.set_active_profile("deepseek").await.unwrap();
        clock.set(3_001);

        let rerouted = manager
            .resolve_for_request(Some("session-a"))
            .await
            .unwrap();
        assert_eq!(rerouted.provider, Provider::DeepSeek);
    }

    #[tokio::test]
    async fn evicts_least_recently_seen_pins() {
        let clock = Arc::new(ManualClock::new(1_000));
        let config = RoutingConfig {
            max_pinned_sessions: 2,
            ..routing_config()
        };
        let manager = RouteManager::from_config_inner(&config, None, clock.clone()).unwrap();
        manager.resolve_for_request(Some("a")).await.unwrap();
        clock.set(2_000);
        manager.resolve_for_request(Some("b")).await.unwrap();
        clock.set(3_000);
        manager.resolve_for_request(Some("c")).await.unwrap();

        let sessions = manager.inner.session_routes.read().await;
        assert!(!sessions.contains_key("a"));
        assert!(sessions.contains_key("b"));
        assert!(sessions.contains_key("c"));
    }

    #[tokio::test]
    async fn persisted_pins_for_missing_profiles_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("route-pins.json");
        RoutePinStore::new(path.clone())
            .save(&HashMap::from([(
                "session-a".into(),
                SessionRoutePin {
                    profile_id: "missing".into(),
                    first_seen_ms: 1_000,
                    last_seen_ms: 1_000,
                },
            )]))
            .unwrap();

        let manager = RouteManager::from_config_inner(
            &routing_config(),
            Some(RoutePinStore::new(path)),
            Arc::new(ManualClock::new(2_000)),
        )
        .unwrap();
        assert_eq!(manager.status().await.unwrap().pinned_session_count, 0);
    }
}
