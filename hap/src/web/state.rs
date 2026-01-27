//! Shared bridge state for the web UI and metrics.
//!
//! This module defines the shared state that is accessible from both
//! the bridge runtime and the web server.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Information about a device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Unique device ID.
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// Device type (light, thermostat, window_covering, door, doorbell).
    pub device_type: DeviceType,
    /// Current status (device-specific).
    pub status: String,
    /// Last update time.
    pub last_update: Option<Instant>,
}

/// Type of device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceType {
    Light,
    Thermostat,
    WindowCovering,
    Door,
    Doorbell,
}

impl DeviceType {
    /// Returns the device type as a string for display.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceType::Light => "light",
            DeviceType::Thermostat => "thermostat",
            DeviceType::WindowCovering => "window_covering",
            DeviceType::Door => "door",
            DeviceType::Doorbell => "doorbell",
        }
    }

    /// Returns a human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            DeviceType::Light => "Light",
            DeviceType::Thermostat => "Thermostat",
            DeviceType::WindowCovering => "Window Covering",
            DeviceType::Door => "Door",
            DeviceType::Doorbell => "Doorbell",
        }
    }
}

/// Connection status of the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Not connected to the Comelit hub.
    Disconnected,
    /// Connecting to the Comelit hub.
    Connecting,
    /// Connected and authenticated.
    Connected,
    /// Connection error.
    Error,
}

impl ConnectionStatus {
    /// Returns the status as a string for display.
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionStatus::Disconnected => "disconnected",
            ConnectionStatus::Connecting => "connecting",
            ConnectionStatus::Connected => "connected",
            ConnectionStatus::Error => "error",
        }
    }
}

/// Internal mutable state.
#[derive(Debug)]
struct BridgeStateInner {
    /// Bridge start time.
    start_time: Instant,
    /// Current connection status.
    connection_status: ConnectionStatus,
    /// Whether the HomeKit bridge is paired.
    is_paired: bool,
    /// HomeKit pairing PIN.
    pairing_pin: String,
    /// HomeKit pairing URL (for QR code).
    pairing_url: String,
    /// Registered devices.
    devices: HashMap<String, DeviceInfo>,
    /// Last successful ping time.
    last_ping: Option<Instant>,
    /// Total ping count.
    ping_count: u64,
    /// Failed ping count.
    ping_failures: u64,
    /// Total device updates received.
    update_count: u64,
    /// Comelit hub host.
    hub_host: Option<String>,
    /// Error message if any.
    last_error: Option<String>,
}

