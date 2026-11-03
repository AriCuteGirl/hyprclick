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

# When it started, then round trips in ms: a TCP handshake and a DNS query over UDP.
# x means it failed.
cat > "$TMP/measure" <<'EOF'
#!/bin/bash
start=$(date +%s%3N)
tcp=$(curl -s -m3 -o /dev/null -w '%{time_connect}' http://1.1.1.1/ | awk '$1 > 0 {printf "%d", $1 * 1000}')
udp=$(dig +time=3 +tries=1 @1.1.1.1 example.com 2> /dev/null | awk '/Query time/ {print $4}')
echo "$start ${tcp:-x} ${udp:-x}"
EOF
chmod +x "$TMP/measure"
systemd-run --user --scope --quiet --unit="$UNIT" bash -c \
    "while :; do $TMP/measure > $TMP/m.new; cat $TMP/m.new >> $TMP/log; mv $TMP/m.new $TMP/m; sleep 0.2; done" &
sleep 2
PID=$(head -1 "/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/app.slice/$UNIT.scope/cgroup.procs")
# The app's first reading that started after now, plus a settle time in ms.
app() {
    local since=$(($(date +%s%3N) + ${1:-300})) line _
    for _ in $(seq 1 100); do
        line=$(awk -v s="$since" '$1 >= s {print $2, $3; exit}' "$TMP/log")
        [ -n "$line" ] && { echo "$line"; return; }
        sleep 0.1
    done
    echo "x x"
}

read -r BASE_TCP BASE_UDP <<< "$(app 0)"
echo "--- normal: tcp ${BASE_TCP} ms, udp ${BASE_UDP} ms"
[ "$BASE_TCP" != x ] && [ "$BASE_UDP" != x ] || { echo "FAIL  the test app can't reach 1.1.1.1"; exit 1; }

echo "--- fake lag $MS ms"
check "lag on" "$(lagswitch lag on --pid "$PID" --ms "$MS" | grep -c "lag $MS")" "$(lagswitch status)"
read -r tcp udp <<< "$(app)"
check "TCP ping goes up by $MS" "$(near "$tcp" $((BASE_TCP + MS)) 60)" "$tcp ms"
check "UDP ping goes up by $MS" "$(near "$udp" $((BASE_UDP + MS)) 60)" "$udp ms"
read -r _ tcp udp <<< "$("$TMP/measure")"
check "other apps keep their ping" "$(near "$tcp" "$BASE_TCP" 40)" "tcp $tcp ms, udp $udp ms"

echo "--- change to 100 ms while on"
lagswitch lag on --pid "$PID" --ms 100 > /dev/null
read -r tcp udp <<< "$(app)"
check "TCP ping follows the new value" "$(near "$tcp" $((BASE_TCP + 100)) 50)" "$tcp ms"
check "UDP ping follows the new value" "$(near "$udp" $((BASE_UDP + 100)) 50)" "$udp ms"

echo "--- freeze while lagged"
FROM=$(date +%s%3N)
lagswitch freeze --pid "$PID" --ms 2500 --dir both > /dev/null
sleep 4
# Only TCP probes that started during the freeze count: the UDP one runs after
# the TCP one gives up, by which time the freeze can be over.
during=$(awk -v a=$((FROM + 100)) -v b=$((FROM + 2300)) '$1 >= a && $1 <= b {print $2}' "$TMP/log" | tr '\n' ' ')
blocked=$([ -n "$during" ] && ! grep -q '[0-9]' <<< "$during" && echo 1 || echo 0)
check "freeze still blocks the app" "$blocked" "tcp during the freeze: ${during:-none}"
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
