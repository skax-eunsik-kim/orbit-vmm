# Arquitetura

[English](../../architecture.md) | [한국어](../ko/architecture.md) | [日本語](../ja/architecture.md) | [简体中文](../zh-CN/architecture.md) | [Español](../es/architecture.md) | **Português (Brasil)** | [Français](../fr/architecture.md) | [Deutsch](../de/architecture.md) | [हिन्दी](../hi/architecture.md)

> Esta é uma tradução. O documento em inglês ([architecture.md](../../architecture.md)) é a fonte da verdade; se os dois divergirem, siga o original em inglês.

```
 macOS                                          Linux guest (Alpine)
 ─────                                          ────────────────────
 docker CLI ── docker.sock ─┐                ┌─ socat ── dockerd / containerd
 vmm-up.sh  ── control.sock ┤   vsock        ├─ control.sh  (ports, clock, pressure, shutdown)
 FSEvents   ── fsnotify     ┘ ─────────────  └─ fsnotify.sh (Mac edits → inotify)
                         orbit-vmm
   vCPU threads ─ Hypervisor.framework ─ in-kernel GICv3, PSCI, PL011
   virtio-mmio:  blk  ─ data.img (ext4, flock'd: one VM per disk; discard)
                 net  ─ libslirp (NAT, DNS, runtime port forwards)
                 vsock ─ Unix sockets on the Mac
                 fs   ─ FUSE passthrough of allowed dirs under /Users
                 balloon ─ free page reporting → memory back to macOS
 launchd ── vmm-run.sh (supervisor: restart, disk guard, crash records)
         └─ healthcheck.py (hourly: checks, resource use, release notice)
```

## VMM (`vmm/`)

O VMM é um único processo Rust. `src/vm/` contém a máquina em si:

- FFI do Hypervisor.framework (`hvf.rs`) e o GIC no kernel (`gic.rs`).
- Boot e a device tree (`boot.rs`, `fdt.rs`).
- O console PL011 (`pl011.rs`).
- Threads de vCPU, PSCI e decodificação de MMIO (`vcpu.rs`).

`src/virtio/` contém o transporte virtio-mmio v2 e as split virtqueues, além de um
arquivo por dispositivo.

`signals.rs` trata SIGTERM e SIGINT enviados ao próprio VMM: registra o
pid e o nome do remetente em `console.log`, pede ao guest que desligue (no máximo 30 s)
e encerra como se tivesse sido morto por esse sinal, para que o supervisor ainda o reinicie.
Um segundo sinal encerra imediatamente.

### Devolução de memória

A RAM do guest é memória anônima obtida de `hv_vm_allocate`, mapeada na VM. Quando o guest
reporta páginas livres (`VIRTIO_BALLOON_F_REPORTING`, fila 2), o VMM:

1. desmapeia o intervalo da VM (`hv_vm_unmap`),
2. substitui-o por páginas zeradas novas (`mmap(MAP_FIXED)`), para que o macOS descarte
   as páginas antigas do footprint do processo,
3. mapeia o intervalo de volta (`hv_vm_map`).

Só `madvise` não reduz o footprint de memória que está mapeada em uma
VM.

O kernel do guest inicializa com `page_reporting.page_reporting_order=2`, então a
unidade de reporte é de 16 KiB, o tamanho de página do macOS. Mesmo memória livre fragmentada
é devolvida.

O Linux retém dados de arquivos como page cache, que o balloon não consegue reportar.
`guest/etc/orbit/memd.sh` cuida disso:

- Após 60 s de carga leve, ele recupera metade do page cache dos containers
  (cgroup `memory.reclaim`).
- Após 30 s totalmente ocioso, ele descarta os caches limpos.
- Nos dois casos, ele compacta a memória para que as páginas liberadas formem blocos reportáveis.
- O tempo todo, ele mantém o page cache abaixo de 1 GiB por meio do `memory.reclaim`
  do cgroup raiz (builds nunca ficam ociosos; o Mac já faz cache de `data.img` de qualquer forma).
