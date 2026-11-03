#!/bin/bash
# End-to-end check of fake lag on a throwaway process, measured over TCP and UDP.
# Needs the daemon installed. Usage: verify-lag.sh [MS]
set -u
export LC_ALL=C
MS=${1:-300}
OUT=$(dirname "$0")/../lag-results.txt
TMP=$(mktemp -d)
exec > >(tee "$OUT") 2>&1
UNIT=lagtest-$$
trap 'lagswitch lag off > /dev/null 2>&1; lagswitch release > /dev/null 2>&1; systemctl --user stop "$UNIT.scope" 2> /dev/null; rm -rf "$TMP"' EXIT

fails=0
check() { # name, ok (0 or 1), detail
    if [ "$2" = 1 ]; then echo "PASS  $1 ($3)"; else echo "FAIL  $1 ($3)"; fails=$((fails + 1)); fi
}
near() { # value, target, slack
    [ "$1" != x ] && [ "$1" -ge $(($2 - $3)) ] && [ "$1" -le $(($2 + $3)) ] && echo 1 || echo 0
}

# Round trips in ms: a TCP handshake, then a DNS query over UDP. x means it failed.
cat > "$TMP/measure" <<'EOF'
#!/bin/bash
tcp=$(curl -s -m3 -o /dev/null -w '%{time_connect}' http://1.1.1.1/ | awk '$1 > 0 {printf "%d", $1 * 1000}')
udp=$(dig +time=3 +tries=1 @1.1.1.1 example.com 2> /dev/null | awk '/Query time/ {print $4}')
echo "${tcp:-x} ${udp:-x}"
EOF
chmod +x "$TMP/measure"
systemd-run --user --scope --quiet --unit="$UNIT" bash -c \
    "while :; do $TMP/measure > $TMP/m.new; mv $TMP/m.new $TMP/m; sleep 0.2; done" &
sleep 2
PID=$(head -1 "/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/app.slice/$UNIT.scope/cgroup.procs")
# The app's latest reading, taken after any change has settled.
app() { sleep "${1:-2.5}"; cat "$TMP/m"; }

read -r BASE_TCP BASE_UDP <<< "$(app 0.5)"
echo "--- normal: tcp ${BASE_TCP} ms, udp ${BASE_UDP} ms"
[ "$BASE_TCP" != x ] && [ "$BASE_UDP" != x ] || { echo "FAIL  the test app can't reach 1.1.1.1"; exit 1; }

echo "--- fake lag $MS ms"
check "lag on" "$(lagswitch lag on --pid "$PID" --ms "$MS" | grep -c "lag $MS")" "$(lagswitch status)"
read -r tcp udp <<< "$(app)"
check "TCP ping goes up by $MS" "$(near "$tcp" $((BASE_TCP + MS)) 60)" "$tcp ms"
check "UDP ping goes up by $MS" "$(near "$udp" $((BASE_UDP + MS)) 60)" "$udp ms"
read -r tcp udp <<< "$("$TMP/measure")"
check "other apps keep their ping" "$(near "$tcp" "$BASE_TCP" 40)" "tcp $tcp ms, udp $udp ms"

echo "--- change to 100 ms while on"
lagswitch lag on --pid "$PID" --ms 100 > /dev/null
read -r tcp udp <<< "$(app)"
check "TCP ping follows the new value" "$(near "$tcp" $((BASE_TCP + 100)) 50)" "$tcp ms"
check "UDP ping follows the new value" "$(near "$udp" $((BASE_UDP + 100)) 50)" "$udp ms"

echo "--- freeze while lagged"
lagswitch freeze --pid "$PID" --ms 2500 --dir both > /dev/null
read -r tcp udp <<< "$(app 2)"
check "freeze still blocks the app" "$([ "$tcp" = x ] && echo 1 || echo 0)" "tcp $tcp"
sleep 1.5
check "lag stays on after the freeze" "$(lagswitch status | grep -c 'idle lag 100')" "$(lagswitch status)"
read -r tcp udp <<< "$(app)"
check "lagged again after the freeze" "$(near "$tcp" $((BASE_TCP + 100)) 50)" "$tcp ms"

echo "--- toggle off"
check "toggle turns it off" "$(lagswitch lag toggle --pid "$PID" | grep -cx idle)" "$(lagswitch status)"
read -r tcp udp <<< "$(app)"
check "ping back to normal" "$(near "$tcp" "$BASE_TCP" 40)" "tcp $tcp ms, udp $udp ms"

echo "--- daemon killed while lagged"
lagswitch lag on --pid "$PID" --ms "$MS" > /dev/null
sudo -n systemctl kill -s KILL lagswitchd 2> /dev/null || sudo systemctl kill -s KILL lagswitchd
sleep 3
check "daemon came back idle" "$(lagswitch status | grep -cx idle)" "$(lagswitch status)"
read -r tcp udp <<< "$(app)"
check "app works at normal ping" "$(near "$tcp" "$BASE_TCP" 40)" "tcp $tcp ms, udp $udp ms"

echo "--- refused"
check "whole PC refused" "$(lagswitch lag on --whole-pc 2>&1 | grep -c 'unknown option')" "fake lag is one app only"
check "too much refused" "$(lagswitch lag on --pid "$PID" --ms 9000 2>&1 | grep -c '1 to 5000')" "over 5000 ms"
echo "--- $fails failed"
echo "--- done"
