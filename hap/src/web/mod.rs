//! Web UI and Prometheus metrics server.
//!
//! This module provides a simple web interface for monitoring the bridge
//! and a Prometheus metrics endpoint for external monitoring.

pub mod metrics;
pub mod qrcode_template;
pub mod state;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use minijinja::{Environment, context};
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::web::metrics::Metrics;
use crate::web::state::{BridgeState, DeviceType};

/// Application state shared with all route handlers.
#[derive(Clone)]
pub struct AppState {
    /// Bridge state.
    pub bridge_state: BridgeState,
    /// Prometheus metrics handle.
    pub metrics_handle: PrometheusHandle,
    /// Template environment.
    pub templates: Arc<RwLock<Environment<'static>>>,
}

/// Web server configuration.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Port to listen on.
    pub port: u16,
    /// Whether to enable the web UI.
    pub enabled: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            enabled: true,
        }
    }
}

/// Start the web server.
///
/// This function spawns the web server in the background and returns immediately.
/// The server will run until the application shuts down.
pub async fn start_web_server(
    config: WebConfig,
    bridge_state: BridgeState,
) -> Result<(), std::io::Error> {
    if !config.enabled {
        info!("Web UI is disabled");
        return Ok(());
    }

    // Initialize Prometheus metrics
    let metrics_handle = metrics::init_metrics();

    // Set up template environment
    let mut env = Environment::new();

    // Add templates
    env.add_template("base.html", include_str!("../../templates/base.html"))
        .expect("Failed to add base template");
    env.add_template("index.html", include_str!("../../templates/index.html"))
        .expect("Failed to add index template");
    env.add_template("devices.html", include_str!("../../templates/devices.html"))
        .expect("Failed to add devices template");

    let app_state = AppState {
        bridge_state,
        metrics_handle,
        templates: Arc::new(RwLock::new(env)),
    };

    // Build router
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/devices", get(devices_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/api/status", get(api_status_handler))
        .route("/qrcode.svg", get(qrcode_handler))
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Starting web server on http://{}", addr);

    let listener = TcpListener::bind(addr).await?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("Web server error: {}", e);
        }
    });

    Ok(())
}

/// Index page handler - shows bridge overview.
async fn index_handler(State(state): State<AppState>) -> Response {
    let summary = state.bridge_state.summary();

    // Update metrics
    Metrics::set_uptime(state.bridge_state.start_time());
    Metrics::set_connected(summary.connection_status == state::ConnectionStatus::Connected);
    Metrics::set_paired(summary.is_paired);

    // Update device count metrics
    for (device_type, count) in &summary.device_counts {
        Metrics::set_device_count(device_type.as_str(), *count);
    }

    let templates = state.templates.read();
    let template = match templates.get_template("index.html") {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to get index template: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response();
        }
    };

    let html = match template.render(context! {
        title => "Comelit HUB Bridge",
        uptime => summary.uptime_display(),
        uptime_seconds => summary.uptime_seconds,
        connection_status => summary.connection_status.as_str(),
        is_paired => summary.is_paired,
        pairing_pin => summary.pairing_pin,
        pairing_url => summary.pairing_url,
        device_count => summary.device_count,
        light_count => summary.device_counts.get(&DeviceType::Light).unwrap_or(&0),
        thermostat_count => summary.device_counts.get(&DeviceType::Thermostat).unwrap_or(&0),
        window_covering_count => summary.device_counts.get(&DeviceType::WindowCovering).unwrap_or(&0),
        door_count => summary.device_counts.get(&DeviceType::Door).unwrap_or(&0),
        doorbell_count => summary.device_counts.get(&DeviceType::Doorbell).unwrap_or(&0),
        last_ping_seconds_ago => summary.last_ping_seconds_ago,
        ping_count => summary.ping_count,
        ping_failures => summary.ping_failures,
        ping_success_rate => format!("{:.1}", summary.ping_success_rate()),
        update_count => summary.update_count,
        hub_host => summary.hub_host.as_deref().unwrap_or("unknown"),
        last_error => summary.last_error,
    }) {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render index template: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Render error").into_response();
        }
    };

    Html(html).into_response()
}

