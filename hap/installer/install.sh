#!/usr/bin/env bash
set -e

BIN_NAME="comelit-hub-hap"
BIN_SRC="./$BIN_NAME"
BIN_DST="/usr/local/bin/$BIN_NAME"

if [[ $EUID -ne 0 ]]; then
  echo "Esegui come root (sudo)"
  exit 1
fi

if [[ ! -f "$BIN_SRC" ]]; then
  echo "Binario $BIN_NAME non trovato"
  exit 1
fi

install_binary() {
  echo "→ Installazione binario"
  cp "$BIN_SRC" "$BIN_DST"
  chmod 755 "$BIN_DST"
}

install_macos() {
  echo "→ macOS detected"
  create_macos_user
  install_binary

  cp installer/macos/com.comelit.hub.hap.plist \
     /Library/LaunchDaemons/

  launchctl unload /Library/LaunchDaemons/com.comelit.hub.hap.plist 2>/dev/null || true
  launchctl load /Library/LaunchDaemons/com.comelit.hub.hap.plist

  echo "✔ Servizio macOS installato"
}

install_linux() {
  echo "→ Linux detected"
  create_linux_user
  install_binary

  mkdir -p /var/lib/comelit-hub-hap

  cp installer/linux/comelit-hub-hap.service \
     /etc/systemd/system/

  systemctl daemon-reload
  systemctl enable comelit-hub-hap
  systemctl restart comelit-hub-hap

  echo "✔ Servizio Linux installato"
}

create_macos_user() {
  if id comelit &>/dev/null; then
    echo "→ Utente comelit già esistente"
    return
  fi

  echo "→ Creazione utente di sistema comelit"

  sysadminctl -addUser comelit \
    -system \
    -shell /usr/bin/false \
    -home /var/lib/comelit-hub-hap

  mkdir -p /var/lib/comelit-hub-hap
  chown -R comelit:wheel /var/lib/comelit-hub-hap
}

create_linux_user() {
  if id comelit &>/dev/null; then
    echo "→ Utente comelit già esistente"
    return
  fi

  echo "→ Creazione utente di sistema comelit"

  useradd \
    --system \
    --no-create-home \
    --shell /usr/sbin/nologin \
    comelit

  mkdir -p /var/lib/comelit-hub-hap
  chown -R comelit:comelit /var/lib/comelit-hub-hap
}

case "$(uname -s)" in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *)
    echo "Sistema non supportato"
    exit 1
    ;;
esac
