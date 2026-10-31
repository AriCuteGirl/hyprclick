#!/usr/bin/env python3
"""Does `socket cgroupv2` match traffic on this kernel, in each hook?

Runs as your user and only calls sudo for nft. Everything is aimed at a
throwaway systemd scope this script creates; a second scope is the control
and must stay online. The only nft table touched is lagswitch_probe.
"""
import errno
import os
import re
import socket
import subprocess
import sys

TABLE = "lagswitch_probe"
MARK = 0x1a65
HOST = "1.1.1.1"
DNS = bytes.fromhex("123401000001000000000000076578616d706c6503636f6d0000010001")
HTTP = b"GET / HTTP/1.1\r\nHost: 1.1.1.1\r\nConnection: keep-alive\r\n\r\n"
RESULTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "probe-results.txt")


def own_cgroup():
    with open("/proc/self/cgroup") as f:
        for line in f:
            if line.startswith("0::"):
                return line[3:].strip().lstrip("/")
    raise SystemExit("no cgroup v2 entry in /proc/self/cgroup")


def nft(script):
    subprocess.run(["sudo", "-n", "nft", "-f", "-"], input=script.encode(), check=True)


def block(hook, cgroup):
    level = cgroup.count("/") + 1
    if hook == "egress":
        # Mark in output, drop at the device: the send should not error.
        devs = ", ".join(f'"{d}"' for d in sorted(os.listdir("/sys/class/net")) if d != "lo")
        nft(
            f'table inet {TABLE} {{\n'
            f'  chain c {{ type filter hook output priority filter; policy accept;\n'
            f'    socket cgroupv2 level {level} "{cgroup}" meta mark set {MARK:#x}\n'
            f'  }}\n}}\n'
            f'table netdev {TABLE} {{\n'
            f'  chain c {{ type filter hook egress devices = {{ {devs} }} priority filter; policy accept;\n'
            f'    meta mark {MARK:#x} counter drop\n'
            f'  }}\n}}\n'
        )
        return
    nft(
        f'table inet {TABLE} {{\n'
        f'  chain c {{ type filter hook {hook} priority filter; policy accept;\n'
        f'    socket cgroupv2 level {level} "{cgroup}" counter drop\n'
        f'  }}\n}}\n'
    )


def unblock():
    for family in ("inet", "netdev"):
        subprocess.run(["sudo", "-n", "nft", "delete", "table", family, TABLE],
                       stderr=subprocess.DEVNULL)


def dropped():
    for family in ("netdev", "inet"):
        out = subprocess.run(["sudo", "-n", "nft", "list", "table", family, TABLE],
                             capture_output=True, text=True).stdout
        m = re.search(r"counter packets (\d+)", out)
        if m:
            return int(m.group(1))
    return -1


def drain(s):
    s.settimeout(0.3)
    try:
        while s.recv(65535):
            pass
    except OSError:
        pass


class Tcp:
    name = "tcp established"

    def __init__(self):
        self.s = socket.create_connection((HOST, 80), timeout=5)

    def ask(self, timeout):
        self.send_error = "-"
        try:
            self.s.sendall(HTTP)
        except OSError as e:
            self.send_error = errno.errorcode.get(e.errno, str(e.errno))
            return False
        return self.wait(timeout)

    def wait(self, timeout):
        self.s.settimeout(timeout)
        try:
            data = self.s.recv(65535)
        except OSError:
            return False
        if data:
            drain(self.s)
        return bool(data)

    def close(self):
        self.s.close()


class Udp:
    def __init__(self, connected):
        self.connected = connected
        self.name = "udp connected" if connected else "udp unconnected"
        self.s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        if connected:
            self.s.connect((HOST, 53))
        else:
            self.s.bind(("0.0.0.0", 0))

    def ask(self, timeout):
        drain(self.s)
        self.send_error = "-"
        try:
            self.s.sendto(DNS, (HOST, 53))
        except OSError as e:
            self.send_error = errno.errorcode.get(e.errno, str(e.errno))
            return False
        self.s.settimeout(timeout)
        try:
            return bool(self.s.recv(65535))
        except OSError:
            return False

    wait = ask

    def close(self):
        self.s.close()


def control():
    """Fresh traffic from an unrelated scope; must work while the rule is up."""
    t = Tcp()
    ok = t.ask(3)
    t.close()
    u = Udp(True)
    ok = u.ask(3) and ok
    u.close()
    print("online" if ok else "OFFLINE")


def run_control():
    script = os.path.abspath(__file__)
    r = subprocess.run(["systemd-run", "--user", "--scope", "--quiet", sys.executable, script, "--control"],
                       capture_output=True, text=True)
    return r.stdout.strip() or f"error: {r.stderr.strip()}"


def ping():
    r = subprocess.run(["ping", "-c1", "-W2", HOST], capture_output=True)
    return "replies" if r.returncode == 0 else "no reply"


def inner():
    cgroup = own_cgroup()
    log = open(RESULTS, "w")

    def say(text):
        print(text, flush=True)
        log.write(text + "\n")
        log.flush()

    say(f"kernel: {os.uname().release}")
    say(f"target cgroup: {cgroup}")
    say(f"{'hook':<11}{'traffic':<17}{'before':<8}{'frozen':<9}{'send':<8}{'after':<8}{'drops':<7}other apps")
    for hook in ("output", "egress", "input", "prerouting"):
        for make in (Tcp, lambda: Udp(True), lambda: Udp(False)):
            conn = make()
            before = conn.ask(4)
            try:
                block(hook, cgroup)
                frozen = conn.ask(3)
                send_error = conn.send_error
                drops = dropped()
                other = run_control()
            finally:
                unblock()
            # A TCP request sent while frozen is retransmitted; a reply here
            # proves the same connection survived the freeze.
            after = conn.wait(15) if isinstance(conn, Tcp) else conn.ask(4)
            conn.close()
            say(f"{hook:<11}{conn.name:<17}{'ok' if before else 'FAIL':<8}"
                f"{'LEAKED' if frozen else 'blocked':<9}{send_error:<8}{'ok' if after else 'FAIL':<8}{drops:<7}{other}")
        try:
            block(hook, cgroup)
            say(f"{hook:<11}ping: {ping()}")
        finally:
            unblock()
    say("done")


if __name__ == "__main__":
    if "--control" in sys.argv:
        control()
    elif "--inner" in sys.argv:
        try:
            inner()
        finally:
            unblock()
    else:
        subprocess.run(["sudo", "-v"], check=True)
        unit = f"lagswitch-probe-{os.getpid()}"
        os.execvp("systemd-run", ["systemd-run", "--user", "--scope", "--quiet", f"--unit={unit}",
                                  sys.executable, os.path.abspath(__file__), "--inner"])
