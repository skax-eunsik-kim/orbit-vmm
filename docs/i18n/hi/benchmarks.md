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

`bench/devflow/devflow.sh` 28 checks चलाता है:

- healthchecks के साथ compose up, और Mac से published ports
- service DNS और `host.docker.internal`
- named volumes, bind-mount visibility और inotify, bind mount पर postgres `initdb`,
  `chown`
- TLS proxy के पीछे HTTPS और एक amd64 image
- एक BuildKit build (`# syntax`, ghcr, apt/uv cache mounts) और एक alembic
  migration
- hostile build steps (symlinked या swapped CA directories, भरी हुई disk) के सामने
  guest का CA wrapper, और एक RHEL (UBI) build जिसका bundle एक symlink है
- logs, exec, stats और clock skew

मौजूदा नतीजा: 28/28 pass।

## Kubernetes

`bench/k8s/k8s.sh` `orbit k8s` (kind v0.33, Kubernetes v1.37, एक
node) की जाँच करता है और VMM का `phys_footprint` record करता है। macOS 26.4, M-series पर
एक isolated instance (8 GiB, 6 vCPUs) पर तीन runs, 2026-10-27:

| | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| `orbit k8s up` (node Ready) | 43 s | 45 s | 40 s |
| Cluster से पहले VM | 2019 MiB | 2797 MiB | 2300 MiB |
| Cluster idle (60 s) | 3267 MiB | 3757 MiB | 4036 MiB |
| 20 nginx pods | 4417 MiB | 5247 MiB | 4598 MiB |
| `orbit k8s down` के 60 s बाद | 2685 MiB | 2302 MiB | 2047 MiB |
| 1 → 20 pods Ready तक scale | 14 s | 5 s | 5 s |

"पहले" में पिछले runs का छोड़ा हुआ page cache शामिल है, इसलिए यह बदलता रहता है। हर 30 s पर
लिए गए एक अलग trace ने दिखाया कि cluster delete होने के 90 s बाद VM अपने pre-cluster size से
नीचे लौट आया (पहले 1686 MiB, बाद में 1468 MiB)।

macOS "warn" memory pressure के तहत (`ORBIT_PRESSURE=warn` से pinned), cluster idle
रहते हुए, हर एक एक minute:

| memd | Page cache refaulted | Disk reads + writes | Page cache held |
|---|---|---|---|
| #33 से पहले | 2.7 GB | 3.4 GB | 460–510 MiB |
| बाद में | 0.09–0.2 GB | 0.14–0.27 GB | लगभग 600 MiB |

मौजूदा नतीजा: 13/13 pass।

## स्थिरता

`bench/stress/multi.sh N` एक VM में एक ही समय पर N worktree stacks चलाता है
और जाँचता है कि एक worktree के edits कभी दूसरे तक न पहुँचें। `bench/stress/soak.sh <hours>`
इसे container और file churn के साथ एक loop में दोहराता है, और
VMM के threads, fds और memory का sample लेता है।

एक 4 h soak ने 295 में से 295 rounds pass किए, और thread और fd की गिनती स्थिर रही।
