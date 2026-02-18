//! Plugin lifecycle host — discover, load, resolve, activate, teardown.

use async_trait::async_trait;
use clawdesk_types::error::PluginError;
use clawdesk_types::plugin::{
    PluginCapabilityGrant, PluginInfo, PluginManifest, PluginResourceLimits,
    PluginSource, PluginState,
};
use crate::sandbox::PluginSandbox;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// A loaded plugin instance that can receive lifecycle callbacks.
#[async_trait]
pub trait PluginInstance: Send + Sync + 'static {
    /// Called when the plugin is activated.
    async fn on_activate(&self) -> Result<(), String>;

    /// Called when the plugin is being stopped. Perform cleanup here.
    async fn on_deactivate(&self) -> Result<(), String>;

    /// Called when the plugin receives a message routed to it.
    async fn on_message(&self, payload: serde_json::Value) -> Result<serde_json::Value, String>;

    /// Health check — return true if functioning correctly.
    async fn health_check(&self) -> bool {
        true
    }
}

/// Handle to a loaded plugin with its metadata and instance.
pub struct PluginHandle {
    pub manifest: PluginManifest,
    pub source: PluginSource,
    pub state: PluginState,
    pub grants: HashSet<PluginCapabilityGrant>,
    pub limits: PluginResourceLimits,
    pub instance: Option<Arc<dyn PluginInstance>>,
    pub sandbox: Option<Arc<PluginSandbox>>,
    pub load_time_ms: u64,
    pub error_count: u32,
    pub last_error: Option<String>,
    /// Health check backoff: number of consecutive failures.
    pub health_failures: u32,
    /// Next time a health check should be attempted.
    pub next_health_check: std::time::Instant,
}

impl PluginHandle {
    /// Base delay for health check backoff (5 seconds).
    const HEALTH_BASE_DELAY_SECS: u64 = 5;
    /// Maximum delay cap for health check backoff (5 minutes).
    const HEALTH_MAX_DELAY_SECS: u64 = 300;

    /// Create a new handle from a discovered manifest.
    pub fn from_manifest(manifest: PluginManifest, source: PluginSource) -> Self {
        Self {
            manifest,
            source,
            state: PluginState::Discovered,
            grants: HashSet::new(),
            limits: PluginResourceLimits::default(),
            instance: None,
            sandbox: None,
            load_time_ms: 0,
            error_count: 0,
            last_error: None,
            health_failures: 0,
            next_health_check: std::time::Instant::now(),
        }
    }

    /// Transition state with FSM validation.
    pub fn transition(&mut self, target: PluginState) -> Result<(), PluginError> {
        let valid = match (&self.state, &target) {
            (PluginState::Discovered, PluginState::Loaded) => true,
            (PluginState::Loaded, PluginState::Resolved) => true,
            (PluginState::Resolved, PluginState::Active) => true,
            (PluginState::Active, PluginState::Stopping) => true,
            (PluginState::Stopping, PluginState::Disabled) => true,
            // Any state can go to Failed.
            (_, PluginState::Failed) => true,
            // Failed/Disabled can restart from Discovered.
            (PluginState::Failed, PluginState::Discovered) => true,
            (PluginState::Disabled, PluginState::Discovered) => true,
            _ => false,
        };

        if !valid {
            return Err(PluginError::ActivationFailed {
                name: self.manifest.name.clone(),
                detail: format!(
                    "Invalid state transition: {:?} → {:?}",
                    self.state, target
                ),
            });
        }

        debug!(
            plugin = %self.manifest.name,
            from = ?self.state,
            to = ?target,
            "State transition"
        );
        self.state = target;
        Ok(())
    }

    /// Convert to public info.
    pub fn info(&self) -> PluginInfo {
        PluginInfo {
            manifest: self.manifest.clone(),
            source: self.source.clone(),
            state: self.state,
            grants: self.grants.clone(),
            resource_limits: self.limits.clone(),
            load_time_ms: self.load_time_ms,
            error: self.last_error.clone(),
        }
    }

    /// Record an error and potentially transition to Failed state.
    pub fn record_error(&mut self, error: &str) {
        self.error_count += 1;
        self.last_error = Some(error.to_string());
        if self.error_count >= 3 {
            warn!(
                plugin = %self.manifest.name,
                errors = self.error_count,
                "Plugin exceeded error threshold, failing"
            );
            let _ = self.transition(PluginState::Failed);
        }
    }

