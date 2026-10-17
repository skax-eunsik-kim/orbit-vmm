#!/usr/bin/env python3
"""orbit-vmm health check, meant to run unattended for long periods.

Each run exercises the things developers rely on (Docker answers, containers
start, published ports reach the Mac, bind-mount edits and inotify events
arrive, the guest clock is right) and records VMM resource use (memory, threads,
fds, disk) to <instance dir>/health.jsonl.

Adaptive schedule: the job may be invoked often (launchd every 15 min); it only
does work when due. Interval starts at 1 h, doubles to 2 h and then 4 h after 3
healthy runs in a row, and drops back to 1 h on any failure or warning
(including resource growth: threads/fds far above the first run's baseline).

Usage: healthcheck.py [--force] [--instance NAME]
Exit status: 0 healthy or not due, 1 unhealthy.
"""
import argparse
import json
import os
import random
import shutil
import socket
import subprocess
import sys
import tempfile
import time

INTERVALS_H = [1, 2, 4]
STREAK_TO_GROW = 3
# Scratch directory for the bind-mount probe; must be inside a shared (allowed)
# directory. Defaults to the checkout this script lives in.
CHECK_DIR = os.environ.get("ORBIT_HEALTH_DIR") or os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), ".orbit-health")


def sh(cmd, timeout=60, env=None):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env)
        return r.returncode, (r.stdout + r.stderr).strip()
    except subprocess.TimeoutExpired:
        return 124, f"timeout after {timeout}s"


def ask(sock_path, line, timeout=3):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(timeout)
    s.connect(sock_path)
    s.sendall(line.encode())
    out = b""
    while True:
        chunk = s.recv(4096)
        if not chunk:
            break
        out += chunk
    s.close()
    return out.decode()


def free_port():
    for _ in range(50):
        p = random.randint(30000, 39999)
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", p)) != 0:
                return p
    return 39999


class Run:
    def __init__(self, inst_dir):
        self.dir = inst_dir
        self.env = dict(os.environ, DOCKER_HOST=f"unix://{inst_dir}/docker.sock")
        self.checks = {}
        self.metrics = {}

    def docker(self, *args, timeout=60):
        return sh(["docker", *args], timeout=timeout, env=self.env)

    def check(self, name, fn):
        t0 = time.time()
        try:
            ok, detail = fn()
        except Exception as e:  # a crashing check is a failed check
            ok, detail = False, f"{type(e).__name__}: {e}"
        self.checks[name] = {"ok": bool(ok), "secs": round(time.time() - t0, 2), "detail": str(detail)[:200]}

    # ---- checks -------------------------------------------------------------

    def vm_alive(self):
        pid = open(f"{self.dir}/vmm.pid").read().strip()
        os.kill(int(pid), 0)
        self.pid = int(pid)
        return True, pid

    def docker_ping(self):
        reply = ask(f"{self.dir}/docker.sock", "GET /_ping HTTP/1.0\r\n\r\n", timeout=5)
        return reply.startswith("HTTP/1.0 200") or reply.startswith("HTTP/1.1 200"), reply.splitlines()[0] if reply else "no reply"

    def container_run(self):
        rc, out = self.docker("run", "--rm", "--label", "orbit-health=1", "alpine:3.22", "true", timeout=60)
        return rc == 0, out

    def port_publish(self):
        port = free_port()
        name = f"orbit-health-port-{port}"
        rc, out = self.docker("run", "-d", "--rm", "--name", name, "--label", "orbit-health=1", "-p", f"{port}:8080",
                              "alpine:3.22", "sh", "-c", "while true; do echo ok | nc -l -p 8080; done")
        if rc != 0:
            return False, out
        try:
            deadline = time.time() + 15
            while time.time() < deadline:
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=2) as s:
                        if s.recv(16).startswith(b"ok"):
                            return True, f"localhost:{port}"
                except OSError:
                    pass
                time.sleep(0.5)
            return False, f"localhost:{port} not reachable in 15s"
        finally:
            self.docker("rm", "-f", name)

    def bind_edit_and_inotify(self):
        os.makedirs(CHECK_DIR, exist_ok=True)
        work = tempfile.mkdtemp(dir=CHECK_DIR)
        try:
            with open(f"{work}/f", "w") as f:
                f.write("v1\n")
            name = f"orbit-health-fs-{os.getpid()}"
            rc, out = self.docker("run", "-d", "--rm", "--name", name, "--label", "orbit-health=1", "-v", f"{work}:/w",
                                  "alpine:3.22", "sh", "-c",
                                  "inotifyd - /w:w > /w/events 2>&1 & while true; do cat /w/f > /w/seen; sleep 0.1; done")
            if rc != 0:
                return False, out
            try:
                time.sleep(2)
                t0 = time.time()
                with open(f"{work}/f", "w") as f:
                    f.write("v2\n")
                seen = event = None
                while time.time() - t0 < 10 and (seen is None or event is None):
                    if seen is None and "v2" in open(f"{work}/seen").read():
                        seen = time.time() - t0
                    if event is None and os.path.exists(f"{work}/events") and "\tf" in open(f"{work}/events").read():
                        event = time.time() - t0
                    time.sleep(0.05)
                self.metrics["edit_visible_s"] = None if seen is None else round(seen, 2)
                self.metrics["inotify_event_s"] = None if event is None else round(event, 2)
                return seen is not None and event is not None, f"visible={seen} event={event}"
            finally:
                self.docker("rm", "-f", name)
        finally:
            shutil.rmtree(work, ignore_errors=True)

    def clock(self):
        t0 = time.time()
        reply = ask(f"{self.dir}/control.sock", "now\n")
        t1 = time.time()
        parts = reply.split()
        if len(parts) != 2 or not all(p.isdigit() for p in parts):
            return True, f"guest has no 'now' command (older guest): {reply.strip()[:40]}"
        offset = int(parts[0]) + int(parts[1]) / 1e6 - (t0 + t1) / 2
        self.metrics["clock_offset_ms"] = round(offset * 1000)
        return abs(offset) < 1.0, f"{offset * 1000:+.0f} ms"

    # ---- resource metrics ----------------------------------------------------

    def collect(self):
        pid = getattr(self, "pid", None)
        if pid:
            rc, out = sh(["footprint", "-p", str(pid)], timeout=20)
            for line in out.splitlines():
                if "phys_footprint:" in line:
                    v, unit = line.split()[1:3]
                    self.metrics["footprint_mib"] = round(float(v) * {"KB": 1 / 1024, "MB": 1, "GB": 1024}.get(unit, 1))
                    break
            rc, out = sh(["ps", "-M", "-p", str(pid)], timeout=10)
            self.metrics["threads"] = max(0, len(out.splitlines()) - 1)
            rc, out = sh(["lsof", "-p", str(pid)], timeout=20)
            self.metrics["fds"] = max(0, len(out.splitlines()) - 1)
        img = f"{self.dir}/data.img"
        if os.path.exists(img):
            st = os.stat(img)
            self.metrics["disk_used_mib"] = st.st_blocks * 512 // (1 << 20)
            self.metrics["disk_size_gib"] = st.st_size >> 30
        rc, out = self.docker("ps", "-q", timeout=20)
        self.metrics["containers"] = len(out.split()) if rc == 0 else None


