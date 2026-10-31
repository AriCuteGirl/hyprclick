#!/bin/bash
# End-to-end check of Whole PC mode. This takes your whole PC offline several times.
set -u
OUT=$(dirname "$0")/../whole-pc-results.txt
exec > >(tee "$OUT") 2>&1
MARKER=/var/lib/lagswitch/network-off
sudo -v || exit 1
trap 'lagswitch release > /dev/null' EXIT

fails=0
check() { # name, actual, expected substring
    if [[ "$2" == *"$3"* ]]; then echo "PASS  $1"; else echo "FAIL  $1: wanted \"$3\", got \"$2\""; fails=$((fails + 1)); fi
}
online() { curl -s -m1 -o /dev/null -w '%{http_code}' http://1.1.1.1/; }
# Seconds until the internet answers again, up to about 40.
back_after() {
    local start i
    start=$(date +%s.%N)
    for i in $(seq 1 40); do
        if [ "$(online)" = 301 ]; then
            printf '%.1f' "$(echo "$(date +%s.%N) - $start" | bc)"
            return
        fi
        sleep 0.5
    done
    echo never
}
state() { nmcli networking; }
marker() { sudo -n ls "$MARKER" 2>&1; }

check "starts online" "$(online)" "301"
check "starts idle" "$(lagswitch status)" "idle"

echo "--- timed"
check "timed starts" "$(lagswitch freeze --whole-pc --ms 3000)" "network"
check "networking is off" "$(state)" "disabled"
check "internet is gone" "$(online)" "000"
check "marker written" "$(marker)" "$MARKER"
sleep 2.5
check "timed ends by itself" "$(lagswitch status)" "idle"
check "networking is on" "$(state)" "enabled"
echo "      internet back $(back_after) s after release"
check "marker gone" "$(marker)" "No such"

echo "--- second press ends it early"
lagswitch freeze --whole-pc --ms 20000 > /dev/null
check "second press" "$(lagswitch freeze --whole-pc --ms 20000)" "idle"
check "networking is on" "$(state)" "enabled"
echo "      internet back $(back_after) s after release"

echo "--- toggle and hold"
check "toggle on" "$(lagswitch toggle --whole-pc)" "frozen toggle"
check "networking is off" "$(state)" "disabled"
check "toggle off" "$(lagswitch toggle --whole-pc)" "idle"
echo "      internet back $(back_after) s after release"
check "hold start" "$(lagswitch hold start --whole-pc)" "frozen hold"
check "hold stop" "$(lagswitch hold stop)" "idle"
check "networking is on" "$(state)" "enabled"
echo "      internet back $(back_after) s after release"

echo "--- failsafe"
lagswitch toggle --whole-pc --cap-ms 1500 > /dev/null
sleep 2
check "cap ends it" "$(lagswitch status)" "idle"
check "networking is on" "$(state)" "enabled"
echo "      internet back $(back_after) s after release"

echo "--- daemon dying mid-freeze"
lagswitch toggle --whole-pc > /dev/null
check "networking is off" "$(state)" "disabled"
sudo -n systemctl kill -s KILL lagswitchd
sleep 3
check "SIGKILL: networking is on" "$(state)" "enabled"
check "SIGKILL: daemon restarted" "$(lagswitch status)" "idle"
check "SIGKILL: marker gone" "$(marker)" "No such"
echo "      internet back $(back_after) s after restart"
lagswitch toggle --whole-pc > /dev/null
sudo -n systemctl stop lagswitchd
check "stop: networking is on" "$(state)" "enabled"
sudo -n systemctl start lagswitchd
sleep 0.5
check "start: idle" "$(lagswitch status)" "idle"
echo "      internet back $(back_after) s"

echo "--- networking you turned off yourself stays off"
nmcli networking off
sudo -n systemctl restart lagswitchd
sleep 0.5
check "restart leaves it off" "$(state)" "disabled"
nmcli networking on
echo "      internet back $(back_after) s"

echo "--- after everything"
check "online" "$(online)" "301"
check "idle" "$(lagswitch status)" "idle"
echo "--- $fails failed"
echo "--- done"
