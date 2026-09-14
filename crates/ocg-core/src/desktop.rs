//! Process-level desktop capabilities: auto-start, Dock visibility, and the
//! signed-update status machine.
//!
//! [`DesktopCapabilities`] is a concrete process/Host facade, not a plugin
//! trait. It owns the OnceLock hooks and the update status machine. Hosts
//! register auto-start before listener bind and Dock/updater during setup;
//! CLI and Docker leave the hooks unset.
//!
//! This module does not import the process host, sqlite, or gateway runtime
//! and stays a DAG leaf outside the remaining host SCC.

use parking_lot::Mutex;
use serde::Serialize;
use std::fmt;
use std::sync::{Arc, OnceLock};

pub type AutoStartSync = fn(bool) -> crate::Result<()>;

pub type DockVisibilitySync = Arc<dyn Fn(bool) -> crate::Result<()> + Send + Sync + 'static>;

/// Shared with [`crate::state::HostSettingsError`] so headless hooks and
/// settings writes fail closed on the same public strings.
pub const AUTO_START_UNAVAILABLE: &str = "auto-start is unavailable in this runtime";
pub const DOCK_VISIBILITY_UNAVAILABLE: &str = "Dock visibility is unavailable in this runtime";

pub type DesktopUpdateStarter = Arc<dyn Fn(String) -> crate::Result<()> + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopUpdatePhase {
    Idle,
    Checking,
    Downloading,
    Installing,
    Failed,
}

impl DesktopUpdatePhase {
    fn is_busy(self) -> bool {
        matches!(self, Self::Checking | Self::Downloading | Self::Installing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesktopUpdateStatus {
    pub phase: DesktopUpdatePhase,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    pub current_version: String,
    pub install_supported: bool,
}

impl DesktopUpdateStatus {
    fn new() -> Self {
        Self {
            phase: DesktopUpdatePhase::Idle,
            downloaded: 0,
            total: None,
            error: None,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            install_supported: false,
        }
    }
}

#[derive(Debug)]
pub enum DesktopUpdateStartError {
    Unsupported,
    Busy,
    Starter(anyhow::Error),
}

impl fmt::Display for DesktopUpdateStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("desktop update installation is unavailable"),
            Self::Busy => f.write_str("a desktop update is already in progress"),
            Self::Starter(error) => write!(f, "failed to start desktop update: {error}"),
        }
    }
}

impl std::error::Error for DesktopUpdateStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Starter(error) => Some(error.as_ref()),
            Self::Unsupported | Self::Busy => None,
        }
    }
}

/// Process/Host facade for desktop hooks and the update status machine.
///
/// The update-status mutex is never held while acquiring another process
/// sync lock.
pub struct DesktopCapabilities {
    auto_start_sync: OnceLock<AutoStartSync>,
    dock_visibility_sync: OnceLock<DockVisibilitySync>,
    desktop_update_starter: OnceLock<DesktopUpdateStarter>,
    desktop_update_status: Mutex<DesktopUpdateStatus>,
}

impl DesktopCapabilities {
    pub fn new() -> Self {
        Self {
            auto_start_sync: OnceLock::new(),
            dock_visibility_sync: OnceLock::new(),
            desktop_update_starter: OnceLock::new(),
            desktop_update_status: Mutex::new(DesktopUpdateStatus::new()),
        }
    }

    pub fn set_auto_start_sync(&self, sync: AutoStartSync) {
        assert!(
            self.auto_start_sync.set(sync).is_ok(),
            "auto-start sync is already configured"
        );
    }

    pub fn auto_start_supported(&self) -> bool {
        self.auto_start_sync.get().is_some()
    }

    pub fn sync_auto_start(&self, enabled: bool) -> crate::Result<()> {
        let sync = self
            .auto_start_sync
            .get()
            .ok_or_else(|| anyhow::anyhow!(AUTO_START_UNAVAILABLE))?;
        sync(enabled)
    }

    pub fn set_dock_visibility_sync(&self, sync: DockVisibilitySync) {
        assert!(
            self.dock_visibility_sync.set(sync).is_ok(),
            "dock visibility sync is already configured"
        );
    }

    pub fn dock_visibility_supported(&self) -> bool {
        self.dock_visibility_sync.get().is_some()
    }