/// Devices page handler - shows all registered devices.
async fn devices_handler(State(state): State<AppState>) -> Response {
    let devices = state.bridge_state.devices();

    // Group devices by type
    let lights: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Light)
        .collect();
    let thermostats: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Thermostat)
        .collect();
    let window_coverings: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::WindowCovering)
        .collect();
    let doors: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Door)
        .collect();
    let doorbells: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Doorbell)
        .collect();

    let templates = state.templates.read();
    let template = match templates.get_template("devices.html") {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to get devices template: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response();
        }
    };

    // Convert devices to template-friendly format
    let to_device_list =
        |devices: Vec<&state::DeviceInfo>| -> Vec<std::collections::HashMap<&str, String>> {
            devices
                .into_iter()
                .map(|d| {
                    let mut map = std::collections::HashMap::new();
                    map.insert("id", d.id.clone());
                    map.insert("name", d.name.clone());
                    map.insert("status", d.status.clone());
                    map.insert(
                        "last_update",
                        d.last_update
                            .map(|t| format!("{}s ago", t.elapsed().as_secs()))
                            .unwrap_or_else(|| "never".to_string()),
                    );
                    map
                })
                .collect()
        };

    let html = match template.render(context! {
        title => "Devices - Comelit HUB Bridge",
        lights => to_device_list(lights),
        thermostats => to_device_list(thermostats),
        window_coverings => to_device_list(window_coverings),
        doors => to_device_list(doors),
        doorbells => to_device_list(doorbells),
        total_count => devices.len(),
    }) {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render devices template: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Render error").into_response();
        }
    };

    Html(html).into_response()
}

/// Health check endpoint.
async fn health_handler(State(state): State<AppState>) -> Response {
    let summary = state.bridge_state.summary();

    let is_healthy = summary.connection_status == state::ConnectionStatus::Connected
        && summary
            .last_ping_seconds_ago
            .map(|s| s < 120)
            .unwrap_or(false);

    if is_healthy {
        (StatusCode::OK, "OK").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "UNHEALTHY").into_response()
    }
}

/// Prometheus metrics endpoint.
async fn metrics_handler(State(state): State<AppState>) -> Response {
    // Update uptime metric before rendering
    Metrics::set_uptime(state.bridge_state.start_time());

    let metrics = state.metrics_handle.render();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        metrics,
    )
        .into_response()
}

/// QR code SVG endpoint - returns an SVG image with the HomeKit pairing QR code.
async fn qrcode_handler(State(state): State<AppState>) -> Response {
    let summary = state.bridge_state.summary();

    if summary.pairing_url.is_empty() || summary.pairing_pin.is_empty() {
        return (StatusCode::NOT_FOUND, "Pairing info not available").into_response();
    }

    match qrcode_template::generate_qr_svg(&summary.pairing_url, &summary.pairing_pin) {
        Ok(svg) => (StatusCode::OK, [("content-type", "image/svg+xml")], svg).into_response(),
        Err(e) => {
            error!("Failed to generate QR code: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to generate QR code",
            )
                .into_response()
        }
    }
}

/// API status endpoint - returns JSON status.
async fn api_status_handler(State(state): State<AppState>) -> Response {
    let summary = state.bridge_state.summary();

    let json = serde_json::json!({
        "status": "ok",
        "uptime_seconds": summary.uptime_seconds,
        "connection_status": summary.connection_status.as_str(),
        "is_paired": summary.is_paired,
        "device_count": summary.device_count,
        "devices": {
            "lights": summary.device_counts.get(&DeviceType::Light).unwrap_or(&0),
            "thermostats": summary.device_counts.get(&DeviceType::Thermostat).unwrap_or(&0),
            "window_coverings": summary.device_counts.get(&DeviceType::WindowCovering).unwrap_or(&0),
            "doors": summary.device_counts.get(&DeviceType::Door).unwrap_or(&0),
            "doorbells": summary.device_counts.get(&DeviceType::Doorbell).unwrap_or(&0),
        },
        "ping": {
            "last_seconds_ago": summary.last_ping_seconds_ago,
            "total": summary.ping_count,
            "failures": summary.ping_failures,
            "success_rate": summary.ping_success_rate(),
        },
        "updates_received": summary.update_count,
        "hub_host": summary.hub_host,
        "last_error": summary.last_error,
    });

    (
        StatusCode::OK,
        [("content-type", "application/json")],
        json.to_string(),
    )
        .into_response()
}
