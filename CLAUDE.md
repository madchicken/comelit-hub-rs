# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust-based HomeKit Accessory Protocol (HAP) bridge for the Comelit Hub 20003150 home automation system. It enables control of Comelit devices (lights, thermostats, window coverings, doors, doorbells) through Apple HomeKit.

## Workspace Structure

This is a Cargo workspace with four main crates:

- **`client/`** (`comelit-client-rs`): Core MQTT-based client library for communicating with Comelit Hub
- **`hap/`** (`comelit-hub-hap`): Main HomeKit bridge application (the primary binary)
- **`viper-client/`**: Video streaming client implementing proprietary doorbell protocol (TCP/UDP on port 64100)
- **`tui/`** (`comelit-hub-tui`): Terminal UI for monitoring and controlling devices

## Build Commands

```bash
# Build entire workspace
cargo build

# Build in release mode (optimized)
cargo build --release

# Build specific crate
cargo build --manifest-path=hap/Cargo.toml --release
cargo build --manifest-path=client/Cargo.toml
cargo build --manifest-path=viper-client/Cargo.toml
cargo build --manifest-path=tui/Cargo.toml

# Clean build artifacts
cargo clean
```

## Running Applications

```bash
# Run HAP bridge (main application)
cargo run --manifest-path=hap/Cargo.toml -- --user admin --password admin

# Run with logging to file
cargo run --manifest-path=hap/Cargo.toml -- \
  --log-dir ./logs --log-rotation daily --max-log-files 7

# Run CLI client
cargo run --manifest-path=client/Cargo.toml --bin comelit-hub-cli -- \
  --user admin --password admin --host 192.168.1.100

# Run viper video client
cargo run --manifest-path=viper-client/Cargo.toml --bin viper-client

# Run TUI
cargo run --manifest-path=tui/Cargo.toml
```

## Testing

```bash
# Run all tests in workspace
cargo test

# Run tests for specific crate
cargo test --manifest-path=client/Cargo.toml
cargo test --manifest-path=hap/Cargo.toml

# Run specific test
cargo test --manifest-path=client/Cargo.toml test_name

# Run tests with output
cargo test -- --nocapture
```

## Architecture Overview

### MQTT Communication Flow

The `client` crate implements the core protocol:

1. **Network discovery**: Uses UDP broadcast on port 5002 to find Comelit Hub devices
2. **MQTT connection**: Connects to hub's MQTT broker (port 1883) using hardcoded credentials
3. **Authentication**: Three-stage login: Announce → Login → Session establishment
4. **Topics**: Dynamic topic generation based on MAC address: `HSrv/{mac}/rx/{client_id}` and `HSrv/{mac}/tx/{client_id}`
5. **Request/Response**: `RequestManager` handles async request-response matching via oneshot channels
6. **Device subscriptions**: Subscribe to device IDs to receive real-time status updates

Key types:
- `ComelitClient`: Main client API (in `client/src/protocol/client.rs`)
- `HomeDeviceData`: Enum of all device types (Light, Thermostat, Door, etc.)
- `StatusUpdate` trait: Observer pattern for receiving device updates

### HAP Bridge Architecture

The `hap` crate bridges Comelit to HomeKit (in `hap/src/bridge.rs`):

1. **Device discovery**: Fetches device index from hub (levels 1 and 2)
2. **Accessory creation**: Converts Comelit devices to HAP accessories:
   - `ComelitLightbulbAccessory`
   - `ComelitThermostatAccessory`
   - `ComelitWindowCoveringAccessory`
   - `ComelitDoorAccessory`
   - `ComelitDoorbellAccessory`
3. **State management**: `Updater` struct receives MQTT updates and propagates to HAP accessories
4. **Web UI**: Axum server on port 8080 with dashboard, device list, and Prometheus metrics
5. **Persistence**: Uses `hap-rs` file storage for pairing and config

Each accessory type (in `hap/src/accessories/`) wraps HAP characteristics and translates between HomeKit commands and Comelit MQTT actions.

### Viper Video Protocol

The `viper-client` implements a proprietary protocol for video streaming from Comelit doorbells:

- **Control**: TCP connection on port 64100 with custom binary protocol
- **Video**: UDP packets also on port 64100 (same port, different protocol)
- **Sessions**: Little-endian session IDs track logical channels (UDPM, RTPC, device communication)
- **Channels**: CTPP (control), UDPM (UDP mode), RTPC (RTP control)
- **Detailed spec**: See `viper-client/README.md` for complete protocol analysis

Key implementation:
- `ViperClient` in `viper-client/src/client.rs`: Main API
- `*_channel.rs` files: Protocol-specific channel handlers
- `video_assembler.rs` and `audio_assembler.rs`: Stream processing

## Common Development Patterns

### Adding a New Device Type

1. Add device data struct in `client/src/protocol/out_data_messages.rs`
2. Add variant to `HomeDeviceData` enum
3. Parse device in `device_data_to_home_device()` function
4. Create accessory in `hap/src/accessories/{device_type}.rs` implementing `ComelitAccessory` trait
5. Register in `hap/src/bridge.rs` bridge startup and updater

### Modifying MQTT Messages

Message builders are in `client/src/protocol/messages.rs`. Follow the existing patterns:
- `make_*_message()` functions construct requests
- All messages need `seq_id`, `agent_id`, and `session_token` (except announce)
- Response parsing in `MqttResponseMessage` deserialization

### Working with HAP Characteristics

Each accessory has characteristics defined in accessory files. To modify:
1. Use HAP characteristic types from `hap` crate
2. Set callbacks using `.on_read()` and `.on_update()` methods
3. Update state using `.set_value()` on the characteristic
4. Changes propagate automatically to HomeKit clients

## Configuration

- **Settings file**: JSON format (see `hap/src/settings.rs`)
- **Device filtering**: Use `mount_lights`, `mount_doors`, etc. flags to enable/disable device types
- **Timings**: Configure `window_covering.{opening_time,closing_time}` and `door.{opening_closing_time,opened_time}` in seconds
- **Pairing code**: Default is 11122333 (configurable in settings)

## Web UI and Metrics

When HAP bridge runs with `--web-enabled true` (default):
- Dashboard: `http://localhost:8080/`
- Devices list: `http://localhost:8080/devices`
- Health check: `http://localhost:8080/health`
- Prometheus metrics: `http://localhost:8080/metrics`
- API status: `http://localhost:8080/api/status`

Metrics include bridge info, uptime, device counts, update counters, ping status, and connection state.

## Dependencies

- **hap-rs**: HomeKit protocol (uses forked version with patches)
- **rumqttc**: MQTT client library
- **tokio**: Async runtime (requires `full` features)
- **axum**: Web framework for metrics/UI
- **dashmap**: Concurrent hash map for device registries
- **tracing**: Logging and instrumentation

## Important Notes

- MQTT credentials are hardcoded in `client/src/protocol/credentials.rs` (compiled from obfuscated strings)
- The bridge creates random MAC address for HomeKit device ID
- Window covering position tracking is estimated based on timing (no position feedback from hub)
- Door state is simulated with timers since hub doesn't report actual door position
- Video streaming is work in progress (current branch: video-stream)
