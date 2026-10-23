#!/usr/bin/env python3
"""orbit-vmm health check, meant to run unattended for long periods.

Each run exercises the things developers rely on (Docker answers, containers
start, published ports reach the Mac, bind-mount edits and inotify events
arrive, the guest clock is right, the Mac's disk has room for the data disk)
and records VMM resource use (memory, threads, fds, disk) to
<instance dir>/health.jsonl.

Adaptive schedule: the job may be invoked often (launchd every 15 min); it only
does work when due. Interval starts at 1 h, doubles to 2 h and then 4 h after 3
healthy runs in a row, and drops back to 1 h on any failure or warning
(including resource growth: threads/fds far above the first run's baseline).

Once a day it also looks up the latest GitHub release of the installed
repository (~/.orbit/bin/VERSION, written by install.sh) and shows one macOS
notification per newer release. It never installs anything: make update does.

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
import secrets
import time

INTERVALS_H = [1, 2, 4]
UPDATE_EVERY_S = 24 * 3600
STREAK_TO_GROW = 3
# The bind-mount probe needs a scratch directory inside a shared directory:
# ORBIT_HEALTH_DIR's parent, or by default the first shared directory the VM
# was started with (vmm-up.sh records them in <instance dir>/shares).
SCRATCH = ".orbit-health"
MAX_REPLY = 64 << 10
MIB = 1 << 20
# Same defaults as the VMM (vmm/src/virtio/blk.rs): it warns below WARN and
# holds guest writes below FLOOR.
DISK_FLOOR = int(os.environ.get("ORBIT_DISK_FLOOR_MIB", 1024)) * MIB
DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC


def disk_image(inst_dir):
    try:
        with open(f"{inst_dir}/disk") as f:
            return f.readline().strip() or f"{inst_dir}/data.img"
    except OSError:
        return f"{inst_dir}/data.img"


def disk_warn(total):
    if os.environ.get("ORBIT_DISK_WARN_MIB"):
        return int(os.environ["ORBIT_DISK_WARN_MIB"]) * MIB
    return max(min(5 << 30, total // 50), 2 * DISK_FLOOR)


def shared_parent(inst_dir):
    if os.environ.get("ORBIT_HEALTH_DIR"):
        return os.path.dirname(os.path.abspath(os.environ["ORBIT_HEALTH_DIR"]))
    try:
        with open(f"{inst_dir}/shares") as f:
            return f.readline().strip() or None
    except OSError:
        return None


# Containers can rename and symlink anything inside the scratch directory, so
# every host-side access goes through a directory fd with O_NOFOLLOW: a planted
# symlink makes the probe fail instead of writing outside the share.
def write_at(dir_fd, name, data):
    fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, 0o600, dir_fd=dir_fd)
    with os.fdopen(fd, "w") as f:
        f.write(data)


def read_at(dir_fd, name):
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=dir_fd)
    except OSError:
        return ""
    with os.fdopen(fd, "rb") as f:
        return f.read(MAX_REPLY).decode(errors="replace")


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
    deadline = time.monotonic() + timeout  # overall, not per read: the guest is untrusted
    while len(out) <= MAX_REPLY:
        left = deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError("control socket reply took too long")
        s.settimeout(left)
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
        parent = shared_parent(self.dir)
        if not parent:
            return False, "no shared directory recorded; restart the VM with vmm-up.sh"
        # The shared directory itself can't be replaced from the guest (the VMM
        # refuses to rename or remove allowed directories, and vmm-up.sh records
        # their real paths); O_NOFOLLOW makes sure of the last step anyway.
        pfd = os.open(parent, DIR_FLAGS)
        try:
            try:
                os.mkdir(SCRATCH, 0o700, dir_fd=pfd)
            except FileExistsError:
                pass
            hfd = os.open(SCRATCH, DIR_FLAGS, dir_fd=pfd)
        finally:
            os.close(pfd)
        run = f"run-{os.getpid()}-{secrets.token_hex(4)}"
        try:
            os.mkdir(run, 0o700, dir_fd=hfd)
            wfd = os.open(run, DIR_FLAGS, dir_fd=hfd)
            try:
                return self._probe(wfd, os.path.join(parent, SCRATCH, run))
            finally:
                os.close(wfd)
        finally:
            shutil.rmtree(run, dir_fd=hfd, ignore_errors=True)
            os.close(hfd)

    def _probe(self, wfd, work):
        write_at(wfd, "f", "v1\n")
        name = f"orbit-health-fs-{os.getpid()}"
        rc, out = self.docker("run", "-d", "--rm", "--name", name, "--label", "orbit-health=1", "-v", f"{work}:/w",
                              "alpine:3.22", "sh", "-c",
                              "inotifyd - /w:w > /w/events 2>&1 & while true; do cat /w/f > /w/seen; sleep 0.1; done")
        if rc != 0:
            return False, out
        try:
            time.sleep(2)
            t0 = time.time()
            try:
                write_at(wfd, "f", "v2\n")
            except OSError as e:
                return False, f"probe file replaced from the guest: {e}"
            seen = event = None
            while time.time() - t0 < 10 and (seen is None or event is None):
                if seen is None and "v2" in read_at(wfd, "seen"):
                    seen = time.time() - t0
                if event is None and "\tf" in read_at(wfd, "events"):
                    event = time.time() - t0
                time.sleep(0.05)
            self.metrics["edit_visible_s"] = None if seen is None else round(seen, 2)
            self.metrics["inotify_event_s"] = None if event is None else round(event, 2)
            return seen is not None and event is not None, f"visible={seen} event={event}"
        finally:
            self.docker("rm", "-f", name)

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

    def disk_free(self):
        img = disk_image(self.dir)
        st = os.statvfs(os.path.dirname(os.path.abspath(img)))
        free, total = st.f_bavail * st.f_frsize, st.f_blocks * st.f_frsize
        self.metrics["mac_disk_free_mib"] = free // MIB
        detail = f"{free // MIB} MiB free on the Mac's disk"
        if free < DISK_FLOOR:
            return False, detail + f" (below {DISK_FLOOR // MIB} MiB: guest writes are held)"
        if free < disk_warn(total):
            self.disk_low = detail
        return True, detail

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
        img = disk_image(self.dir)
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
    # Not bare "panic" (the kernel command line has panic=-1) or every
    # "orbit-vmm: " line (most are informational: forwarding, balloon, clock).
    bad = ("Kernel panic", "panicked at", "Oops", "BUG:", "Out of memory", "EXT4-fs error", "EXT4-fs warning",
           "potential data loss", "I/O error", "Read-only file system", "segfault",
           "orbit-vmm: DISK FULL", "orbit-vmm: disk space low", "orbit-vmm: disk write failed", "orbit-vmm: disk flush failed")
    return [l.strip()[:160] for l in new.splitlines() if any(b in l for b in bad)][:20]


def semver(v):
    """(major, minor, patch) of a tag or `git describe` output, (0, 0, 0) if none."""
    import re
    m = re.match(r"v(\d+)\.(\d+)\.(\d+)", v or "")
    return tuple(int(x) for x in m.groups()) if m else (0, 0, 0)


def notify(text):
    # Text goes in as an argument, never into the AppleScript source.
    subprocess.run(["osascript", "-e", "on run argv", "-e",
                    "display notification (item 1 of argv) with title (item 2 of argv)", "-e", "end run",
                    text, "orbit-vmm"], capture_output=True, timeout=10)


def update_notice(state, now):
    """Notifies once per release newer than the installed one. Returns True if state changed."""
    if now - state.get("update_checked", 0) < UPDATE_EVERY_S:
        return False
    state["update_checked"] = now
    info = {}
    try:
        for line in open(os.path.expanduser(os.environ.get("ORBIT_BIN", "~/.orbit/bin")) + "/VERSION"):
            k, _, v = line.strip().partition("=")
            info[k] = v
    except OSError:
        return True
    # Not the REST API: unauthenticated it allows 60 calls an hour per IP, which
    # a company's shared egress address uses up (seen: 403, 0 remaining). The
    # web page redirects to the latest release's tag with no such limit.
    url = os.environ.get("ORBIT_RELEASES_URL") or (
        f"https://github.com/{info['repo']}/releases/latest" if info.get("repo") else "")
    if not url:
        return True
    # /usr/bin/curl uses the macOS trust store, so it works behind TLS inspection.
    out = subprocess.run(["/usr/bin/curl", "-sS", "-o", "/dev/null", "--max-time", "10",
                          "-w", "%{redirect_url}", url], capture_output=True, text=True)
    latest = out.stdout.strip().rsplit("/tag/", 1)[-1] if "/tag/" in out.stdout else ""
    if semver(latest) > semver(info.get("version")) and state.get("update_notified") != latest:
        notify(f"{latest} is available (installed: {info.get('version', '?')}). Run make update.")
        state["update_notified"] = latest
    return True


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
    if not a.record and not a.instance and update_notice(state, now):
        tmp = state_path + ".tmp"
        json.dump(state, open(tmp, "w"))
        os.replace(tmp, state_path)
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
    r.check("disk_free", r.disk_free)
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
    if getattr(r, "disk_low", None):
        warnings.append(f"disk space low: {r.disk_low}")
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
