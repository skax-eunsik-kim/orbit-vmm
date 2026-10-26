#!/usr/bin/env python3
"""Prints, as PEM, the certificates this Mac trusts as TLS roots beyond the
built-in Apple set: the ones an admin or the user marked "Always Trust"
(typically a corporate TLS-inspection CA). These are what containers need to
reach the internet through such a proxy.

Certificates that are merely present in a keychain (intermediates, device or
802.1X certificates, ones set to "Never Trust"), ones trusted only for other
purposes (Wi-Fi/EAP, S/MIME, code signing), and anything that isn't a CA
certificate are left out.
"""
import base64
import hashlib
import os
import plistlib
import re
import subprocess
import sys
import tempfile

TRUST_ROOT, TRUST_AS_ROOT, DENY = 1, 2, 3
# DER body of the SSL policy OID (1.2.840.113635.100.1.3).
SSL_POLICY_OID = bytes.fromhex("2a864886f763640103")
KEYCHAINS = [
    "/Library/Keychains/System.keychain",
    os.path.expanduser("~/Library/Keychains/login.keychain-db"),
]


def trusted_sha1s():
    """SHA-1s with root trust in the admin or user trust-settings domain."""
    trusted, denied = set(), set()
    with tempfile.TemporaryDirectory() as tmp:
        for args in (["-d"], []):  # admin domain, user domain
            out = os.path.join(tmp, "settings.plist")
            r = subprocess.run(["security", "trust-settings-export", *args, out], capture_output=True)
            if r.returncode != 0 or not os.path.exists(out):
                continue
            with open(out, "rb") as f:
                trust_list = plistlib.load(f).get("trustList", {})
            os.remove(out)
            for sha1, entry in trust_list.items():
                settings = entry.get("trustSettings", [])
                # Only settings that apply to TLS: no policy (= every policy) or the SSL policy.
                tls = [s for s in settings if s.get("kSecTrustSettingsPolicy", SSL_POLICY_OID) == SSL_POLICY_OID]
                results = {s.get("kSecTrustSettingsResult", TRUST_ROOT) for s in tls}
                if DENY in results:
                    denied.add(sha1.upper())
                # An empty settings array means "trust as root for everything".
                elif not settings or results & {TRUST_ROOT, TRUST_AS_ROOT}:
                    trusted.add(sha1.upper())
    return trusted - denied


def certs_by_sha1():
    """Keychain certificates keyed by the SHA-1 of their own DER bytes."""
    found = {}
    for kc in KEYCHAINS:
        if not os.path.exists(kc):
            continue
        r = subprocess.run(["security", "find-certificate", "-a", "-p", kc], capture_output=True, text=True)
        for pem in re.findall(r"-----BEGIN CERTIFICATE-----\n.*?-----END CERTIFICATE-----\n", r.stdout, re.S):
            body = "".join(pem.splitlines()[1:-1])
            found.setdefault(hashlib.sha1(base64.b64decode(body)).hexdigest().upper(), pem)
    return found


def is_ca(pem):
    r = subprocess.run(["openssl", "x509", "-noout", "-text"], input=pem, capture_output=True, text=True)
    return r.returncode == 0 and re.search(r"Basic Constraints:.*\n\s*CA:TRUE", r.stdout) is not None


def main():
    trusted = trusted_sha1s()
    certs = certs_by_sha1()
    picked = [certs[s] for s in sorted(trusted) if s in certs and is_ca(certs[s])]
    sys.stdout.write("".join(picked))
    print(f"exported {len(picked)} trusted root(s) of {len(certs)} keychain certificate(s)", file=sys.stderr)


if __name__ == "__main__":
    main()