- Enquanto o macOS reporta pressão de memória, ele mantém o page cache abaixo de 256 MiB
  (warn) ou 64 MiB (critical) em vez disso (`orbit.memd_warn_mb`,
  `orbit.memd_crit_mb`).

`pressure.rs` monitora a pressão de memória do macOS (uma fonte de memory pressure da
libdispatch, mais `kern.memorystatus_vm_pressure_level` relido a cada 30 s) e
envia `pressure normal|warn|critical` pelo vsock de controle. Quando o nível muda,
`control.sh` registra o nível em `/run/orbit-pressure` e executa
`memd.sh reclaim <level>` imediatamente: recupera até o novo limite (no critical,
descarta antes todos os caches limpos) e compacta, para que o free page reporting devolva as
páginas antes que o macOS precise comprimi-las. `ORBIT_PRESSURE=off` desativa esse
comportamento, e `ORBIT_PRESSURE=warn|critical` fixa o nível, para comparações. Isso
ajuda com cache que o guest tocou uma única vez (as entradas de um build, uma imagem baixada);
um working set que o guest relê continuamente permanece residente, qualquer que seja o limite.
O guest é informado de toda mudança; o `console.log` recebe no máximo uma linha a cada
10 minutos, com o número de mudanças ocorridas nesse intervalo.

A coluna Memória do Monitor de Atividade é o `phys_footprint` do processo. Quando o
Mac está com pouca memória, o macOS comprime ou faz swap de parte da RAM do guest, e o
footprint então cresce além das páginas do guest que o VMM de fato mantém (em cerca de
um terço a metade da quantidade comprimida, nas medições; fica perto de zero
quando nada está comprimido). Para ver quais páginas do guest estão retidas, inicie o VMM
com `ORBIT_MEMDUMP=<prefix>`, execute `touch <prefix>.req` e compare `<prefix>.bin`
(um byte de `mincore` por página de 16 KiB da RAM do guest) com o
`/proc/kpageflags` do guest usando `vmm/bench/memxref.py`.

### Disco de dados (virtio-blk)

`blk.rs` serve o `data.img`, um arquivo esparso, e obtém um `flock` sobre ele para que duas
VMs não possam compartilhar um mesmo disco.

- **Discard.** Os discards do guest viram `F_PUNCHHOLE` na imagem, e o `memd`
  executa `fstrim /data` após o boot e a cada 10 minutos quando a VM está tranquila, para que
  o espaço liberado pelo Docker volte para o Mac.
- **Proteção de disco.** A cada 2 s, o VMM verifica o espaço livre no Mac. Abaixo de
  `ORBIT_DISK_WARN_MIB`, ele registra um aviso. Abaixo de `ORBIT_DISK_FLOOR_MIB`, ou quando
  uma escrita falha com ENOSPC, ele retém as escritas do guest em vez de fazê-las falhar
  (como o `werror=enospc` do QEMU) por até `ORBIT_DISK_PAUSE_SECS`, e retoma assim que
  há espaço liberado, para que o ext4 nunca veja um erro de escrita. O supervisor para a
  VM de forma limpa antes disso, abaixo de `ORBIT_STOP_FREE_GB`.

### Compartilhamento de arquivos (virtio-fs)

`fs.rs` é um servidor FUSE e `hostfs.rs` cuida do acesso a caminhos.

- Todo caminho é percorrido com `openat(O_NOFOLLOW)` a partir de um fd de diretório, para que um
  symlink não consiga escapar dos diretórios permitidos (`--share-allow`). Diretórios
  acima de um permitido podem ser atravessados, mas não listados.
- O próprio VMM verifica a allowlist: as entradas devem ser caminhos reais (sem
  symlinks, com maiúsculas e minúsculas exatas, não aninhados), e o diretório home, tudo o que está acima
  dele, diretórios ocultos, `~/Library`, `~/bin` e `~/Applications` são recusados. O guest não pode renomear
  nem remover os próprios diretórios permitidos.
- Fds de diretório de percursos anteriores ficam em cache por 1 s, para que um caminho não
  reabra todos os diretórios desde `/Users`. As próprias renomeações e
  rmdirs do guest removem as entradas na hora, e entradas expiradas são fechadas na próxima
  inserção. Caminhos inexistentes ficam em cache no guest por 1 s
  (`ORBIT_FS_NEG_TTL_MS`).
