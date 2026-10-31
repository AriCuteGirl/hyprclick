#!/bin/bash
# Build as you, then install the two binaries and the root service. Run as your own user.
set -eu
cd "$(dirname "$0")/.."
[ "$(id -u)" != 0 ] || { echo "run this as your own user, it calls sudo itself"; exit 1; }
cargo build --release
UNIT=$(mktemp)
trap 'rm -f "$UNIT"' EXIT
sed "s/@UID@/$(id -u)/" dist/lagswitchd.service.in > "$UNIT"
sudo install -m 755 target/release/lagswitchd target/release/lagswitch /usr/local/bin/
sudo install -m 644 "$UNIT" /etc/systemd/system/lagswitchd.service
sudo systemctl daemon-reload
sudo systemctl enable lagswitchd
sudo systemctl restart lagswitchd
# Started doesn't mean listening yet.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -S /run/lagswitch/sock ] && break
    sleep 0.2
done
systemctl --no-pager --lines=0 status lagswitchd
lagswitch status