/// Shared bridge state.
///
/// This is thread-safe and can be shared between the bridge and web server.
#[derive(Debug, Clone)]
pub struct BridgeState {
    inner: Arc<RwLock<BridgeStateInner>>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeState {
    /// Create a new bridge state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(BridgeStateInner {
                start_time: Instant::now(),
                connection_status: ConnectionStatus::Disconnected,
                is_paired: false,
                pairing_pin: String::new(),
                pairing_url: String::new(),
                devices: HashMap::new(),
                last_ping: None,
                ping_count: 0,
                ping_failures: 0,
                update_count: 0,
                hub_host: None,
                last_error: None,
            })),
        }
    }

    /// Get the bridge start time.
    pub fn start_time(&self) -> Instant {
        self.inner.read().start_time
    }

    /// Get uptime in seconds.
    pub fn uptime_seconds(&self) -> u64 {
        self.inner.read().start_time.elapsed().as_secs()
    }

    /// Get the current connection status.
    pub fn connection_status(&self) -> ConnectionStatus {
        self.inner.read().connection_status
    }

    /// Set the connection status.
    pub fn set_connection_status(&self, status: ConnectionStatus) {
        self.inner.write().connection_status = status;
    }

    /// Check if the bridge is paired.
    pub fn is_paired(&self) -> bool {
        self.inner.read().is_paired
    }

    /// Set the pairing status.
    pub fn set_paired(&self, paired: bool) {
        self.inner.write().is_paired = paired;
    }

    /// Get the pairing PIN.
    pub fn pairing_pin(&self) -> String {
        self.inner.read().pairing_pin.clone()
    }

    /// Set the pairing PIN.
    pub fn set_pairing_pin(&self, pin: String) {
        self.inner.write().pairing_pin = pin;
    }

    /// Get the pairing URL.
    pub fn pairing_url(&self) -> String {
        self.inner.read().pairing_url.clone()
    }

    /// Set the pairing URL.
    pub fn set_pairing_url(&self, url: String) {
        self.inner.write().pairing_url = url;
    }

    /// Get the Comelit hub host.
    pub fn hub_host(&self) -> Option<String> {
        self.inner.read().hub_host.clone()
    }

    /// Set the Comelit hub host.
    pub fn set_hub_host(&self, host: String) {
        self.inner.write().hub_host = Some(host);
    }

    /// Register a device.
    pub fn register_device(&self, device: DeviceInfo) {
        self.inner.write().devices.insert(device.id.clone(), device);
    }

    /// Update a device's status.
    pub fn update_device_status(&self, id: &str, status: String) {
        let mut inner = self.inner.write();
        if let Some(device) = inner.devices.get_mut(id) {
            device.status = status;
            device.last_update = Some(Instant::now());
        }
        inner.update_count += 1;
    }

    /// Get all devices.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.inner.read().devices.values().cloned().collect()
    }

    /// Get devices by type.
    pub fn devices_by_type(&self, device_type: DeviceType) -> Vec<DeviceInfo> {
        self.inner
            .read()
            .devices
            .values()
            .filter(|d| d.device_type == device_type)
            .cloned()
            .collect()
    }

    /// Get device counts by type.
    pub fn device_counts(&self) -> HashMap<DeviceType, usize> {
        let inner = self.inner.read();
        let mut counts = HashMap::new();
        for device in inner.devices.values() {
            *counts.entry(device.device_type).or_insert(0) += 1;
        }
        counts
    }

    /// Get total device count.
    pub fn device_count(&self) -> usize {
        self.inner.read().devices.len()
    }

    /// Record a ping result.
    pub fn record_ping(&self, success: bool) {
        let mut inner = self.inner.write();
        inner.ping_count += 1;
        if success {
            inner.last_ping = Some(Instant::now());
        } else {
            inner.ping_failures += 1;
        }
    }

    /// Get the last successful ping time.
    pub fn last_ping(&self) -> Option<Instant> {
        self.inner.read().last_ping
    }

    /// Get seconds since last successful ping.
    pub fn seconds_since_last_ping(&self) -> Option<u64> {
        self.inner.read().last_ping.map(|t| t.elapsed().as_secs())
    }

    /// Get total ping count.
    pub fn ping_count(&self) -> u64 {
        self.inner.read().ping_count
    }

    /// Get ping failure count.
    pub fn ping_failures(&self) -> u64 {
        self.inner.read().ping_failures
    }

    /// Get total update count.
    pub fn update_count(&self) -> u64 {
        self.inner.read().update_count
    }

    /// Set an error message.
    pub fn set_error(&self, error: Option<String>) {
        self.inner.write().last_error = error;
    }

    /// Get the last error message.
    pub fn last_error(&self) -> Option<String> {
        self.inner.read().last_error.clone()
    }

    /// Get a summary of the bridge state for the web UI.
    pub fn summary(&self) -> BridgeStateSummary {
        let inner = self.inner.read();
        BridgeStateSummary {
            uptime_seconds: inner.start_time.elapsed().as_secs(),
            connection_status: inner.connection_status,
            is_paired: inner.is_paired,
            pairing_pin: inner.pairing_pin.clone(),
            pairing_url: inner.pairing_url.clone(),
            device_count: inner.devices.len(),
            device_counts: {
                let mut counts = HashMap::new();
                for device in inner.devices.values() {
                    *counts.entry(device.device_type).or_insert(0) += 1;
                }
                counts
            },
            last_ping_seconds_ago: inner.last_ping.map(|t| t.elapsed().as_secs()),
            ping_count: inner.ping_count,
            ping_failures: inner.ping_failures,
            update_count: inner.update_count,
            hub_host: inner.hub_host.clone(),
            last_error: inner.last_error.clone(),
        }
    }
}

