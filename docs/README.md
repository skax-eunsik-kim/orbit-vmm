# docs

| Document | What it covers |
|---|---|
| [architecture.md](architecture.md) | How orbit-vmm works: the VMM, memory return, the data disk, file sharing, networking, the guest, and the scripts on the Mac. |
| [benchmarks.md](benchmarks.md) | Measured memory, developer-workflow and stability results, and how they were measured. |
| [i18n/](i18n) | Translations of the README and these documents. |

## Translations

English is the source of truth; each translation says so at the top.

| Language | README | Architecture | Benchmarks |
|---|---|---|---|
| 한국어 | [README](i18n/ko/README.md) | [architecture](i18n/ko/architecture.md) | [benchmarks](i18n/ko/benchmarks.md) |
| 日本語 | [README](i18n/ja/README.md) | [architecture](i18n/ja/architecture.md) | [benchmarks](i18n/ja/benchmarks.md) |
| 简体中文 | [README](i18n/zh-CN/README.md) | [architecture](i18n/zh-CN/architecture.md) | [benchmarks](i18n/zh-CN/benchmarks.md) |
| Español | [README](i18n/es/README.md) | [architecture](i18n/es/architecture.md) | [benchmarks](i18n/es/benchmarks.md) |
| Português (Brasil) | [README](i18n/pt-BR/README.md) | [architecture](i18n/pt-BR/architecture.md) | [benchmarks](i18n/pt-BR/benchmarks.md) |
| Français | [README](i18n/fr/README.md) | [architecture](i18n/fr/architecture.md) | [benchmarks](i18n/fr/benchmarks.md) |
| Deutsch | [README](i18n/de/README.md) | [architecture](i18n/de/architecture.md) | [benchmarks](i18n/de/benchmarks.md) |
| हिन्दी | [README](i18n/hi/README.md) | [architecture](i18n/hi/architecture.md) | [benchmarks](i18n/hi/benchmarks.md) |

`scripts/readme-langs.py` writes the language selector at the top of every
document, and CI warns when a change to an English document leaves a
translation behind. Translation rules are in
[CONTRIBUTING.md](../CONTRIBUTING.md#readme-translations).