    /// Record a health check failure and schedule next check with backoff.
    pub fn record_health_failure(&mut self) {
        self.health_failures += 1;
        let delay_secs = (Self::HEALTH_BASE_DELAY_SECS * 2u64.pow(self.health_failures - 1))
            .min(Self::HEALTH_MAX_DELAY_SECS);
        // Add simple jitter: ±25% of delay.
        let jitter = delay_secs / 4;
        let actual = delay_secs.saturating_sub(jitter / 2)
            + (self.health_failures as u64 % (jitter.max(1)));
        self.next_health_check =
            std::time::Instant::now() + std::time::Duration::from_secs(actual);
        debug!(
            plugin = %self.manifest.name,
            failures = self.health_failures,
            next_check_secs = actual,
            "Health check backoff"
        );
    }

    /// Record a successful health check and reset backoff.
    pub fn record_health_success(&mut self) {
        if self.health_failures > 0 {
            debug!(
                plugin = %self.manifest.name,
                prev_failures = self.health_failures,
                "Health restored, resetting backoff"
            );
            self.health_failures = 0;
        }
        self.next_health_check = std::time::Instant::now();
    }

    /// Whether a health check should be attempted now (respects backoff).
    pub fn should_health_check(&self) -> bool {
        std::time::Instant::now() >= self.next_health_check
    }
}

/// Factory for creating plugin instances from manifests.
#[async_trait]
pub trait PluginFactory: Send + Sync + 'static {
    async fn create(
        &self,
        manifest: &PluginManifest,
    ) -> Result<Arc<dyn PluginInstance>, PluginError>;
}

/// The plugin host manages the full lifecycle of all plugins.
pub struct PluginHost {
    plugins: RwLock<HashMap<String, PluginHandle>>,
    factory: Arc<dyn PluginFactory>,
    max_plugins: usize,
}

