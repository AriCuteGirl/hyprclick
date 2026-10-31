#!/bin/bash
# Remove the service and binaries. The rules go when the service stops.
set -eu
sudo systemctl disable --now lagswitchd
sudo rm -f /etc/systemd/system/lagswitchd.service /usr/local/bin/lagswitchd /usr/local/bin/lagswitch
sudo systemctl daemon-reload
