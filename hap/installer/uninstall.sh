#!/usr/bin/env bash
set -e

if [[ $EUID -ne 0 ]]; then
  echo "Execute as root"
  exit 1
fi

LOG_DIR="/var/log/comelit-hub-hap"

case "$(uname -s)" in
  Darwin)
    launchctl unload /Library/LaunchDaemons/com.comelit.hub.hap.plist || true
    rm -f /Library/LaunchDaemons/com.comelit.hub.hap.plist
    rm -f /var/lib/comelit-hub-hap/comelit-hub-hap.pid
    ;;
  Linux)
    systemctl disable --now comelit-hub-hap || true
    rm -f /etc/systemd/system/comelit-hub-hap.service
    rm -rf /run/comelit-hub-hap
    systemctl daemon-reload
    ;;
esac

rm -f /etc/comelit-hub-hap/comelit-hub-hap.env
rm -f /etc/comelit-hub-hap/comelit-hub-hap-config.json
rmdir /etc/comelit-hub-hap 2>/dev/null || true
rm -f /usr/local/bin/comelit-hub-hap
rm -f /usr/local/bin/comelit-hub-hap-wrapper.sh
rm -f /usr/local/bin/comelit-hub-ctl

# Ask before removing logs
if [[ -d "$LOG_DIR" ]]; then
  read -p "Remove log directory $LOG_DIR? [y/N] " -n 1 -r
  echo
  if [[ $REPLY =~ ^[Yy]$ ]]; then
    rm -rf "$LOG_DIR"
    echo "✔ Log directory removed"
  else
    echo "→ Log directory preserved at $LOG_DIR"
  fi
fi

# rm -rf /var/lib/comelit-hub-hap/data
echo "✔ Uninstalled"
