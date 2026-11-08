#!/bin/bash
# End-to-end check of both freeze modes. This takes your whole PC offline several
# times, but only for a few seconds each.
set -u
export LC_ALL=C
APP=${1:-org.kde.okular}
OUT=$(dirname "$0")/../whole-pc-results.txt
TMP=$(mktemp -d)
exec > >(tee "$OUT") 2>&1
sudo -v || exit 1
if flatpak ps --columns=application | grep -qx "$APP"; then
    echo "$APP is already running, pick another app"; exit 1
fi

SCOPE=
trap 'lagswitch release > /dev/null; [ -n "$SCOPE" ] && systemctl --user stop "$SCOPE"; rm -rf "$TMP"' EXIT

fails=0
check() { # name, actual, expected substring
    if [[ "$2" == *"$3"* ]]; then echo "PASS  $1"; else echo "FAIL  $1: wanted \"$3\", got \"$2\""; fails=$((fails + 1)); fi
}
online() { curl -s -m1 -o /dev/null -w '%{http_code}' http://1.1.1.1/; }
rule() { ip rule show priority 1; }
tables() { sudo -n nft list tables | grep -c lagswitch; }
# Milliseconds until the internet answers, up to about 10 s.
back_after() {
    local start i
    start=$(date +%s%3N)
    for i in $(seq 1 100); do
        if [ "$(curl -s -m0.5 -o /dev/null -w '%{http_code}' http://1.1.1.1/)" = 301 ]; then
            echo "$(($(date +%s%3N) - start))"
            return
        fi
        sleep 0.05
    done
    echo never
}
# What a UDP send returns right now.
udp_send() {
    python3 -I -c '
import errno, socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
try:
    s.sendto(b"x", ("1.1.1.1", 53)); print("sent")
except OSError as e:
    print(errno.errorcode.get(e.errno, e.errno))'
}

check "starts online" "$(online)" "301"
check "starts idle" "$(lagswitch status)" "idle"
NM_BEFORE=$(nmcli networking)

echo "--- whole pc: timed"
check "timed starts" "$(lagswitch freeze --whole-pc --ms 3000)" "network"
check "internet is gone" "$(online)" "000"
check "sends fail at once" "$(udp_send)" "ENETUNREACH"
check "loopback still works" "$(ping -c1 -W1 127.0.0.1 > /dev/null && echo ok)" "ok"
check "NetworkManager untouched" "$(nmcli networking)" "$NM_BEFORE"
sleep 3.5
check "timed ends by itself" "$(lagswitch status)" "idle"
[ -z "$(rule)" ] || { echo "FAIL  rule still there"; fails=$((fails + 1)); }
check "rules table gone" "$(tables)" "0"
check "sends work" "$(udp_send)" "sent"

echo "--- whole pc: how fast it comes back"
for _ in 1 2 3; do
    lagswitch toggle --whole-pc > /dev/null
    sleep 1
    lagswitch toggle --whole-pc > /dev/null
    echo "      internet back after $(back_after) ms"
done

echo "--- whole pc: a TCP connection survives"
python3 -I - <<'PY' &
import socket, subprocess
HTTP = b"GET / HTTP/1.1\r\nHost: 1.1.1.1\r\nConnection: keep-alive\r\n\r\n"
s = socket.create_connection(("1.1.1.1", 80), timeout=5)
s.sendall(HTTP); s.settimeout(3); s.recv(65535)
subprocess.run(["lagswitch", "freeze", "--whole-pc", "--ms", "2000"], stdout=subprocess.DEVNULL)
try:
    s.sendall(HTTP); s.settimeout(15)
    print("PASS  same connection answered after the freeze" if s.recv(65535) else "FAIL  connection closed")
except OSError as e:
    print("FAIL  connection lost:", e)
PY
wait $!
[ "$(lagswitch status)" = idle ] || lagswitch release > /dev/null

echo "--- whole pc: toggle, hold, failsafe"
check "toggle on" "$(lagswitch toggle --whole-pc)" "frozen toggle"
check "toggle off" "$(lagswitch toggle --whole-pc)" "idle"
check "hold start" "$(lagswitch hold start --whole-pc)" "frozen hold"
check "hold stop" "$(lagswitch hold stop)" "idle"
lagswitch toggle --whole-pc --cap-ms 1000 > /dev/null
sleep 1.5
check "cap ends it" "$(lagswitch status)" "idle"
check "online after" "$(online)" "301"

echo "--- whole pc: daemon dying mid-freeze"
lagswitch toggle --whole-pc > /dev/null
check "rule present" "$(rule)" "unreachable"
sudo -n systemctl kill -s KILL lagswitchd
sleep 2.5
[ -z "$(rule)" ] && echo "PASS  SIGKILL: rule gone" || { echo "FAIL  SIGKILL: rule still there"; fails=$((fails + 1)); }
check "SIGKILL: daemon restarted" "$(lagswitch status)" "idle"
check "SIGKILL: online" "$(online)" "301"
lagswitch toggle --whole-pc > /dev/null
sudo -n systemctl stop lagswitchd
[ -z "$(rule)" ] && echo "PASS  stop: rule gone" || { echo "FAIL  stop: rule still there"; fails=$((fails + 1)); }
check "stop: rules table gone" "$(tables)" "0"
sudo -n systemctl start lagswitchd
sleep 0.5
check "start: idle" "$(lagswitch status)" "idle"

echo "--- one app"
flatpak run --command=sh "$APP" -c \
    'while :; do curl -s -m1 -o /dev/null -w "%{http_code}\n" http://1.1.1.1/; sleep 0.5; done' > "$TMP/app" 2>/dev/null &
sleep 2
SCOPE=$(lagswitch list "$APP" | awk -F'\t' '{n=split($6,p,"/"); print p[n]; exit}')
app() { sleep 1.8; tail -1 "$TMP/app"; }
check "app online before" "$(app)" "301"
for dir in both in out; do
    check "$dir starts" "$(lagswitch freeze --app "$APP" --ms 3000 --dir "$dir")" "frozen timed $dir"
    check "$dir blocks the app" "$(app)" "000"
    check "$dir leaves the rest online" "$(online)" "301"
    sleep 1.5
    check "$dir ends" "$(lagswitch status)" "idle"
    check "$dir app back" "$(sleep 0.5; app)" "301"
done

echo "--- after everything"
check "online" "$(online)" "301"
check "idle" "$(lagswitch status)" "idle"
check "rules table gone" "$(tables)" "0"
echo "--- $fails failed"
echo "--- done"
