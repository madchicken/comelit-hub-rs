# Comelit Hub HAP
## A HomeKit Accessory Protocol (HAP) implementation for the Comelit Hub 20003150

This is a HomeKit Accessory Protocol (HAP) implementation for the Comelit Hub 20003150.
The current implementation supports the following features:

- Control of the Comelit Hub's lights and switches
- Control of window coverings (basic model)
- Control of the Comelit Hub's thermostats and humidifiers
- Control of doors and gates
- Listen to doorbell events (no video)

## Usage
Depending on the OS you are using, you can run the Comelit Hub HAP by executing the following command:

```
comelit-hub-hap --user admin --password admin --host 192.168.1.100 --port 1883 --settings /path/to/settings.json
```

All parameters are optional. If omitted, host will be scanned automatically.

## Installation

You can install the Comelit Hub HAP as a service, Depending on your OS:

### Linux

```
sudo cargo install comelit-hub-hap
```

### macOS
Create a configuration file named `com.comelit.hub.hap.plist` like the following:

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
  </array>

  <key>RunAtLoad</key>
  <true/>

  <key>KeepAlive</key>
  <true/>

  <key>StandardOutPath</key>
  <string>/var/log/comelit-hub-hap.log</string>
  <key>StandardErrorPath</key>
  <string>/var/log/comelit-hub-hap.err</string>
</dict>
</plist>
```

then copy executable and configuration it to the right places:

```bash
sudo cp comelit-hub-hap /usr/local/bin/
sudo cp com.comelit.hub.hap.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.comelit.hub.hap.plist
```

### Linux (systemd)

Create a file `/etc/systemd/system/comelit-hub-hap.service` with the following content:

```ini
[Unit]
Description=Comelit HUB HAP
After=network.target

[Service]
ExecStart=/usr/local/bin/comelit-hub-hap
Environment=RUST_LOG=comelit_hub_hap=info
Restart=always
RestartSec=5
User=root
WorkingDirectory=/var/lib/comelit-hub-hap

[Install]
WantedBy=multi-user.target
```

then enable and start the service:

```bash
sudo cp comelit-hub-hap /usr/local/bin/
sudo systemctl daemon-reload
sudo systemctl enable comelit-hub-hap
sudo systemctl start comelit-hub-hap
```

you can check the output of the service with:

```bash
journalctl -u comelit-hub-hap -f
```

### Windows
You can use NSSM to install the service:

```bash
nssm install ComelitHubHAP
```

start it with:

```bash
nssm start ComelitHubHAP
```
