#!/usr/bin/env bash
set -e

BIN_TYPE="${BIN_TYPE:-release}"
BIN_NAME="comelit-hub-hap"
BIN_SRC="../../target/$BIN_TYPE/$BIN_NAME"
BIN_DST="/usr/local/bin/$BIN_NAME"
LOG_DIR="/var/log/comelit-hub-hap"

if [[ $EUID -ne 0 ]]; then
  echo "Run this script as root (sudo)"
  exit 1
fi

if [[ ! -f "$BIN_SRC" ]]; then
  echo "Binary $BIN_NAME not found"
  exit 1
fi

install_binary() {
  echo "→ Installing binary"
  cp "$BIN_SRC" "$BIN_DST"
  chmod 755 "$BIN_DST"
}

# An existing configuration (credentials, ViP token) is kept on upgrades
install_config() {
  mkdir -p /etc/comelit-hub-hap
  for file in comelit-hub-hap.env:comelit-hub-hap.env default-config.json:comelit-hub-hap-config.json; do
    src="${file%%:*}"
    dst="/etc/comelit-hub-hap/${file##*:}"
    if [[ -f "$dst" ]]; then
      echo "→ Keeping $dst"
    else
      cp "./$src" "$dst"
    fi
  done
  chmod 600 /etc/comelit-hub-hap/comelit-hub-hap.env /etc/comelit-hub-hap/comelit-hub-hap-config.json
}

install_macos() {
  echo "→ macOS detected"
  install_binary

  # working directory of the service: HomeKit pairings and PID file
  mkdir -p /var/lib/comelit-hub-hap

  cp ./macos/com.comelit.hub.hap.plist \
     /Library/LaunchDaemons/
  install_config
  cp ./comelit-hub-wrapper.sh /usr/local/bin/comelit-hub-hap-wrapper.sh
  chmod 755 /usr/local/bin/comelit-hub-hap-wrapper.sh

  cp ./comelit-hub-ctl.sh /usr/local/bin/comelit-hub-ctl
  chmod 755 /usr/local/bin/comelit-hub-ctl

  # Create log directory with proper ownership
  # Log rotation is handled internally by the application
  mkdir -p "$LOG_DIR"
  touch "$LOG_DIR/comelit-hub-hap.log"
  touch "$LOG_DIR/comelit-hub-hap.err"
  chmod 755 "$LOG_DIR"
  chmod 644 "$LOG_DIR/comelit-hub-hap.log"
  chmod 644 "$LOG_DIR/comelit-hub-hap.err"

  launchctl unload /Library/LaunchDaemons/com.comelit.hub.hap.plist 2>/dev/null || true
  launchctl load /Library/LaunchDaemons/com.comelit.hub.hap.plist

  echo "✔ Services macOS installed"
  echo ""
  echo "Note: Log rotation is handled automatically by the application."
  echo "      Logs are stored in: $LOG_DIR"
  echo "      Configure rotation settings in: /etc/comelit-hub-hap/comelit-hub-hap.env"
}

install_linux() {
  echo "→ Linux detected"
  install_binary

  mkdir -p /var/lib/comelit-hub-hap

  cp ./linux/comelit-hub-hap.service \
     /etc/systemd/system/

  install_config
  cp ./comelit-hub-wrapper.sh /usr/local/bin/comelit-hub-hap-wrapper.sh
  chmod 755 /usr/local/bin/comelit-hub-hap-wrapper.sh

  cp ./comelit-hub-ctl.sh /usr/local/bin/comelit-hub-ctl
  chmod 755 /usr/local/bin/comelit-hub-ctl

  # Create log directory with proper ownership
  # Log rotation is handled internally by the application
  mkdir -p "$LOG_DIR"
  touch "$LOG_DIR/comelit-hub-hap.log"
  touch "$LOG_DIR/comelit-hub-hap.err"
  chmod 755 "$LOG_DIR"
  chmod 644 "$LOG_DIR/comelit-hub-hap.log"
  chmod 644 "$LOG_DIR/comelit-hub-hap.err"

  systemctl daemon-reload
  systemctl enable comelit-hub-hap
  systemctl restart comelit-hub-hap

  echo "✔ Services Linux installed"
  echo ""
  echo "Note: Log rotation is handled automatically by the application."
  echo "      Logs are stored in: $LOG_DIR"
  echo "      Configure rotation settings in: /etc/comelit-hub-hap/comelit-hub-hap.env"
}

case "$(uname -s)" in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *)
    echo "Unsupported system"
    exit 1
    ;;
esac
