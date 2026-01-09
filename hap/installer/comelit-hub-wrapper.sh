#!/bin/sh
set -a
. /etc/comelit-hub-hap/comelit-hub-hap.env
set +a

# Write PID file before exec (using $$ gives the shell's PID, which exec will replace)
echo $$ > /var/run/comelit-hub-hap.pid

exec /usr/local/bin/comelit-hub-hap \
    --settings "$COMELIT_CONFIG" \
    --user "$COMELIT_USER" \
    --password "$COMELIT_PASSWORD" \
    --log-file /var/log/comelit-hub-hap.log \
    --error-log-file /var/log/comelit-hub-hap.err