- Há quatro filas de requisições.
- O TTL de entradas é 5 s e o TTL de atributos é 500 ms. `KEEP_CACHE` e
  `AUTO_INVAL_DATA` estão habilitados, assim como o readdirplus.
- O cache de writeback está desligado. Com ele ligado, edições feitas no Mac não ficavam visíveis no
  guest.
- O `chown` de containers é armazenado no xattr `user.orbit.owner` e reportado
  de volta no `stat`. Os bits de permissão de grupo/outros espelham os bits do dono, para que
  usuários não root de containers possam escrever nos seus arquivos.
- Edições no Mac chegam ao guest como eventos inotify. `fsevents.rs` monitora os
  diretórios permitidos com FSEvents; `forward_events` transforma cada lote em
  edições, criações, renomeações e exclusões (a partir do estado atual do Mac e
  do que o guest já consultou) e os envia para `fsnotify.sh` via vsock.
  O guest repete cada mudança sob um gid reservado: um `truncate` de mesmo tamanho
  para uma edição (`IN_MODIFY`), create/mkdir, rename ou unlink/rmdir para o
  restante. O servidor reconhece esse gid, nunca aplica nada disso ao Mac
  e só deixa a operação ter sucesso para caminhos que ele enfileirou enquanto o Mac ainda corresponde.
- O diretório do socket do docker (`~/.orbit/vmm/`) aparece para o guest como um
  ponto de montagem vazio e somente leitura, e o guest cria ali um link para `docker.sock`, para que
  ferramentas que fazem bind mount do caminho de `DOCKER_HOST` (o Ryuk do testcontainers) funcionem.

### Rede

`slirp.rs` executa a libslirp em uma thread própria.

- `ports.rs` encaminha as portas publicadas para o `127.0.0.1` do Mac. Ele acompanha
  os eventos de containers do Docker (`/events` no próprio socket do docker do VMM) e
  encaminha um publish `-p` assim que o container inicia, inclusive portas
  publicadas em um IP específico do host. A cada 250 ms, ele também lê os
  sockets em escuta do guest pelo canal de controle, para serviços com `--network host`.
- Os containers alcançam o localhost do Mac por meio de `host.docker.internal`
  (`ORBIT_HOST_LOOPBACK=0` desativa isso).
- DNS: os containers consultam o dnsmasq do guest, que responde por
  `host.docker.internal` e encaminha o restante para o resolver da libslirp e, de
  lá, para o do Mac. Ele permite 4096 consultas simultâneas em andamento; o padrão do dnsmasq, de
  150, fazia builds paralelos falharem com "Temporary failure in name resolution".
- O canal de controle também mantém o relógio do guest sincronizado. Ele mede o
  tempo de ida e volta e só ajusta o relógio quando a diferença passa de 250 ms.
  Por isso, o relógio pode voltar no tempo: código no guest não deve depender de mtimes para
  saber se algo mudou.
- `csrc/asyncclose.c` é uma pequena dylib que intercepta `close()` para sockets de
  internet. Extensões de rede de filtro de conteúdo podem fazer o `close()` após o FIN bloquear
  por cerca de 1 s, o que travaria todas as conexões na thread de rede.

## Guest (`guest/`)

`scripts/fetch-guest.sh` baixa um kernel e um minirootfs do Alpine em versões fixadas
(verificados por sha256) e monta um initramfs a partir deles e de `guest/`. Os
pacotes que a primeira inicialização instala (Docker, qemu, …) não são fixados: vêm
dos repositórios do Alpine naquele momento. O Alpine
remove versões antigas de pacotes, então o workflow semanal `guest-pins` verifica se
os dois downloads fixados ainda existem.

- **Primeiro boot:** instala o Docker e ferramentas relacionadas no disco de dados e depois tira um
  snapshot somente leitura da raiz. O Docker vem do Alpine v3.23 (29.5): o 29.8 reconstruía
  todas as camadas sobre uma imagem base construída localmente a cada build com cache quente.
