#!/bin/sh
set -a
. /etc/comelit-hub-hap/comelit-hub-hap.env
set +a

# Determine the PID file location based on OS
# - Linux (systemd): RuntimeDirectory creates /run/comelit-hub-hap
# - macOS: Use /var/lib/comelit-hub-hap which is owned by comelit user
case "$(uname -s)" in
    Linux)
        PID_DIR="/run/comelit-hub-hap"
        ;;
    Darwin)
        PID_DIR="/var/lib/comelit-hub-hap"
        ;;
    *)
        PID_DIR="/var/lib/comelit-hub-hap"
        ;;
esac

# Write PID file before exec (using $$ gives the shell's PID, which exec will replace)
echo $$ > "${PID_DIR}/comelit-hub-hap.pid"

exec /usr/local/bin/comelit-hub-hap \
    --settings "$COMELIT_CONFIG" \
    --user "$COMELIT_USER" \
    --password "$COMELIT_PASSWORD" \
    --log-dir "$COMELIT_LOG_DIR" \
    --log-prefix "$COMELIT_LOG_PREFIX" \
    --log-rotation "$COMELIT_LOG_ROTATION" \
    --max-log-files "$COMELIT_MAX_LOG_FILES"