    pub fn sync_dock_visibility(&self, visible: bool) -> crate::Result<()> {
        let sync = self
            .dock_visibility_sync
            .get()
            .ok_or_else(|| anyhow::anyhow!(DOCK_VISIBILITY_UNAVAILABLE))?;
        sync(visible)
    }

    pub fn set_desktop_update_starter(&self, starter: DesktopUpdateStarter) {
        assert!(
            self.desktop_update_starter.set(starter).is_ok(),
            "desktop update starter is already configured"
        );
        self.desktop_update_status.lock().install_supported = true;
    }

    pub fn desktop_update_supported(&self) -> bool {
        self.desktop_update_starter.get().is_some()
    }

    pub fn desktop_update_status(&self) -> DesktopUpdateStatus {
        self.desktop_update_status.lock().clone()
    }

    pub fn start_desktop_update(
        &self,
        expected_version: String,
    ) -> Result<(), DesktopUpdateStartError> {
        let starter = self
            .desktop_update_starter
            .get()
            .cloned()
            .ok_or(DesktopUpdateStartError::Unsupported)?;
        {
            let mut status = self.desktop_update_status.lock();
            if status.phase.is_busy() {
                return Err(DesktopUpdateStartError::Busy);
            }
            status.phase = DesktopUpdatePhase::Checking;
            status.downloaded = 0;
            status.total = None;
            status.error = None;
            status.install_supported = true;
        }

        if let Err(error) = starter(expected_version) {
            self.set_desktop_update_failed(error.to_string());
            return Err(DesktopUpdateStartError::Starter(error));
        }
        Ok(())
    }

    pub fn set_desktop_update_progress(&self, downloaded: u64, total: Option<u64>) -> bool {
        let mut status = self.desktop_update_status.lock();
        if !matches!(
            status.phase,
            DesktopUpdatePhase::Checking | DesktopUpdatePhase::Downloading
        ) {
            return false;
        }
        status.phase = DesktopUpdatePhase::Downloading;
        status.downloaded = downloaded;
        status.total = total;
        status.error = None;
        true
    }

    pub fn set_desktop_update_installing(&self) -> bool {
        let mut status = self.desktop_update_status.lock();
        if !matches!(
            status.phase,
            DesktopUpdatePhase::Checking | DesktopUpdatePhase::Downloading
        ) {
            return false;
        }
        status.phase = DesktopUpdatePhase::Installing;
        status.error = None;
        true
    }

    pub fn set_desktop_update_failed(&self, error: impl Into<String>) {
        let mut status = self.desktop_update_status.lock();
        status.phase = DesktopUpdatePhase::Failed;
        status.error = Some(error.into());
    }

    pub fn set_desktop_update_idle(&self) {
        let mut status = self.desktop_update_status.lock();
        status.phase = DesktopUpdatePhase::Idle;
        status.downloaded = 0;
        status.total = None;
        status.error = None;
    }
}

