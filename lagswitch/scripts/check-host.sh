#!/bin/bash
# During an inbound freeze, did the rule catch other apps, or did the remote
# server throttle us? Aim the frozen app at one address while the host fetches
# from two. If only the app's address suffers, the rule isn't the cause.
set -u
APP=${1:-org.kde.okular}
A=1.1.1.1
B=208.67.222.222
BIN=$(dirname "$0")/../target/debug/lagswitch
OUT=$(dirname "$0")/../check-results.txt
TMP=$(mktemp -d)
exec > >(tee "$OUT") 2>&1

sudo -v || exit 1
if flatpak ps --columns=application | grep -qx "$APP"; then
    echo "$APP is already running, pick another app"; exit 1
fi

SCOPE=
cleanup() {
    [ -n "$SCOPE" ] && systemctl --user stop "$SCOPE"
    sudo -n "$BIN" release
    jobs -p | xargs -r kill 2>/dev/null
    rm -rf "$TMP"
}
trap cleanup EXIT

host_loop() {
    while :; do
        curl -s -m3 -o /dev/null -w '%{http_code} %{time_total}\n' "http://$1/" >> "$2"
        sleep 0.4
    done
}

summary() {
    awk -v name="$1" '{n++; if ($1 == "000") fail++; else if ($2 > 0.5) slow++}
        END {printf "  host -> %-10s %d fetches, %d failed, %d slower than 0.5 s\n", name, n, fail, slow}' "$2"
}

# phase NAME APP_TARGET FREEZE(yes/no)
phase() {
    echo "--- $1: app fetches $2, freeze=$3"
    flatpak run --command=sh "$APP" -c \
        "while :; do curl -s -m1 -o /dev/null http://$2/; sleep 0.5; done" 2>/dev/null &
    sleep 2
    SCOPE=$("$BIN" list | awk -v a="$APP" '$1==a{n=split($2,p,"/"); print p[n]; exit}')
    : > "$TMP/a"; : > "$TMP/b"
    host_loop $A "$TMP/a" & LA=$!
    host_loop $B "$TMP/b" & LB=$!
    for _ in 1 2 3 4 5; do
        if [ "$3" = yes ]; then
            sudo -n "$BIN" freeze --app "$APP" --ms 3000 --dir in > /dev/null
        else
            sleep 3
        fi
        sleep 1
    done
    kill $LA $LB; wait $LA $LB 2>/dev/null
    summary $A "$TMP/a"
    summary $B "$TMP/b"
    systemctl --user stop "$SCOPE"
    SCOPE=
    sleep 3
}

phase "no freeze" $A no
phase "freeze, app on A" $A yes
sleep 20
phase "freeze, app on B" $B yes
phase "no freeze again" $A no
echo "--- tables left behind (must be empty)"
sudo -n nft list tables | grep lagswitch
echo "--- done"
