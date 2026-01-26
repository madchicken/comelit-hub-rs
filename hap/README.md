# Comelit Hub HAP
## A HomeKit Accessory Protocol (HAP) implementation for the Comelit Hub 20003150

This is a HomeKit Accessory Protocol (HAP) implementation for the Comelit Hub 20003150.
The current implementation supports the following features:

- Control of the Comelit Hub's lights and switches
- Control of window coverings (basic model)
- Control of the Comelit Hub's thermostats and humidifiers
- Control of doors and gates
- Listen to doorbell events (no video)

## Build
To build the Comelit Hub HAP, you need to have Rust installed on your system.
You can install Rust by following the instructions on the [official Rust website](https://www.rust-lang.org/tools/install).

Once Rust is installed, you can build the Comelit Hub HAP by running the following command from the main directory:

```
cargo build --release --manifest-path=hap/Cargo.toml
```

This will build the Comelit Hub HAP in release mode and place the executable in the `target/release` directory.

## Usage
Depending on the OS you are using, you can run the Comelit Hub HAP by executing the following command:

```
comelit-hub-hap --user admin --password admin --host 192.168.1.100 --port 1883 --settings /path/to/settings.json
```

All parameters are optional. If omitted, host will be scanned automatically.

### Logging Options

The application supports built-in log rotation, which works natively on all platforms including macOS:

| Option | Description | Default |
|--------|-------------|---------|
| `--log-dir <PATH>` | Directory for log files. If not set, logs to stdout | None (stdout) |
| `--log-prefix <PREFIX>` | Prefix for log file names | `comelit-hub` |
| `--log-rotation <PERIOD>` | Rotation period: `minutely`, `hourly`, `daily`, `never` | `daily` |
| `--max-log-files <N>` | Maximum number of log files to keep (0 = unlimited) | `7` |
| `--log-to-console` | Also output logs to console when file logging is enabled | `false` |

#### Examples

```bash
# Log to console only (default)
comelit-hub-hap --user admin --password admin

# Log to files with daily rotation, keeping 7 days of logs
comelit-hub-hap --log-dir /var/log/comelit-hub-hap --log-prefix my-hub --log-rotation daily --max-log-files 7

# Log to files with hourly rotation
comelit-hub-hap --log-dir ./logs --log-rotation hourly --max-log-files 24

# Log to both file and console
comelit-hub-hap --log-dir ./logs --log-to-console
```

Log files are named with timestamps, for example: `comelit-hub.2024-01-15.log` (for daily rotation).

## Installation

You can install the Comelit Hub HAP as a service. The installer handles all configuration automatically.

### Using the Installer Script

The easiest way to install is using the provided installer script:

```bash
cd hap/installer
sudo ./install.sh
```

This will:
- Create a system user `comelit`
- Install the binary to `/usr/local/bin/`
- Set up the service (launchd on macOS, systemd on Linux)
- Create configuration files in `/etc/comelit-hub-hap/`
- Create the log directory at `/var/log/comelit-hub-hap/`

### Configuration

After installation, edit the configuration file:

```bash
sudo nano /etc/comelit-hub-hap/comelit-hub-hap.env
```

Configuration options:
```
RUST_LOG=comelit_hub_hap=info
COMELIT_CONFIG=/etc/comelit-hub-hap/comelit-hub-hap-config.json
COMELIT_USER=admin
COMELIT_PASSWORD=admin

# Logging configuration
COMELIT_LOG_DIR=/var/log/comelit-hub-hap
COMELIT_LOG_PREFIX=comelit-hub
COMELIT_LOG_ROTATION=daily
COMELIT_MAX_LOG_FILES=7
```

### Service Management

Use the control script to manage the service:

```bash
# Start the service
sudo comelit-hub-ctl start

# Stop the service
sudo comelit-hub-ctl stop

# Restart the service
sudo comelit-hub-ctl restart

# Check service status
comelit-hub-ctl status

# View recent logs
comelit-hub-ctl logs

# Follow logs in real-time
comelit-hub-ctl logs -f

# View last 100 lines
comelit-hub-ctl logs -n 100

# List all log files
comelit-hub-ctl list-logs
```

### macOS Manual Installation

Create a configuration file named `com.comelit.hub.hap.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
 "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.comelit.hub.hap</string>

  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/comelit-hub-hap</string>
    <string>--log-dir</string>
    <string>/var/log/comelit-hub-hap</string>
    <string>--log-rotation</string>
    <string>daily</string>
    <string>--max-log-files</string>
    <string>7</string>
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <true/>

  <key>WorkingDirectory</key>
  <string>/var/lib/comelit-hub-hap</string>
</dict>
</plist>
```

Then install:

```bash
sudo mkdir -p /var/log/comelit-hub-hap /var/lib/comelit-hub-hap
sudo cp comelit-hub-hap /usr/local/bin/
sudo cp com.comelit.hub.hap.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.comelit.hub.hap.plist
```

### Linux (systemd) Manual Installation

Create a file `/etc/systemd/system/comelit-hub-hap.service`:

```ini
[Unit]
Description=Comelit HUB HAP
After=network.target

[Service]
ExecStart=/usr/local/bin/comelit-hub-hap --log-dir /var/log/comelit-hub-hap --log-rotation daily --max-log-files 7
Environment=RUST_LOG=comelit_hub_hap=info
Restart=always
RestartSec=5
User=comelit
WorkingDirectory=/var/lib/comelit-hub-hap

[Install]
WantedBy=multi-user.target
```

Then enable and start the service:

```bash
sudo mkdir -p /var/log/comelit-hub-hap /var/lib/comelit-hub-hap
sudo useradd --system --no-create-home comelit
sudo chown comelit:comelit /var/log/comelit-hub-hap /var/lib/comelit-hub-hap
sudo cp comelit-hub-hap /usr/local/bin/
sudo systemctl daemon-reload
sudo systemctl enable comelit-hub-hap
sudo systemctl start comelit-hub-hap
```

Check the service status:

```bash
systemctl status comelit-hub-hap
```

View logs:

```bash
# Application logs (with rotation)
ls -la /var/log/comelit-hub-hap/
tail -f /var/log/comelit-hub-hap/comelit-hub.*.log

# Or use journalctl for systemd output
journalctl -u comelit-hub-hap -f
```

### Windows

You can use NSSM to install the service:

```bash
nssm install ComelitHubHAP "C:\path\to\comelit-hub-hap.exe" "--log-dir" "C:\logs\comelit" "--log-rotation" "daily"
```

Start it with:

```bash
nssm start ComelitHubHAP
```

## Uninstalling

Use the uninstall script:

```bash
cd hap/installer
sudo ./uninstall.sh
```

This will stop the service, remove the binary, and clean up configuration files.