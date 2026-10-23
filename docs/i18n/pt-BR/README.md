# orbit-vmm

[English](../../../README.md) | [한국어](../ko/README.md) | [日本語](../ja/README.md) | [简体中文](../zh-CN/README.md) | [Español](../es/README.md) | **Português (Brasil)** | [Français](../fr/README.md) | [Deutsch](../de/README.md) | [हिन्दी](../hi/README.md)

> Esta é uma tradução. O [README em inglês](../../../README.md) é a fonte oficial; se houver divergência, siga a versão em inglês.

[![CI](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml/badge.svg)](https://github.com/skax-eunsik-kim/orbit-vmm/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../../../LICENSE)

Um engine Docker econômico em memória para Macs com Apple Silicon.

O orbit-vmm é um pequeno monitor de máquina virtual escrito em Rust sobre o
Hypervisor.framework da Apple. Ele inicializa um guest Alpine Linux que roda o
Docker, e a CLI `docker` / `docker compose` no seu Mac se comunica com ele por
um socket Unix.

A maioria das VMs Docker no macOS cresce até o pico de uso de memória e fica
lá. O orbit-vmm devolve a memória. O guest informa as páginas que libera
(*free page reporting* do virtio-balloon), e o VMM as devolve ao macOS em
poucos segundos. Um pequeno daemon no guest também descarta o page cache
ocioso e compacta a memória, para que a memória liberada possa de fato ser
devolvida.

> **Status: inicial.** Ele roda stacks compose reais todos os dias, mas é
> jovem, tem um único mantenedor e não tem instalador nem GUI. Leia
> [Limitações conhecidas](#limitações-conhecidas) antes de depender dele.

## Recursos

- **A memória volta para o macOS.** Free page reporting com granularidade de
  16 KiB, além do corte do page cache ocioso. Depois que uma carga de trabalho
  termina, o footprint da VM no Mac cai de volta para cerca de 1 GB em menos de
  um minuto ([benchmarks](benchmarks.md)).
- **Bind mounts via virtio-fs.** `-v $PWD:/app` funciona no mesmo caminho que
  no Mac. Há várias filas de requisições, cache de atributos e readdirplus.
  Edições no Mac, novos arquivos e diretórios, renomeações e exclusões disparam
  eventos inotify nos containers, então o hot reload do `uvicorn --reload`, do
  vite e do nodemon continua funcionando durante um `git checkout` e com
  geradores de código. `chown` e `chmod` dentro de um container também
  funcionam. Ambos são emulados com xattrs. O arquivo real continua sendo seu e
  nunca vira setuid nem gravável pelo grupo ou por todos no Mac.
- **Compartilhamento opt-in.** Só os diretórios permitidos ficam visíveis para
  a VM, e a resolução de caminhos é segura contra symlinks. Um container com o
  socket do Docker não consegue ler `~/.ssh`.
- **Rede que se comporta como o Docker Desktop:**
  - Portas publicadas aparecem automaticamente em `127.0.0.1`.
  - `host.docker.internal` e `host-gateway` resolvem para o Mac.
  - Os containers confiam nas CAs raiz em que o seu Mac confia, então `pip`,
    `npm`, `apk` e `curl` funcionam atrás de proxies com inspeção de TLS.
- **Imagens amd64** rodam via emulação de usuário do qemu (binfmt).
- **Várias VMs lado a lado** com `ORBIT_INSTANCE=<name>`.
- **Pequeno e auditável.** Um único binário Rust de cerca de 5,5 mil linhas.
  Ele depende de `libc`, `vm-fdt` e libslirp, e de nada mais.

## Requisitos

- Mac com Apple Silicon, macOS 15 ou posterior (a API de GIC no kernel exige
  15+).
- Rust 1.87+ e libslirp: `brew install rust libslirp`.
- Docker CLI com o plugin compose: `brew install docker docker-compose`.

### Em uma versão do macOS em que o orbit ainda não foi testado

Testado até agora no macOS 26.4. Como a memória do Hypervisor.framework pode
ser devolvida ao macOS, e como o Activity Monitor a contabiliza, depende do
macOS; por isso, verifique estes três pontos em uma versão nova antes de
confiar nele:

1. **Liberação de memória.** Sem nenhuma VM rodando:

   ```bash
   vmm/target/release/orbit-vmm --reclaim-selftest
   ```

   No bloco `guest RAM from hv_vm_allocate`, a linha
   `unmap + mmap(MAP_FIXED anon) + map` deve terminar perto de
   `released 1 MiB`. Esse é o método que o VMM usa (`vmm/src/reclaim.rs`). Se
   ficar em ~257 MiB, a memória liberada pelo guest não é devolvida nessa
   versão: a VM só cresce no Mac.
2. **Fluxo de desenvolvimento.** Com a VM no ar: `make devflow` deve reportar
   0 fail (25 verificações).
3. **Activity Monitor.** A coluna Memory dele é o `phys_footprint` do VMM.
   Quando o Mac está com pouca memória, o macOS comprime parte da RAM do guest
   e esse número passa a ser maior do que o guest usa. Nos testes, a diferença
   ficou perto de zero sem nada comprimido e em cerca de um terço a metade do
   volume comprimido sob pressão. Uma diferença distinta em outra versão do
   macOS é uma mudança na forma como o macOS contabiliza, não um vazamento,
   desde que `ORBIT_MEMDUMP` + `vmm/bench/memxref.py` mostrem o VMM retendo
   aproximadamente o que o guest usa (veja
   [docs/architecture.md](architecture.md)).

## Início rápido

```bash
git clone https://github.com/skax-eunsik-kim/orbit-vmm ~/src/orbit-vmm
cd ~/src/orbit-vmm
make guest      # download pinned Alpine kernel + rootfs (sha256-checked) into ~/.orbit/guest
make install    # build, sign, copy the VMM and scripts to ~/.orbit/bin, install the watchdog
make up         # start the VM from ~/.orbit/bin; first boot installs Docker (~1 min), later boots 2-4 s
export DOCKER_HOST=unix://$HOME/.orbit/vmm/docker.sock
docker run --rm hello-world
```

Coloque a linha do `DOCKER_HOST` no perfil do seu shell, ou crie um context:

```bash
docker context create orbit --docker host=unix://$HOME/.orbit/vmm/docker.sock
docker context use orbit
```

`make up` (ou `~/.orbit/bin/vmm-up.sh`) reinicia uma VM em execução, o que
para todos os containers dela. A VM e o health check rodam apenas a partir da
cópia instalada em `~/.orbit/bin`. Os containers podem gravar nos seus
diretórios compartilhados, e este checkout costuma ser um deles, então o
`make up` não compila nem executa nada a partir dele. Só `make install` e
`make guest` fazem isso. Antes de rodá-los, verifique se o checkout não mudou
inesperadamente (`git status`). Melhor ainda: mantenha o checkout fora dos
seus diretórios compartilhados.

## Configuração

`scripts/vmm-up.sh` (o que o `make up` executa) lê estas variáveis de ambiente:

| Variável | Padrão | Significado |
|---|---|---|
| `ORBIT_MEM_MIB` | `8192` | Teto de RAM do guest. A memória liberada volta para o macOS, então um teto alto custa pouco. |
| `ORBIT_CPUS` | Número de núcleos do Mac (`sysctl -n hw.ncpu`) | vCPUs. vCPUs ociosas ficam estacionadas e não consomem CPU; com poucas, a inicialização paralela de containers (por exemplo, sandboxes de agentes) fica na fila por CPU. |
| `ORBIT_GUEST` | `~/.orbit/guest` | Diretório com o `kernel` e o `initramfs` do guest (por exemplo, um gerado com `ORBIT_HOME=… scripts/fetch-guest.sh`). |
| `ORBIT_DISK_GB` | `64` | Tamanho de um **novo** disco de dados (arquivo esparso). |
| `ORBIT_DISK_WARN_MIB` | `ORBIT_WARN_FREE_GB` em MiB | Abaixo desse espaço livre no Mac, o VMM também emite um aviso no `console.log`. |
| `ORBIT_DISK_FLOOR_MIB` | `1024` | Último recurso abaixo de `ORBIT_STOP_FREE_GB`: abaixo disso, as gravações em disco do guest ficam retidas até que haja espaço livre (veja [Disco de dados](#disco-de-dados)). |
| `ORBIT_DISK_PAUSE_SECS` | `600` | Quanto tempo uma gravação retida espera antes de falhar. |
| `ORBIT_SHARE_ALLOW` | veja abaixo | Diretórios separados por dois-pontos que os containers podem montar via bind mount. |
| `ORBIT_HOST_LOOPBACK` | `1` | `0` impede que os containers acessem serviços no `127.0.0.1` do Mac (e no `host.docker.internal`). |
| `ORBIT_WARN_FREE_GB` / `ORBIT_STOP_FREE_GB` | `10` / `3` | Espaço livre em disco no Mac a partir do qual a VM avisa você, e a partir do qual ela desliga de forma limpa antes que o disco encha. |
| `ORBIT_INSTANCE` | (não definida) | Roda uma VM separada, com disco e sockets próprios, em `~/.orbit/vmm-<name>`. |

### Compartilhamento de arquivos

Por padrão, a VM enxerga aqueles que existirem entre `~/work`, `~/src`,
`~/code`, `~/dev`, `~/projects` e `~/Developer`. Todo o resto em `/Users` fica
invisível. Para adicionar diretórios:

```bash
ORBIT_SHARE_ALLOW="$HOME/src:$HOME/clients" make up
```

Diretórios compartilhados são **graváveis por todos os containers**. O
orbit-vmm se recusa a compartilhar o seu diretório home, qualquer coisa acima
dele, diretórios ocultos (`~/.ssh`, `~/.orbit`, …), `~/Library`, `~/bin` e
`~/Applications`. Um diretório dentro de outro já compartilhado é descartado,
pois o externo já o cobre. Os containers não conseguem renomear nem remover os
próprios diretórios compartilhados.

- Edições em arquivos feitas no Mac ficam visíveis nos containers em cerca de
  0,5 s. Novos arquivos e diretórios renomeados aparecem em cerca de 1 s.
- Edições e novos arquivos no Mac geram `IN_MODIFY` / `IN_CLOSE_WRITE` nos
  containers cerca de 1 s depois.

### Rede

- `-p 8080:80` e `ports:` do compose são encaminhados para `127.0.0.1:8080` no
  Mac em cerca de 1 s. Se a porta do Mac estiver ocupada, o encaminhamento
  falha, e o erro vai para `~/.orbit/vmm/console.log`.
- Os containers acessam serviços no localhost do Mac via
  `host.docker.internal`. Isso inclui qualquer coisa escutando apenas em
  `127.0.0.1` (bancos de dados, notebooks, servidores de modelos locais).
  Defina `ORBIT_HOST_LOOPBACK=0` se não quiser isso.
- Qualquer processo escutando em todos os endereços na VM recebe a mesma porta
  no `127.0.0.1` do Mac, não só as publicações com `-p` (como acontece com
  `--network host`).

### Certificados raiz (proxies com inspeção de TLS)

- `make guest` lê as raízes em que o seu Mac confia além do conjunto embutido
  da Apple (as marcadas como "Always Trust" para SSL, normalmente uma CA
  corporativa de inspeção de TLS) e as incorpora ao seu guest. Em um Mac sem
  essas raízes, nada é adicionado, e os containers veem os bundles das
  próprias imagens. O repositório e os releases não contêm certificados.
- Containers e etapas de build veem essas raízes anexadas ao bundle de CAs da
  imagem, por meio de um mount. Uma etapa de build recebe uma cópia do
  diretório do bundle, então `update-ca-certificates` e `apt-get upgrade`
  funcionam. Nenhum dos dois grava as raízes do Mac na imagem.
- `ORBIT_HOST_CA=0 make guest && make up` gera um guest sem elas.
- A partir daí, o seu guest (`~/.orbit/guest`) e o disco de dados contêm as
  CAs da sua organização: não os compartilhe fora dela.

## Arquivos

| Caminho | Conteúdo |
|---|---|
| `~/.orbit/guest/` | Kernel e initramfs gerados por `make guest`. |
| `~/.orbit/vmm/data.img` | Disco de dados (imagens Docker, volumes). |
| `~/.orbit/vmm/docker.sock` | API do Docker. |
| `~/.orbit/vmm/console.log` | Console do guest; olhe aqui primeiro quando algo falhar (`console.prev.log` é a execução anterior). |
| `~/.orbit/vmm/watchdog.log`, `crashes/` | Log do supervisor e registros de paradas inesperadas. |

**Não compartilhe `~/.orbit/guest/*` nem `data.img`.** Eles contêm binários
GPL e os certificados raiz confiáveis da sua máquina
([THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md)).

Para aumentar o disco de dados, pare a VM, aumente o arquivo e inicie a VM
novamente. O guest redimensiona o sistema de arquivos na inicialização:

```bash
python3 -c "import os;p=os.path.expanduser('~/.orbit/vmm/data.img');os.truncate(p,max(os.path.getsize(p),128<<30))"
```

### Disco de dados

O `data.img` é esparso: ocupa no Mac tanto espaço quanto o Docker gravou, até
o seu tamanho lógico (o `vmm-up.sh` mostra os dois). Ele cresce
silenciosamente, por isso duas proteções monitoram o espaço livre do Mac. O
supervisor (`vmm-run.sh`) notifica você abaixo de `ORBIT_WARN_FREE_GB` e
desliga o guest de forma limpa abaixo de `ORBIT_STOP_FREE_GB` (e também não o
inicia nessa situação). Abaixo disso, como último recurso, o VMM verifica a
cada 5 segundos:

- Abaixo de `ORBIT_DISK_WARN_MIB`, ele registra `orbit-vmm: disk space low` no
  `console.log` (novamente a cada 10 minutos enquanto o espaço continuar
  baixo), e o `scripts/healthcheck.py` reporta um aviso.
- Abaixo de `ORBIT_DISK_FLOOR_MIB`, ou quando uma gravação falha porque o
  disco está cheio, ele retém as gravações em disco do guest e registra
  `orbit-vmm: DISK FULL` uma vez. O Docker trava (o guest pode registrar
  avisos de hung task), mas nada falha. Libere espaço no Mac e as gravações
  continuam de onde pararam (`disk writes resumed`).
- Se nenhum espaço for liberado dentro de `ORBIT_DISK_PAUSE_SECS`, as
  gravações falham. O ext4 então reporta erros de I/O e pode passar para
  somente leitura; por isso, libere espaço antes disso e reinicie a VM caso
  isso aconteça.

Reter as gravações em vez de fazê-las falhar é o que o QEMU faz por padrão
(`werror=enospc`): uma gravação que falha é um dado que o ext4 já prometeu
armazenar, e perdê-lo é a forma como um disco cheio no Mac corrompe o guest.

## Atualização

```bash
make update            # the latest release; TO=v0.2.1 for a given one
make rollback          # back to what was installed before the last update
```

`make update` compila o release em um clone próprio (`~/.orbit/src`), não no
seu checkout, e gera o novo guest antes de substituir qualquer coisa, então um
download ou build com falha deixa a versão instalada como estava. Ele mantém
os binários e o guest anteriores para o `make rollback`. Ele só reinicia a VM
quando não há containers em execução; caso contrário, a VM mantém a versão
antiga até o `make up` (ou execute `NOW=1 make update`). A primeira
inicialização após uma atualização do guest reinstala os pacotes do guest
(cerca de 1 min).

Se o health check estiver instalado, ele consulta o release mais recente uma
vez por dia e mostra uma notificação do macOS para cada release mais novo.
Nada é instalado sem o `make update`.

Para acompanhar a `main` em vez dos releases:
`git pull && make guest install up`.

## Watchdog

O `make install` também instala um agente do launchd, `com.orbit.vmm`, que
executa a VM e a recupera. Ele existe porque nada mais faz isso: o orbit não
tem um app que fique em execução, então, sem ele, um VMM que morre ou trava
continua fora do ar.

```bash
ORBIT_WATCHDOG=0 make install          # install without it
./scripts/install-watchdog.sh --uninstall
./scripts/install-watchdog.sh --reload # apply a changed agent now (restarts the VM)
```

Instalar novamente não mexe em uma VM em execução.

- **Crash:** se o VMM morrer, ou se o guest reiniciar ou entrar em panic, o
  launchd o inicia novamente em poucos segundos.
- **Travamento:** o guest recebe um ping a cada 30 s. Após três pings sem
  resposta, o VMM é abortado, para que o macOS gere um relatório de crash, e
  em seguida ele é reiniciado. Isso leva cerca de 90 s.
- **Loop de crashes:** depois de mais de três inicializações em 10 minutos,
  ele para de reiniciar e avisa você.
- **Registros de crash:** cada parada inesperada é guardada em
  `~/.orbit/vmm/crashes/<time>/` (log do console, relatório de crash do macOS,
  log do watchdog). Os 10 mais recentes são mantidos, e você recebe uma
  notificação do macOS. Os containers param junto com a VM, então os que não
  têm uma política `restart:` precisam ser iniciados novamente.
- **De propósito:** `make down` (`vmm-up.sh --stop`) e um `poweroff` no guest
  contam como paradas intencionais e não são reiniciados. Uma VM que estava em
  execução quando você fez logout é iniciada novamente no login.

Sem o watchdog, o `make up` executa o mesmo supervisor em segundo plano, então
travamentos continuam sendo detectados e crashes continuam sendo registrados,
mas nada é reiniciado.

## Health check (opcional)

Depois do `make install`, o `scripts/install-healthcheck.sh` instala um agente
do launchd que executa o `~/.orbit/bin/healthcheck.py` instalado. Ele verifica
o Docker, as portas, os bind mounts, o inotify e o relógio do guest, e
registra a memória, o número de threads e de fds do VMM em
`~/.orbit/vmm/health.jsonl`.

A verificação roda a cada hora. Após três execuções saudáveis seguidas, o
intervalo passa para a cada 2 h e depois para a cada 4 h. Qualquer problema o
volta para a cada hora. Para removê-lo, execute `--uninstall`.

## Limitações conhecidas

- **inotify:** mudanças feitas no Mac chegam aos containers cerca de um
  segundo depois. Uma nova árvore de diretórios com mais de 256 entradas (uma
  worktree nova, um `npm install` no Mac) gera um único `IN_CREATE` para o
  diretório do topo; watchers recursivos a varrem a partir dali. Renomeações
  só formam par quando o guest já tinha visto o nome antigo; caso contrário,
  aparecem como exclusão + criação.
- **Encaminhamento de portas:** publicações UDP não são encaminhadas.
  Publicações em um IP específico do host (`-p 127.0.0.1:8080:80`) são
  encaminhadas para o `127.0.0.1` do Mac como as demais; a mesma porta
  publicada em dois desses IPs chega apenas à mais recente. Se duas VMs
  publicarem a mesma porta, o encaminhamento da segunda falha (com registro no
  log).
- **Bind mounts:** nenhum lock de arquivo é compartilhado entre o Mac e os
  containers. Sockets Unix e FIFOs não podem ser criados em bind mounts.
- **Disco de dados:** o espaço que o Docker libera só volta para o Mac quando
  o guest executa trim (`fstrim`, a cada 10 minutos quando a VM está ociosa, e
  uma vez após a inicialização), então um `docker system prune` aparece no Mac
  alguns minutos depois. A imagem nunca encolhe abaixo do que o guest gravou e
  não liberou.
- **Certificados raiz:** uma etapa de build que copia o bundle de CAs para
  outro lugar (`cp /etc/ssl/certs/ca-certificates.crt /app/`) copia junto as
  raízes do Mac para dentro da imagem. Regenerar o bundle funciona em etapas
  de build, mas não em containers em execução, e comandos posteriores no mesmo
  `RUN` não veem as raízes do Mac. Se algo estiver montado no diretório de CAs
  (`RUN --mount`, `-v`), as raízes não são adicionadas ali.
- **O que falta:** não há instalador, GUI nem Kubernetes.

## Como funciona

Veja [docs/architecture.md](architecture.md). Em resumo:

```
docker CLI ──unix socket── orbit-vmm ──vsock── guest: dockerd
                              │
          Hypervisor.framework (vCPUs, in-kernel GICv3)
          virtio-mmio: blk · net (libslirp) · vsock · fs · balloon (free page reporting)
```

## Contribuindo

Relatos de bugs e pull requests são bem-vindos. Veja o
[CONTRIBUTING.md](../../../CONTRIBUTING.md) para a configuração do ambiente de
desenvolvimento e as verificações. Reporte problemas de segurança de forma
privada ([SECURITY.md](../../../SECURITY.md)).

## Licença

Apache-2.0 ([LICENSE](../../../LICENSE)). A imagem do guest que você gera baixa pacotes
do Alpine Linux, que têm as próprias licenças; veja
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md).