impl Default for DesktopCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopCapabilities, DesktopUpdatePhase, DesktopUpdateStartError};
    use std::sync::{Arc, Barrier, Mutex as StdMutex};

    fn ok_auto_start(_enabled: bool) -> crate::Result<()> {
        Ok(())
    }

    fn fail_auto_start(_enabled: bool) -> crate::Result<()> {
        anyhow::bail!("auto-start sync failed")
    }

    #[test]
    fn auto_start_is_unsupported_until_the_host_registers_a_hook() {
        let desktop = DesktopCapabilities::new();
        assert!(!desktop.auto_start_supported());
        desktop
            .sync_auto_start(true)
            .expect_err("headless runtimes leave auto-start unset");

        desktop.set_auto_start_sync(ok_auto_start);
        assert!(desktop.auto_start_supported());
        desktop
            .sync_auto_start(true)
            .expect("registered auto-start hook should run");

        let failing = DesktopCapabilities::new();
        failing.set_auto_start_sync(fail_auto_start);
        assert!(failing.sync_auto_start(false).is_err());
    }

    #[test]
    fn dock_visibility_is_unsupported_until_the_host_registers_a_hook() {
        let desktop = DesktopCapabilities::new();
        assert!(!desktop.dock_visibility_supported());
        desktop
            .sync_dock_visibility(false)
            .expect_err("headless runtimes leave Dock visibility unset");

        let applied = Arc::new(StdMutex::new(Vec::new()));
        let captured = applied.clone();
        desktop.set_dock_visibility_sync(Arc::new(move |visible| {
            captured.lock().expect("capture lock").push(visible);
            Ok(())
        }));
        assert!(desktop.dock_visibility_supported());
        desktop
            .sync_dock_visibility(false)
            .expect("registered Dock hook should run");
        assert_eq!(*applied.lock().expect("capture lock"), [false]);
    }

    #[test]
    fn desktop_update_state_machine_is_serializable_atomic_and_retriable() {
        let desktop = Arc::new(DesktopCapabilities::new());
        assert_eq!(
            serde_json::to_value(desktop.desktop_update_status()).expect("status should serialize"),
            serde_json::json!({
                "phase": "idle",
                "downloaded": 0,
                "total": null,
                "error": null,
                "current_version": env!("CARGO_PKG_VERSION"),
                "install_supported": false })
        );
        assert!(!desktop.desktop_update_supported());
        assert!(matches!(
            desktop.start_desktop_update("9.9.9".to_string()),
            Err(DesktopUpdateStartError::Unsupported)
        ));
        assert!(!desktop.set_desktop_update_progress(1, Some(2)));
        assert!(!desktop.set_desktop_update_installing());

        let started_versions = Arc::new(StdMutex::new(Vec::new()));
        let captured_versions = started_versions.clone();
        desktop.set_desktop_update_starter(Arc::new(move |expected_version| {
            captured_versions
                .lock()
                .expect("captured versions lock should work")
                .push(expected_version);
            Ok(())
        }));
        assert!(desktop.desktop_update_supported());
        assert!(desktop.desktop_update_status().install_supported);

        let barrier = Arc::new(Barrier::new(3));
        let threads = [desktop.clone(), desktop.clone()].map(|desktop| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                desktop.start_desktop_update("9.9.9".to_string())
            })
        });
        barrier.wait();
        let results = threads.map(|thread| thread.join().expect("start thread should not panic"));
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(DesktopUpdateStartError::Busy)))
                .count(),
            1
        );
        assert_eq!(
            started_versions
                .lock()
                .expect("started versions lock should work")
                .as_slice(),
            ["9.9.9"]
        );
        assert_eq!(
            desktop.desktop_update_status().phase,
            DesktopUpdatePhase::Checking
        );

        assert!(desktop.set_desktop_update_progress(25, Some(100)));
        let downloading = desktop.desktop_update_status();
        assert_eq!(downloading.phase, DesktopUpdatePhase::Downloading);
        assert_eq!(downloading.downloaded, 25);
        assert_eq!(downloading.total, Some(100));
        assert!(desktop.set_desktop_update_installing());
        assert!(!desktop.set_desktop_update_progress(50, Some(100)));
        desktop.set_desktop_update_failed("install failed");
        let failed = desktop.desktop_update_status();
        assert_eq!(failed.phase, DesktopUpdatePhase::Failed);
        assert_eq!(failed.error.as_deref(), Some("install failed"));

        desktop
            .start_desktop_update("10.0.0".to_string())
            .expect("a failed update should be retriable");
        let retrying = desktop.desktop_update_status();
        assert_eq!(retrying.phase, DesktopUpdatePhase::Checking);
        assert_eq!(retrying.downloaded, 0);
        assert_eq!(retrying.total, None);
        assert_eq!(retrying.error, None);
        assert_eq!(
            started_versions
                .lock()
                .expect("started versions lock should work")
                .as_slice(),
            ["9.9.9", "10.0.0"]
        );

        desktop.set_desktop_update_idle();
        assert_eq!(
            desktop.desktop_update_status().phase,
            DesktopUpdatePhase::Idle
        );
    }

    #[test]
    fn desktop_update_starter_failure_is_reported_in_status() {
        let desktop = DesktopCapabilities::new();
        desktop.set_desktop_update_starter(Arc::new(|_| anyhow::bail!("starter failed")));

        assert!(matches!(
            desktop.start_desktop_update("9.9.9".to_string()),
            Err(DesktopUpdateStartError::Starter(_))
        ));
        let status = desktop.desktop_update_status();
        assert_eq!(status.phase, DesktopUpdatePhase::Failed);
        assert_eq!(status.error.as_deref(), Some("starter failed"));
    }
}
