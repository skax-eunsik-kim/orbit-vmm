#!/usr/bin/env python3
"""Checks every relative link and in-page anchor in the repository's Markdown.

Anchors follow GitHub's rule: lowercase, keep letters, marks, numbers, "-",
"_" and spaces, spaces become "-". Usage: scripts/check-doc-links.py [ROOT]
"""
import re, sys, os, glob, unicodedata
def slug(t):
    t = re.sub(r"`|\*\*|\*", "", t.strip()).lower()
    t = "".join(c for c in t if c in "-_ " or unicodedata.category(c)[0] in "LMN")
    return t.replace(" ", "-")
root = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__))); bad = 0
files = [f for f in glob.glob(os.path.join(root, "**/*.md"), recursive=True) if "/target/" not in f and "/.claude/" not in f]
heads = {}
for f in files:
    s = re.sub(r"```.*?```", "", open(f, encoding="utf-8").read(), flags=re.S)
    heads[os.path.normpath(f)] = {slug(h) for h in re.findall(r"^#{1,6} (.+)$", s, flags=re.M)}
for f in sorted(files):
    s = re.sub(r"```.*?```", "", open(f, encoding="utf-8").read(), flags=re.S)
    for t in re.findall(r"\]\(([^)\s]+)\)", s):
        if re.match(r"^[a-z]+:", t): continue
        path, _, frag = t.partition("#")
        tgt = os.path.normpath(os.path.join(os.path.dirname(f), path)) if path else os.path.normpath(f)
        if not os.path.exists(tgt):
            print(f"BROKEN {os.path.relpath(f, root)} -> {t}"); bad += 1
        elif frag and tgt.endswith(".md") and frag not in heads.get(tgt, set()):
            print(f"ANCHOR {os.path.relpath(f, root)} -> {t}"); bad += 1
print(f"{len(files)} files, {bad} problems"); sys.exit(1 if bad else 0)
