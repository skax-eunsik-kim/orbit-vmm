#!/usr/bin/env python3
"""Keeps the language selector the same in every document and its
translations, and checks that translations keep up with the English source.

  scripts/readme-langs.py            rewrite the selectors
  scripts/readme-langs.py --check    fail if a translation is missing or a selector differs
  scripts/readme-langs.py --stale BASE
                                     warn (GitHub annotation) for each translation not
                                     changed since BASE while its English source was

English sources are listed in DOCS. A translation of `<dir>/<name>.md` lives
at `docs/i18n/<code>/<name>.md`; add a language to LANGS and run the script.
The selector is the first non-empty line after the H1 title.
"""
import os
import posixpath
import subprocess
import sys

LANGS = [
    ("en", "English"),
    ("ko", "한국어"),
    ("ja", "日本語"),
    ("zh-CN", "简体中文"),
    ("es", "Español"),
    ("pt-BR", "Português (Brasil)"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("hi", "हिन्दी"),
]
DOCS = ["README.md", "docs/architecture.md", "docs/benchmarks.md"]
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def path_of(doc, code):
    return doc if code == "en" else f"docs/i18n/{code}/{posixpath.basename(doc)}"


def selector(doc, current):
    here = posixpath.dirname(path_of(doc, current))
    return " | ".join(
        f"**{name}**" if code == current else f"[{name}]({posixpath.relpath(path_of(doc, code), here or '.')})"
        for code, name in LANGS
    )


def selector_index(lines):
    """Index of the selector line: the first non-empty line after the H1."""
    h1 = next(i for i, line in enumerate(lines) if line.startswith("# "))
    return next(i for i in range(h1 + 1, len(lines)) if lines[i].strip())


def is_selector(line):
    return line.strip() == "<!-- LANG-SELECTOR -->" or (" | " in line and "](" in line and "English" in line)


def main(argv):
    if argv[:1] == ["--stale"]:
        base = argv[1]
        changed = set(subprocess.run(["git", "diff", "--name-only", f"{base}...HEAD"], cwd=ROOT,
                                     capture_output=True, text=True, check=True).stdout.split())
        for doc in DOCS:
            if doc not in changed:
                continue
            for code, name in LANGS[1:]:
                f = path_of(doc, code)
                if f not in changed:
                    print(f"::warning file={f}::{doc} changed but the {name} translation did not; "
                          "update it or note in the PR that it lags")
        return 0
    check = argv[:1] == ["--check"]
    bad = []
    for doc in DOCS:
        for code, _ in LANGS:
            f = path_of(doc, code)
            full = os.path.join(ROOT, f)
            if not os.path.exists(full):
                bad.append(f"{f} (missing)")
                continue
            with open(full, encoding="utf-8") as fh:
                lines = fh.read().split("\n")
            i = selector_index(lines)
            want = selector(doc, code)
            if lines[i] == want:
                continue
            if check:
                bad.append(f)
                continue
            if is_selector(lines[i]):
                lines[i] = want
            else:
                lines[i:i] = [want, ""]
            with open(full, "w", encoding="utf-8") as fh:
                fh.write("\n".join(lines))
    if bad:
        print("translations missing or language selector out of date: " + ", ".join(bad)
              + " (run scripts/readme-langs.py)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
