#!/bin/bash
# End-to-end check of the installed daemon against a throwaway Flatpak sandbox.
set -u
APP=${1:-org.kde.okular}
OUT=$(dirname "$0")/../daemon-results.txt
TMP=$(mktemp -d)
exec > >(tee "$OUT") 2>&1

sudo -v || exit 1
if flatpak ps --columns=application | grep -qx "$APP"; then
    echo "$APP is already running, pick another app"; exit 1
fi

fails=0
check() { # name, actual, expected substring
    if [[ "$2" == *"$3"* ]]; then echo "PASS  $1"; else echo "FAIL  $1: wanted \"$3\", got \"$2\""; fails=$((fails + 1)); fi
}
# Newest result from the curl loop inside the sandbox, after giving it time to report.
app() { sleep 1.8; tail -1 "$TMP/app"; }
tables() { sudo -n nft list tables | grep -c lagswitch; }
host() { curl -s -m2 -o /dev/null -w '%{http_code}' http://1.1.1.1/; }

flatpak run --command=sh "$APP" -c \
    'while :; do curl -s -m1 -o /dev/null -w "%{http_code}\n" http://1.1.1.1/; sleep 0.5; done' > "$TMP/app" 2>/dev/null &
sleep 2
SCOPE=$(lagswitch list "$APP" | awk -F'\t' '{n=split($6,p,"/"); print p[n]; exit}')
trap 'systemctl --user stop "$SCOPE"; lagswitch release > /dev/null; rm -rf "$TMP"' EXIT

check "starts idle" "$(lagswitch status)" "idle"
check "app online before" "$(app)" "301"

echo "--- timed"
check "timed starts" "$(lagswitch freeze --app "$APP" --ms 4000)" "frozen timed both"
check "app frozen" "$(app)" "000"
check "host online" "$(host)" "301"
sleep 2.5
check "timed ends by itself" "$(lagswitch status)" "idle"
check "rules gone" "$(tables)" "0"
check "app back" "$(app)" "301"
lagswitch freeze --app "$APP" --ms 20000 > /dev/null
check "second press ends a timed freeze" "$(lagswitch freeze --app "$APP" --ms 20000)" "idle"

echo "--- toggle"
check "toggle on" "$(lagswitch toggle --app "$APP")" "frozen toggle"
check "app frozen" "$(app)" "000"
check "hold stop leaves toggle running" "$(lagswitch hold stop)" "frozen toggle"
check "toggle off" "$(lagswitch toggle --app "$APP")" "idle"
check "app back" "$(app)" "301"

echo "--- hold"
check "hold start" "$(lagswitch hold start --app "$APP")" "frozen hold"
check "app frozen" "$(app)" "000"
check "hold stop" "$(lagswitch hold stop)" "idle"
check "app back" "$(app)" "301"

echo "--- failsafe"
lagswitch toggle --app "$APP" --cap-ms 1500 > /dev/null
sleep 2
check "cap ends a toggle" "$(lagswitch status)" "idle"
lagswitch hold start --app "$APP" --cap-ms 1500 > /dev/null
sleep 2
check "cap ends a hold" "$(lagswitch status)" "idle"
lagswitch freeze --app "$APP" --ms 60000 --cap-ms 1500 > /dev/null
sleep 2
check "cap ends a long timed freeze" "$(lagswitch status)" "idle"

echo "--- directions"
for dir in in out; do
    check "$dir starts" "$(lagswitch freeze --app "$APP" --ms 3000 --dir "$dir")" "frozen timed $dir"
    check "$dir blocks the app" "$(app)" "000"
    sleep 1.5
    check "$dir ends" "$(lagswitch status)" "idle"
    sleep 2
done

echo "--- picker and config"
check "list finds the app" "$(lagswitch list "$APP")" "$APP"
check "last app is remembered" "$(lagswitch freeze --ms 1000)" "frozen timed"
sleep 1.5
check "toggle uses last app" "$(lagswitch toggle)" "frozen toggle"
lagswitch release > /dev/null
check "config saved" "$(cat ~/.config/lagswitch/config.toml)" "last_app = \"$APP\""

echo "--- refusals"
check "system process" "$(lagswitch freeze --pid 1 --ms 100 2>&1)" "doesn't belong"
check "unknown app" "$(lagswitch freeze --app no.such.app --ms 100 2>&1)" "no running Flatpak"
check "another user's process" "$(lagswitch freeze --app systemd-journald --ms 100 2>&1)" "no running"
check "still idle" "$(lagswitch status)" "idle"

echo "--- daemon dying mid-freeze"
lagswitch toggle --app "$APP" > /dev/null
sudo -n systemctl kill -s KILL lagswitchd
sleep 2.5
check "SIGKILL: rules gone" "$(tables)" "0"
check "SIGKILL: daemon restarted" "$(lagswitch status)" "idle"
check "SIGKILL: app back" "$(app)" "301"
lagswitch toggle --app "$APP" > /dev/null
sudo -n systemctl stop lagswitchd
check "stop: rules gone" "$(tables)" "0"
check "stop: client message" "$(lagswitch status 2>&1)" "isn't running"
sudo -n systemctl start lagswitchd
sleep 0.5
check "start: idle" "$(lagswitch status)" "idle"

echo "--- kernel module loads on demand, as after a reboot"
sudo -n rmmod nft_socket 2>&1
check "freeze with module unloaded" "$(lagswitch freeze --app "$APP" --ms 1000)" "frozen timed"
lagswitch release > /dev/null

echo "--- $fails failed"
echo "--- done"
