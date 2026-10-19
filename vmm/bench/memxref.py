"""Which guest pages is the Mac still holding?

Start the VM with ORBIT_MEMDUMP=<prefix>, then at the same moment:
  touch <prefix>.req                                   # VMM writes <prefix>.bin
  docker run --rm --privileged -v "$PWD:/out" alpine sh -c \
    'dd if=/proc/kpageflags of=/out/kpf.bin bs=8 skip=$((0x80000)) count=$((<MiB>*256))'
and run: python3 -I memxref.py <prefix>.bin kpf.bin

<prefix>.bin has one mincore(2) byte per 16 KiB host page of guest RAM;
kpf.bin has the guest's 8-byte page flags per 4 KiB page from RAM_BASE.
Free buddy blocks only flag their head page, so a run of flagless pages after
a BUDDY page is counted as free (an estimate).
"""

import collections
import struct
import sys

INCORE, PAGED_OUT = 0x1, 0x20
BUDDY, LRU, SLAB, ANON, NOPAGE, PGTABLE, RESERVED = 1 << 10, 1 << 5, 1 << 7, 1 << 12, 1 << 20, 1 << 26, 1 << 32
MIB = 1 << 20


def kind(f: int, free: bool) -> str:
    if free:
        return "free(buddy)"
    for bit, name in ((NOPAGE, "nopage"), (ANON, "anon"), (LRU, "pagecache"), (SLAB, "slab"), (PGTABLE, "pagetable"), (RESERVED, "reserved")):
        if f & bit:
            return name
    return "noflags(kernel)" if f == 0 else "other"


def main() -> None:
    host = open(sys.argv[1], "rb").read()
    raw = open(sys.argv[2], "rb").read()
    flags = struct.unpack(f"<{len(raw) // 8}Q", raw)
    free, run = [], False
    for f in flags:
        run = True if f & BUDDY else (run if f == 0 else False)
        free.append(run)
    held: collections.Counter[tuple[str, str]] = collections.Counter()
    for hp, st in enumerate(host):
        kinds = [kind(flags[i], free[i]) for i in range(hp * 4, min(hp * 4 + 4, len(flags)))]
        if not kinds or not st & (INCORE | PAGED_OUT):
            continue
        k = collections.Counter(kinds).most_common(1)[0][0]
        held[(k, "incore" if st & INCORE else "paged out")] += 16384
    for (k, where), v in sorted(held.items(), key=lambda x: -x[1]):
        print(f"{k:<16} {where:<10} {v / MIB:8.0f} MiB")
    print(f"total held by the Mac: {sum(held.values()) / MIB:.0f} MiB")


if __name__ == "__main__":
    main()
