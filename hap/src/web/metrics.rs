//! Prometheus metrics definitions and registration.
//!
//! This module defines all the metrics that are exposed via the `/metrics` endpoint.

#![allow(dead_code)]

use metrics::{counter, describe_counter, describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::time::Instant;

/// Initialize the Prometheus metrics exporter and register all metric descriptions.
///
/// Returns a handle that can be used to render the metrics.
pub fn init_metrics() -> PrometheusHandle {
    let builder = PrometheusBuilder::new();
    let handle = builder
        .install_recorder()
        .expect("Failed to install Prometheus recorder");

    // Register metric descriptions
    register_metric_descriptions();

    handle
}

/// Register descriptions for all metrics.
fn register_metric_descriptions() {
    // Bridge metrics
    describe_gauge!(
        "comelit_bridge_info",
        "Information about the Comelit bridge (always 1, labels contain version info)"
    );
    describe_gauge!(
        "comelit_bridge_uptime_seconds",
        "Time in seconds since the bridge started"
    );
    describe_gauge!(
        "comelit_bridge_paired",
        "Whether the HomeKit bridge is paired (1) or not (0)"
    );

    // Connection metrics
    describe_gauge!(
        "comelit_connection_status",
        "MQTT connection status (1 = connected, 0 = disconnected)"
    );
    describe_counter!(
        "comelit_connection_reconnects_total",
        "Total number of MQTT reconnection attempts"
    );

    // Device metrics
    describe_gauge!("comelit_devices_total", "Total number of devices by type");

    // Update metrics
    describe_counter!(
        "comelit_device_updates_total",
        "Total number of device status updates received"
    );
    describe_counter!(
        "comelit_device_update_errors_total",
        "Total number of device update errors"
    );

    // Ping metrics
    describe_counter!("comelit_ping_total", "Total number of ping attempts");
    describe_counter!(
        "comelit_ping_success_total",
        "Total number of successful pings"
    );
    describe_counter!("comelit_ping_failure_total", "Total number of failed pings");
    describe_gauge!(
        "comelit_ping_last_success_timestamp",
        "Unix timestamp of the last successful ping"
    );

    // HAP server metrics
    describe_counter!(
        "comelit_hap_requests_total",
        "Total number of HomeKit requests received"
    );
}

/// Metrics helper functions for easy recording.
pub struct Metrics;

impl Metrics {
    /// Record bridge uptime based on start time.
    pub fn set_uptime(start_time: Instant) {
        let uptime = start_time.elapsed().as_secs_f64();
        gauge!("comelit_bridge_uptime_seconds").set(uptime);
    }

    /// Set bridge info metric with version labels.
    pub fn set_bridge_info(version: &str) {
        gauge!("comelit_bridge_info", "version" => version.to_string()).set(1.0);
    }

    /// Set whether the bridge is paired.
    pub fn set_paired(paired: bool) {
        gauge!("comelit_bridge_paired").set(if paired { 1.0 } else { 0.0 });
    }

    /// Set connection status.
    pub fn set_connected(connected: bool) {
        gauge!("comelit_connection_status").set(if connected { 1.0 } else { 0.0 });
    }

    /// Increment reconnection counter.
    pub fn inc_reconnects() {
        counter!("comelit_connection_reconnects_total").increment(1);
    }

    /// Set device count for a specific type.
    pub fn set_device_count(device_type: &str, count: usize) {
        gauge!("comelit_devices_total", "type" => device_type.to_string()).set(count as f64);
    }

    /// Increment device update counter.
    pub fn inc_device_updates(device_type: &str) {
        counter!("comelit_device_updates_total", "type" => device_type.to_string()).increment(1);
    }

    /// Increment device update error counter.
    pub fn inc_device_update_errors(device_type: &str) {
        counter!("comelit_device_update_errors_total", "type" => device_type.to_string())
            .increment(1);
    }

    /// Record a ping attempt.
    pub fn record_ping(success: bool) {
        counter!("comelit_ping_total").increment(1);
        if success {
            counter!("comelit_ping_success_total").increment(1);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            gauge!("comelit_ping_last_success_timestamp").set(now);
        } else {
            counter!("comelit_ping_failure_total").increment(1);
        }
    }

    /// Increment HAP request counter.
    pub fn inc_hap_requests() {
        counter!("comelit_hap_requests_total").increment(1);
    }
}
