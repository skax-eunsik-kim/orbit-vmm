#!/usr/bin/env python3
"""Keeps the language selector line the same in every README, and checks that
translations keep up with README.md (the source of truth).

  scripts/readme-langs.py            rewrite the selector in every README
  scripts/readme-langs.py --check    fail if a selector is missing or differs
  scripts/readme-langs.py --stale BASE
                                     warn (GitHub annotation) for each translation
                                     not changed since BASE while README.md was

A translation is README.<code>.md; add it to LANGS to list it. The selector is
the first line after the H1 title.
"""
import os
import subprocess
import sys

LANGS = [
    ("README.md", "English"),
    ("README.ko.md", "한국어"),
    ("README.ja.md", "日本語"),
    ("README.zh-CN.md", "简体中文"),
    ("README.es.md", "Español"),
    ("README.pt-BR.md", "Português (Brasil)"),
    ("README.fr.md", "Français"),
    ("README.de.md", "Deutsch"),
    ("README.hi.md", "हिन्दी"),
]
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def selector(current):
    return " | ".join(f"**{name}**" if f == current else f"[{name}]({f})" for f, name in LANGS)


def lines_of(f):
    with open(os.path.join(ROOT, f), encoding="utf-8") as fh:
        return fh.read().split("\n")


def selector_index(lines):
    """Index of the selector line: the first non-empty line after the H1."""
    h1 = next(i for i, line in enumerate(lines) if line.startswith("# "))
    return next(i for i in range(h1 + 1, len(lines)) if lines[i].strip())


def main(argv):
    if argv[:1] == ["--stale"]:
        base = argv[1]
        changed = set(subprocess.run(["git", "diff", "--name-only", f"{base}...HEAD"], cwd=ROOT,
                                     capture_output=True, text=True, check=True).stdout.split())
        if "README.md" in changed:
            for f, name in LANGS[1:]:
                if f not in changed:
                    print(f"::warning file={f}::README.md changed but the {name} translation did not; "
                          "update it or note in the PR that it lags")
        return 0
    check = argv[:1] == ["--check"]
    bad = []
    for f, _ in LANGS:
        lines = lines_of(f)
        i = selector_index(lines)
        want = selector(f)
        if lines[i] == want:
            continue
        if check:
            bad.append(f)
            continue
        if lines[i].startswith(("<!-- LANG-SELECTOR -->", "**", "[")) and "|" in lines[i] or lines[i] == "<!-- LANG-SELECTOR -->":
            lines[i] = want
        else:
            lines.insert(i, want)
            lines.insert(i + 1, "")
        with open(os.path.join(ROOT, f), "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines))
    if bad:
        print("language selector missing or out of date in: " + ", ".join(bad) + " (run scripts/readme-langs.py)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
