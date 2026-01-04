#!/usr/bin/env bash
set -e

if [[ $EUID -ne 0 ]]; then
  echo "Esegui come root"
  exit 1
fi

case "$(uname -s)" in
  Darwin)
    launchctl unload /Library/LaunchDaemons/com.comelit.hub.hap.plist || true
    rm -f /Library/LaunchDaemons/com.comelit.hub.hap.plist
    ;;
  Linux)
    systemctl disable --now comelit-hub-hap || true
    rm -f /etc/systemd/system/comelit-hub-hap.service
    systemctl daemon-reload
    ;;
esac

rm -f /usr/local/bin/comelit-hub-hap
echo "✔ Disinstallato"
