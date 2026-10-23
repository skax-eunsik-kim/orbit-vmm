# बेंचमार्क

[English](../../benchmarks.md) | [한국어](../ko/benchmarks.md) | [日本語](../ja/benchmarks.md) | [简体中文](../zh-CN/benchmarks.md) | [Español](../es/benchmarks.md) | [Português (Brasil)](../pt-BR/benchmarks.md) | [Français](../fr/benchmarks.md) | [Deutsch](../de/benchmarks.md) | **हिन्दी**

> यह एक अनुवाद है। अंग्रेज़ी दस्तावेज़ ([benchmarks.md](../../benchmarks.md)) ही source of truth है; अगर दोनों में अंतर हो तो अंग्रेज़ी दस्तावेज़ को ही मानें।

सभी आँकड़े October 2026 में एक M-series MacBook (macOS 26) से, `bench/` में
मौजूद scripts का इस्तेमाल करके लिए गए हैं। ये orders of magnitude दिखाते हैं, सटीक नतीजे नहीं।
अपनी machine पर तुलना करने के लिए scripts खुद चलाएँ।

## macOS को लौटाई गई मेमोरी

`bench/memtest.sh` VM process का `phys_footprint` चार बिंदुओं पर मापता है:
idle, एक ऐसे container के दौरान जो लगभग 3.5 GB dirty करता है (page cache और anonymous
memory), और container के exit होने के 30 s और 90 s बाद। Table में idle और 90 s
बाद के आँकड़े दिखाए गए हैं।

| Engine | Idle | Exit के 90 s बाद |
|---|---|---|
| orbit-vmm | 0.66 GB | 0.69 GB |
| OrbStack | 1.25 GB | 1.11 GB |
| Colima (QEMU/vz) | 1.59 GB | 3.61 GB |

`bench/memfrag.sh` fragmentation पर दबाव डालता है। यह 24 containers start करता है जिनमें से हर एक
150 MiB पकड़े रखता है, फिर हर दूसरे container को free करता है, फिर बाक़ी को free करता है:

| Step | Mac footprint | Guest used |
|---|---|---|
| शुरुआत | 849 MiB | 304 MiB |
| 24 × 150 MiB held | 4854 MiB | 4165 MiB |
| हर दूसरा free किया (+60 s) | 2917 MiB | 2206 MiB |
| सभी free किए (+60 s) | 1043 MiB | 334 MiB |

## डेवलपर वर्कफ़्लो

`bench/devflow/devflow.sh` 25 checks चलाता है:

- healthchecks के साथ compose up, और Mac से published ports
- service DNS और `host.docker.internal`
- named volumes, bind-mount visibility और inotify, bind mount पर postgres `initdb`,
  `chown`
- TLS proxy के पीछे HTTPS और एक amd64 image
- एक BuildKit build (`# syntax`, ghcr, apt/uv cache mounts) और एक alembic
  migration
- logs, exec, stats और clock skew

मौजूदा नतीजा: 25/25 pass।

## स्थिरता

`bench/stress/multi.sh N` एक VM में एक ही समय पर N worktree stacks चलाता है
और जाँचता है कि एक worktree के edits कभी दूसरे तक न पहुँचें। `bench/stress/soak.sh <hours>`
इसे container और file churn के साथ एक loop में दोहराता है, और
VMM के threads, fds और memory का sample लेता है।

एक 4 h soak ने 295 में से 295 rounds pass किए, और thread और fd की गिनती स्थिर रही।