def console_errors(inst_dir, state):
    """New guest/VMM error lines since the last run."""
    path = f"{inst_dir}/console.log"
    try:
        size = os.path.getsize(path)
    except OSError:
        return []
    start = state.get("console_offset", 0)
    if start > size:  # the log was recreated by a VM restart
        start = 0
    with open(path, "rb") as f:
        f.seek(start)
        new = f.read().decode(errors="replace")
    state["console_offset"] = size
    bad = ("panic", "Kernel panic", "Oops", "BUG:", "Out of memory", "EXT4-fs error", "orbit-vmm: ", "segfault")
    return [l.strip()[:160] for l in new.splitlines() if any(b in l for b in bad) and "clock was off" not in l][:20]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--instance", default="")
    ap.add_argument("--record", help="append the result to this file and leave the schedule alone (for soak tests)")
    a = ap.parse_args()
    inst_dir = os.path.expanduser("~/.orbit/vmm" + (f"-{a.instance}" if a.instance else ""))
    state_path = f"{inst_dir}/health-state.json"
    try:
        state = json.load(open(state_path))
    except (OSError, ValueError):
        state = {"interval_h": 1, "streak": 0, "next_due": 0}
    now = time.time()
    if a.record:
        a.force = True
    if not a.force and now < state.get("next_due", 0):
        return 0

    r = Run(inst_dir)
    r.check("vm_alive", r.vm_alive)
    if r.checks["vm_alive"]["ok"]:
        r.check("docker_ping", r.docker_ping)
        r.check("container_run", r.container_run)
        r.check("port_publish", r.port_publish)
        r.check("bind_edit_inotify", r.bind_edit_and_inotify)
        r.check("clock", r.clock)
    r.collect()
    errors = console_errors(inst_dir, state)

    warnings = []
    base = state.setdefault("baseline", {})
    for k, slack in (("threads", 20), ("fds", 50)):
        v = r.metrics.get(k)
        if v is None:
            continue
        base.setdefault(k, v)
        if v > 2 * base[k] + slack:
            warnings.append(f"{k} {v} vs baseline {base[k]}")
    if errors:
        warnings.append(f"{len(errors)} new error line(s) in console.log")

    healthy = all(c["ok"] for c in r.checks.values()) and not warnings
    steps = INTERVALS_H
    if healthy:
        state["streak"] = state.get("streak", 0) + 1
        if state["streak"] >= STREAK_TO_GROW and state["interval_h"] < steps[-1]:
            state["interval_h"] = steps[min(steps.index(state["interval_h"]) + 1, len(steps) - 1)]
            state["streak"] = 0
    else:
        state["interval_h"], state["streak"] = steps[0], 0
    state["next_due"] = now + state["interval_h"] * 3600 - 60

    record = {
        "time": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "healthy": healthy,
        "checks": r.checks,
        "metrics": r.metrics,
        "warnings": warnings,
        "console_errors": errors,
        "next_interval_h": state["interval_h"],
    }
    with open(a.record or f"{inst_dir}/health.jsonl", "a") as f:
        f.write(json.dumps(record, ensure_ascii=False) + "\n")
    if not a.record:
        tmp = state_path + ".tmp"
        json.dump(state, open(tmp, "w"))
        os.replace(tmp, state_path)

    failed = [k for k, c in r.checks.items() if not c["ok"]]
    print(f"{'HEALTHY' if healthy else 'UNHEALTHY'}  failed={failed or '-'} warnings={warnings or '-'} "
          f"metrics={r.metrics} next check in {state['interval_h']}h")
    return 0 if healthy else 1


if __name__ == "__main__":
    sys.exit(main())
