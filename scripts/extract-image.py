"""Turn Alpine's vmlinuz-virt into a raw arm64 Image.

Handles the three shapes it has shipped as: raw Image, gzip, and EFI zboot
(PE header "MZ" + "zimg" magic wrapping a compressed payload).
"""
import gzip
import struct
import sys

ARM64_MAGIC = b"ARM\x64"  # at offset 0x38 of a raw Image


def unpack(data: bytes) -> bytes:
    if data[0x38:0x3C] == ARM64_MAGIC:
        return data
    if data[:2] == b"\x1f\x8b":
        return unpack(gzip.decompress(data))
    if data[:2] == b"MZ" and data[4:8] == b"zimg":
        off, size = struct.unpack_from("<II", data, 8)
        comp = data[24:32].rstrip(b"\0").decode()
        payload = data[off : off + size]
        if comp == "gzip":
            return unpack(gzip.decompress(payload))
        if comp == "zstd":
            import subprocess

            return unpack(subprocess.run(["zstd", "-dc"], input=payload, capture_output=True, check=True).stdout)
        sys.exit(f"unsupported zboot compression: {comp}")
    sys.exit("unrecognized kernel format")


src, dst = sys.argv[1], sys.argv[2]
with open(src, "rb") as f:
    image = unpack(f.read())
with open(dst, "wb") as f:
    f.write(image)
print(f"    kernel: {len(image) // 1024} KiB raw Image")
