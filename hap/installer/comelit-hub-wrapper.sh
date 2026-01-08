#!/bin/sh
set -a
. /etc/comelit-hub-hap/comelit-hub-hap.env
set +a

exec /usr/local/bin/comelit-hub-hap --config $COMELIT_CONFIG --user $COMELIT_USER --password $COMELIT_PASSWORD
echo $$ > /var/run/comelit-hub-hap.pid
