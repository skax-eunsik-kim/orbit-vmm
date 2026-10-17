"""Host side of the vsock test: push random bytes through the guest echo
server over the unix socket and verify they come back intact.
Usage: vsock-echo.py <socket path> <MiB> [connections]"""
import hashlib
import os
import socket
import sys
import threading
import time

path, mib = sys.argv[1], int(sys.argv[2])
conns = int(sys.argv[3]) if len(sys.argv) > 3 else 1


def one(results, i):
    data = os.urandom(mib << 20)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(path)
    got = bytearray()

    def reader():
        while True:
            chunk = s.recv(1 << 20)
            if not chunk:
                break
            got.extend(chunk)

    t = threading.Thread(target=reader)
    t.start()
    t0 = time.time()
    s.sendall(data)
    s.shutdown(socket.SHUT_WR)  # half-close: the echo must still come back
    t.join(timeout=60)
    dt = time.time() - t0
    ok = hashlib.md5(got).digest() == hashlib.md5(data).digest()
    results[i] = (ok, len(got), dt)
    s.close()


results = [None] * conns
threads = [threading.Thread(target=one, args=(results, i)) for i in range(conns)]
for t in threads:
    t.start()
for t in threads:
    t.join()
for i, (ok, n, dt) in enumerate(results):
    print(f"VSOCK conn{i}: {'OK' if ok else 'MISMATCH'} {n >> 20} MiB echoed in {dt:.2f}s ({2 * n / dt / 1e6:.1f} MB/s both ways)")
sys.exit(0 if all(r[0] for r in results) else 1)
