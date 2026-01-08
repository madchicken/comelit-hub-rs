#!/usr/bin/env bash
set -e

if [[ $EUID -ne 0 ]]; then
  echo "Execute as root"
  exit 1
fi

case "$(uname -s)" in
  Darwin)
    launchctl unload /Library/LaunchDaemons/com.comelit.hub.hap.plist || true
    rm -f /Library/LaunchDaemons/com.comelit.hub.hap.plist
    rm -f /etc/newsyslog.d/comelit-hub-hap.conf
    ;;
  Linux)
    systemctl disable --now comelit-hub-hap || true
    rm -f /etc/systemd/system/comelit-hub-hap.service
    rm -f /etc/logrotate.d/comelit-hub-hap
    systemctl daemon-reload
    ;;
esac

rm -f /etc/comelit-hub-hap/comelit-hub-hap.env
rm -f /etc/comelit-hub-hap/comelit-hub-hap-config.json
rm -f /usr/local/bin/comelit-hub-hap
rm -f /usr/local/bin/comelit-hub-hap-wrapper.sh
# rm -rf /var/lib/comelit-hub-hap/data
echo "✔ Uninstalled"