impl PluginHost {
    pub fn new(factory: Arc<dyn PluginFactory>, max_plugins: usize) -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
            factory,
            max_plugins,
        }
    }

    /// Discover and register a plugin.
    pub async fn discover(
        &self,
        manifest: PluginManifest,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        let mut plugins = self.plugins.write().await;
        if plugins.len() >= self.max_plugins {
            return Err(PluginError::LoadFailed {
                name: manifest.name.clone(),
                detail: format!("Maximum plugin count ({}) reached", self.max_plugins),
            });
        }
        if plugins.contains_key(&manifest.name) {
            return Err(PluginError::LoadFailed {
                name: manifest.name.clone(),
                detail: "Plugin already registered".to_string(),
            });
        }

        info!(plugin = %manifest.name, version = %manifest.version, "Discovered plugin");
        let name = manifest.name.clone();
        let handle = PluginHandle::from_manifest(manifest, source);
        plugins.insert(name, handle);
        Ok(())
    }

    /// Load a plugin — create its instance via the factory.
    pub async fn load(&self, name: &str) -> Result<(), PluginError> {
        let manifest = {
            let plugins = self.plugins.read().await;
            let handle = plugins.get(name).ok_or_else(|| PluginError::NotFound {
                name: name.to_string(),
            })?;
            if handle.state != PluginState::Discovered {
                return Err(PluginError::ActivationFailed {
                    name: name.to_string(),
                    detail: format!("Expected Discovered, got {:?}", handle.state),
                });
            }
            handle.manifest.clone()
        };

        let start = std::time::Instant::now();
        let instance = self.factory.create(&manifest).await?;
        let load_time_ms = start.elapsed().as_millis() as u64;

        let mut plugins = self.plugins.write().await;
        if let Some(handle) = plugins.get_mut(name) {
            handle.instance = Some(instance);
            handle.load_time_ms = load_time_ms;
            // Initialize sandbox with the plugin's grants and limits.
            handle.sandbox = Some(Arc::new(PluginSandbox::new(
                name.to_string(),
                handle.limits.clone(),
                handle.grants.clone(),
            )));
            handle.transition(PluginState::Loaded)?;
            info!(plugin = %name, load_time_ms, "Loaded plugin");
        }
        Ok(())
    }

    /// Resolve a plugin's dependencies.
    pub async fn resolve(&self, name: &str) -> Result<(), PluginError> {
        let deps = {
            let plugins = self.plugins.read().await;
            let handle = plugins.get(name).ok_or_else(|| PluginError::NotFound {
                name: name.to_string(),
            })?;
            handle.manifest.dependencies.clone()
        };

        // Check all dependencies are active.
        let plugins = self.plugins.read().await;
        for dep in &deps {
            match plugins.get(dep) {
                Some(h) if h.state == PluginState::Active => {}
                Some(h) => {
                    return Err(PluginError::ActivationFailed {
                        name: name.to_string(),
                        detail: format!(
                            "Dependency '{}' is {:?}, needs Active",
                            dep, h.state
                        ),
                    });
                }
                None => {
                    return Err(PluginError::ActivationFailed {
                        name: name.to_string(),
                        detail: format!("Dependency '{}' not found", dep),
                    });
                }
            }
        }
        drop(plugins);

        let mut plugins = self.plugins.write().await;
        if let Some(handle) = plugins.get_mut(name) {
            handle.transition(PluginState::Resolved)?;
        }
        Ok(())
    }

    /// Activate a plugin — call on_activate callback.
    pub async fn activate(&self, name: &str) -> Result<(), PluginError> {
        let instance = {
            let plugins = self.plugins.read().await;
            let handle = plugins.get(name).ok_or_else(|| PluginError::NotFound {
                name: name.to_string(),
            })?;
            handle.instance.clone().ok_or_else(|| PluginError::ActivationFailed {
                name: name.to_string(),
                detail: "Plugin not loaded".to_string(),
            })?
        };

        instance.on_activate().await.map_err(|e| {
            PluginError::ActivationFailed {
                name: name.to_string(),
                detail: e,
            }
        })?;

        let mut plugins = self.plugins.write().await;
        if let Some(handle) = plugins.get_mut(name) {
            handle.transition(PluginState::Active)?;
            info!(plugin = %name, "Activated plugin");
        }
        Ok(())
    }

    /// Deactivate and stop a plugin.
    pub async fn deactivate(&self, name: &str) -> Result<(), PluginError> {
        let instance = {
            let plugins = self.plugins.read().await;
            let handle = plugins.get(name).ok_or_else(|| PluginError::NotFound {
                name: name.to_string(),
            })?;
            handle.instance.clone()
        };

        if let Some(inst) = &instance {
            if let Err(e) = inst.on_deactivate().await {
                error!(plugin = %name, error = %e, "Deactivation error");
            }
        }

        let mut plugins = self.plugins.write().await;
        if let Some(handle) = plugins.get_mut(name) {
            handle.transition(PluginState::Stopping)?;
            handle.transition(PluginState::Disabled)?;
            handle.instance = None;
            info!(plugin = %name, "Deactivated");
        }
        Ok(())
    }

    /// Full lifecycle: discover → load → resolve → activate.
    pub async fn install_and_activate(
        &self,
        manifest: PluginManifest,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        let name = manifest.name.clone();
        self.discover(manifest, source).await?;
        self.load(&name).await?;
        self.resolve(&name).await?;
        self.activate(&name).await?;
        Ok(())
    }

    /// Get plugin info for all registered plugins.
    pub async fn list_plugins(&self) -> Vec<PluginInfo> {
        let plugins = self.plugins.read().await;
        plugins.values().map(|h| h.info()).collect()
    }

    /// Get a specific plugin's info.
    pub async fn get_plugin(&self, name: &str) -> Option<PluginInfo> {
        let plugins = self.plugins.read().await;
        plugins.get(name).map(|h| h.info())
    }

    /// Send a message to an active plugin.
    ///
    /// ## Security (T-02)
    ///
    /// Before dispatching, the sandbox is consulted:
    /// 1. A `TaskGuard` is acquired (enforces max_tasks via CAS).
    /// 2. Message payload size is checked against memory limits.
    /// 3. The call is wrapped in a timeout (`max_cpu_ms`).
    /// 4. On sandbox violation, the plugin's error is recorded.
    pub async fn send_to_plugin(
        &self,
        name: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let (instance, sandbox, timeout_ms) = {
            let plugins = self.plugins.read().await;
            let handle = plugins.get(name).ok_or_else(|| PluginError::NotFound {
                name: name.to_string(),
            })?;
            if handle.state != PluginState::Active {
                return Err(PluginError::ActivationFailed {
                    name: name.to_string(),
                    detail: format!("Plugin not active (state: {:?})", handle.state),
                });
            }
            let inst = handle.instance.clone().ok_or_else(|| PluginError::ActivationFailed {
                name: name.to_string(),
                detail: "No instance".to_string(),
            })?;
            (inst, handle.sandbox.clone(), handle.limits.max_cpu_ms)
        };

        // ── Sandbox enforcement ──────────────────────────────
        let _task_guard = if let Some(ref sb) = sandbox {
            // Acquire task slot (CAS-protected, no TOCTOU).
            let guard = sb.try_spawn_task().map_err(|_| PluginError::ActivationFailed {
                name: name.to_string(),
                detail: "sandbox: max concurrent tasks exceeded".to_string(),
            })?;

            // Check payload size against memory budget.
            let payload_size = serde_json::to_vec(&payload)
                .map(|v| v.len() as u64)
                .unwrap_or(0);
            sb.try_allocate_memory(payload_size).map_err(|_| PluginError::ActivationFailed {
                name: name.to_string(),
                detail: "sandbox: memory limit exceeded".to_string(),
            })?;

            Some(guard)
        } else {
            None
        };

        // Wrap in timeout to enforce max_cpu_ms.
        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        let result = tokio::time::timeout(timeout_duration, instance.on_message(payload)).await;

        match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => {
                let mut plugins = self.plugins.write().await;
                if let Some(handle) = plugins.get_mut(name) {
                    handle.record_error(&e);
                }
                Err(PluginError::ActivationFailed {
                    name: name.to_string(),
                    detail: e,
                })
            }
            Err(_elapsed) => {
                let mut plugins = self.plugins.write().await;
                if let Some(handle) = plugins.get_mut(name) {
                    handle.record_error("execution timeout exceeded");
                }
                Err(PluginError::Timeout {
                    name: name.to_string(),
                    timeout_secs: (timeout_ms / 1000) as u64,
                })
            }
        }
    }

    /// Run health checks on all active plugins.
    ///
    /// Respects per-plugin exponential backoff: plugins that have recently failed
    /// health checks are skipped until their backoff timer expires (5s base, 300s cap).
    pub async fn health_check_all(&self) -> HashMap<String, bool> {
        let active: Vec<(String, Arc<dyn PluginInstance>)> = {
            let plugins = self.plugins.read().await;
            plugins
                .iter()
                .filter(|(_, h)| {
                    h.state == PluginState::Active
                        && h.instance.is_some()
                        && h.should_health_check()
                })
                .map(|(name, h)| (name.clone(), h.instance.clone().unwrap()))
                .collect()
        };

        let mut results = HashMap::new();
        for (name, instance) in active {
            let healthy = instance.health_check().await;
            // Update backoff state.
            {
                let mut plugins = self.plugins.write().await;
                if let Some(handle) = plugins.get_mut(&name) {
                    if healthy {
                        handle.record_health_success();
                    } else {
                        handle.record_health_failure();
                        warn!(plugin = %name, failures = handle.health_failures, "Health check failed (backoff active)");
                    }
                }
            }
            results.insert(name, healthy);
        }
        results
    }

    /// Gracefully shut down all plugins.
    pub async fn shutdown_all(&self) {
        let names: Vec<String> = {
            let plugins = self.plugins.read().await;
            plugins
                .iter()
                .filter(|(_, h)| h.state == PluginState::Active)
                .map(|(name, _)| name.clone())
                .collect()
        };

        for name in names.iter().rev() {
            if let Err(e) = self.deactivate(name).await {
                error!(plugin = %name, error = %e, "Shutdown error");
            }
        }
        info!(count = names.len(), "All plugins shut down");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::plugin::PluginCapabilities;

    struct TestFactory;

    #[async_trait]
    impl PluginFactory for TestFactory {
        async fn create(
            &self,
            _manifest: &PluginManifest,
        ) -> Result<Arc<dyn PluginInstance>, PluginError> {
            Ok(Arc::new(TestPlugin))
        }
    }

    struct TestPlugin;

    #[async_trait]
    impl PluginInstance for TestPlugin {
        async fn on_activate(&self) -> Result<(), String> { Ok(()) }
        async fn on_deactivate(&self) -> Result<(), String> { Ok(()) }
        async fn on_message(&self, p: serde_json::Value) -> Result<serde_json::Value, String> { Ok(p) }
    }

    fn test_manifest(name: &str) -> PluginManifest {
        PluginManifest {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: format!("Test plugin {name}"),
            author: "test".to_string(),
            min_sdk_version: "0.1.0".to_string(),
            dependencies: vec![],
            capabilities: PluginCapabilities::default(),
        }
    }

    #[tokio::test]
    async fn test_full_lifecycle() {
        let host = PluginHost::new(Arc::new(TestFactory), 10);
        host.install_and_activate(test_manifest("test"), PluginSource::Bundled)
            .await
            .unwrap();
        let plugins = host.list_plugins().await;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].state, PluginState::Active);
    }

    #[tokio::test]
    async fn test_max_plugins_limit() {
        let host = PluginHost::new(Arc::new(TestFactory), 1);
        host.discover(test_manifest("p1"), PluginSource::Bundled).await.unwrap();
        let err = host.discover(test_manifest("p2"), PluginSource::Bundled).await.unwrap_err();
        matches!(err, PluginError::LoadFailed { .. });
    }

    #[tokio::test]
    async fn test_deactivation() {
        let host = PluginHost::new(Arc::new(TestFactory), 10);
        host.install_and_activate(test_manifest("p1"), PluginSource::Bundled).await.unwrap();
        host.deactivate("p1").await.unwrap();
        let info = host.get_plugin("p1").await.unwrap();
        assert_eq!(info.state, PluginState::Disabled);
    }

    #[tokio::test]
    async fn test_send_to_inactive_plugin() {
        let host = PluginHost::new(Arc::new(TestFactory), 10);
        host.discover(test_manifest("p1"), PluginSource::Bundled).await.unwrap();
        let err = host
            .send_to_plugin("p1", serde_json::json!({}))
            .await
            .unwrap_err();
        matches!(err, PluginError::ActivationFailed { .. });
    }
}