- **Boots seguintes:** usam esse snapshot sob um overlay tmpfs, então levam
  2–4 s.

Componentes do guest:

- `dockerd.sh` escreve o `daemon.json` (cache de build limitado a 20 GB com
  `defaultMaxUsedSpace`) e inicia o dockerd com `orbit-runc`.
- `orbit-runc` é um wrapper de runtime OCI que faz os containers confiarem nas raízes
  em que o Mac confia além do conjunto da Apple. `make guest` exporta essas raízes
  (`scripts/export-host-ca.py`, geralmente uma CA corporativa de inspeção TLS; nenhuma na
  maioria dos Macs, e nenhuma com `ORBIT_HOST_CA=0`) para `/etc/orbit/host-ca.pem`.
  - Um container recebe uma cópia do bundle de CAs da imagem com elas acrescentadas,
    montada via bind mount somente leitura sobre o bundle.
  - Uma etapa `RUN` de build recebe, em vez disso, uma cópia de leitura e escrita do diretório do bundle,
    para que ferramentas que regeneram o bundle por rename funcionem. O runc é executado como
    processo filho; quando ele retorna (o BuildKit passa `--keep`, então o rootfs ainda está
    montado), o diretório é gravado de volta no rootfs da etapa com o
    bundle da própria imagem, somente se o conteúdo tiver mudado. As raízes do host nunca
    chegam a uma camada.
  - Um diretório que contém o diretório de outro bundle ou um ponto de montagem recebe, em vez disso, o
    bind do arquivo, e um bundle em um ponto de montagem existente ou abaixo dele
    (`RUN --mount`, `-v`) não é alterado.
- `dns.sh` executa o dnsmasq (veja Rede).
- `binfmt.sh` registra o qemu-x86_64.
- `memd.sh`: veja acima.

## No Mac (`scripts/`)

- `make install` copia o VMM, seu interposer de `close()`, `vmm-up.sh`,
  `vmm-run.sh` e `healthcheck.py` para `~/.orbit/bin`, escreve
  `~/.orbit/bin/VERSION` e instala o watchdog do launchd. A VM só é
  executada a partir dessa cópia: os containers podem escrever no checkout.
- `vmm-run.sh` supervisiona a VM, com ou sem launchd. Ele carrega as
  configurações que o `vmm-up.sh` salvou em `config.env` e as exporta, já que o
  launchd o inicia sem o ambiente do usuário.
  - Ele detecta um crash, um reboot ou panic do guest, ou um travamento (três
    pings perdidos no socket de controle; o VMM é abortado antes para que o macOS gere um
    relatório de crash). Só sob o launchd (o watchdog) a VM é iniciada de novo
    em seguida; sem ele, o supervisor registra a parada e termina.
  - Ele para de reiniciar após mais de três inicializações em 10 minutos.
  - Ele mantém um registro de cada parada inesperada em `crashes/<time>/`.
  - Ele emite um aviso abaixo de `ORBIT_WARN_FREE_GB` e para a VM de forma limpa abaixo de
    `ORBIT_STOP_FREE_GB`.
- `healthcheck.py`, se instalado, verifica o Docker, as portas, os bind mounts, o inotify,
  o relógio e o espaço em disco, registra a memória, as threads e os fds do VMM e, uma vez
  por dia, lê a versão mais recente na página `releases/latest` e mostra uma
  notificação para cada versão mais nova.
- `orbit` é o comando instalado: `up` e `down` executam o `vmm-up.sh`, `update`
  e `rollback` executam o `update.sh`, `status` reporta o estado e `uninstall` remove os
  agentes, os diretórios temporários do health check e o `~/.orbit`. Os targets
  de execução do Makefile o chamam, então nada do checkout é executado.
- `update.sh` (`orbit update`, `orbit rollback`) compila uma tag de release em seu próprio
  clone (`~/.orbit/src`) do repositório registrado em `~/.orbit/bin/VERSION`,
  e o novo guest em um diretório de staging, mantém os
  binários e o guest anteriores para rollback e reinicia a VM somente quando não há
  containers em execução.