/// Summary of the bridge state for the web UI.
#[derive(Debug, Clone)]
pub struct BridgeStateSummary {
    /// Uptime in seconds.
    pub uptime_seconds: u64,
    /// Current connection status.
    pub connection_status: ConnectionStatus,
    /// Whether the bridge is paired.
    pub is_paired: bool,
    /// HomeKit pairing PIN.
    pub pairing_pin: String,
    /// HomeKit pairing URL.
    pub pairing_url: String,
    /// Total number of devices.
    pub device_count: usize,
    /// Device counts by type.
    pub device_counts: HashMap<DeviceType, usize>,
    /// Seconds since last successful ping.
    pub last_ping_seconds_ago: Option<u64>,
    /// Total ping count.
    pub ping_count: u64,
    /// Ping failure count.
    pub ping_failures: u64,
    /// Total update count.
    pub update_count: u64,
    /// Comelit hub host.
    pub hub_host: Option<String>,
    /// Last error message.
    pub last_error: Option<String>,
}

impl BridgeStateSummary {
    /// Format uptime as a human-readable string.
    pub fn uptime_display(&self) -> String {
        let secs = self.uptime_seconds;
        let days = secs / 86400;
        let hours = (secs % 86400) / 3600;
        let mins = (secs % 3600) / 60;
        let secs = secs % 60;

        if days > 0 {
            format!("{}d {}h {}m {}s", days, hours, mins, secs)
        } else if hours > 0 {
            format!("{}h {}m {}s", hours, mins, secs)
        } else if mins > 0 {
            format!("{}m {}s", mins, secs)
        } else {
            format!("{}s", secs)
        }
    }

    /// Get ping success rate as a percentage.
    pub fn ping_success_rate(&self) -> f64 {
        if self.ping_count == 0 {
            100.0
        } else {
            let successes = self.ping_count - self.ping_failures;
            (successes as f64 / self.ping_count as f64) * 100.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_state_new() {
        let state = BridgeState::new();
        assert_eq!(state.connection_status(), ConnectionStatus::Disconnected);
        assert!(!state.is_paired());
        assert_eq!(state.device_count(), 0);
    }

    #[test]
    fn test_register_device() {
        let state = BridgeState::new();
        state.register_device(DeviceInfo {
            id: "light1".to_string(),
            name: "Living Room Light".to_string(),
            device_type: DeviceType::Light,
            status: "on".to_string(),
            last_update: None,
        });
        assert_eq!(state.device_count(), 1);
        assert_eq!(state.devices_by_type(DeviceType::Light).len(), 1);
    }

    #[test]
    fn test_update_device_status() {
        let state = BridgeState::new();
        state.register_device(DeviceInfo {
            id: "light1".to_string(),
            name: "Living Room Light".to_string(),
            device_type: DeviceType::Light,
            status: "off".to_string(),
            last_update: None,
        });
        state.update_device_status("light1", "on".to_string());
        let devices = state.devices();
        assert_eq!(devices[0].status, "on");
        assert!(devices[0].last_update.is_some());
    }

    #[test]
    fn test_ping_recording() {
        let state = BridgeState::new();
        state.record_ping(true);
        state.record_ping(true);
        state.record_ping(false);
        assert_eq!(state.ping_count(), 3);
        assert_eq!(state.ping_failures(), 1);
        assert!(state.last_ping().is_some());
    }

    #[test]
    fn test_uptime_display() {
        let summary = BridgeStateSummary {
            uptime_seconds: 90061, // 1 day, 1 hour, 1 minute, 1 second
            connection_status: ConnectionStatus::Connected,
            is_paired: false,
            pairing_pin: String::new(),
            pairing_url: String::new(),
            device_count: 0,
            device_counts: HashMap::new(),
            last_ping_seconds_ago: None,
            ping_count: 0,
            ping_failures: 0,
            update_count: 0,
            hub_host: None,
            last_error: None,
        };
        assert_eq!(summary.uptime_display(), "1d 1h 1m 1s");
    }
}
