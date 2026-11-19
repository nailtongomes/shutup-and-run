# O que falta testar num Windows real

[TESTING.pt-BR.md](TESTING.pt-BR.md) tem o roteiro completo. Esta lista é só o que o CI não alcança: o runner do Windows instala, atualiza e faz rollback, mas não tem bateria, não tem tela, não tem programas de usuário abertos e não roda uma tarefa de verdade dentro do WSL2. Cada item diz o que conferir e onde. Ao terminar, anote o resultado (ok, falhou, com a saída) em [TESTING.pt-BR.md](TESTING.pt-BR.md) e apague daqui o que passou.

Antes de tudo, rode `.\tests\windows-round\check.ps1` (ou `check.ps1 -JobName NOME` com um container gerenciado `once`, para medir uma tarefa): ele confere sozinho o que não precisa de uma pessoa e escreve `rodada-AAAAMMDD-HHMM.md` com ok/falhou/ver por item. Junte esse arquivo ao relatório.

Um PC Windows 11 com WSL2, instalado ou atualizado para a 0.2.22 com `irm .../install.ps1 | iex` (`shutup-and-run version` diz 0.2.22), a página em `http://127.0.0.1:8787/` e o mock de poll do roteiro ([TESTING.pt-BR.md, seção 2](TESTING.pt-BR.md#2-uma-url-de-poll-para-testar)) servindo jobs curtos. Sem URL de poll, o modo gerenciado serve: um container `once` (`managed apply`, `managed mode on`) e `managed start NOME` para cada tarefa que um item pedir. Um container desabilitado não começa nem com `start`: `enable`, `start`, `disable`.

Para um agente que lê a página com um script: um POST para `/local/*` vindo de fora da própria página recebe 403 de propósito (só a página, no navegador, muda algo). Para pausar, autorizar ou mudar config por script, use a CLI (`shutup-and-run pause`, `safety authorize`, `config set`).

## Da 0.2.19 (o que a rodada 3 não cobriu)

Em [TESTING.pt-BR.md, "0.2.19"](TESTING.pt-BR.md#0219-bateria-tela-cheia-horário-consumo-consentimento-do-contrato): um job rodando ao cruzar 60% de bateria termina normalmente; de volta à tomada, os polls voltam em até um minuto; Teams ou PowerPoint em tela cheia; `lend_hours`; e `wsl install --owner-consent`.

## 0.2.20: relógio do Windows para `lend_hours`

O supervisor lê o fuso com `GetTimeZoneInformation` e manda a diferença para UTC a cada 10 s.

- [ ] Com `lend_hours` definido, a linha **Empresta** da página e o `shutup-and-run doctor` (linha `lending`) mostram a hora igual ao relógio da barra de tarefas, com o fuso (`now wed 14:05, UTC-03:00` em Brasília).
- [ ] Mude o fuso do Windows (Configurações > Hora e idioma) para outro, por exemplo Lisboa. Em até 10 s a página mostra a hora nova e o novo `UTC+00:00` (ou `UTC+01:00` no verão), sem reiniciar nada.
- [ ] Num fuso com horário de verão em vigor, o deslocamento inclui a hora a mais.
- [ ] `wsl -d shutup-and-run -u root date` pode mostrar outro fuso: tudo bem, o `lend_hours` segue o Windows.

## 0.2.20: pausar por um tempo

- [ ] Na página, **por 1 hora**: o topo diz **PAUSADO** e "até HH:MM" no relógio do Windows; o mock não recebe poll; uma tarefa que já roda termina.
- [ ] **pelo resto do dia**: o horário mostrado é 00:00.
- [ ] `shutup-and-run pause --for 2m`, espere dois minutos: os polls voltam sozinhos, sem clicar em nada.
- [ ] Com uma pausa por 1 hora, reinicie o PC: depois de entrar, a página ainda diz **PAUSADO** com o mesmo horário. Uma pausa sem prazo (o botão **Pausar**) termina com o reinício.

## 0.2.20: memória, o que fechar, quanto, e o WSL devolvendo

Prepare um PC com pouca memória livre: abra o Chrome com muitas abas, o Teams e o Excel até a memória livre do Windows ficar abaixo de 20% (Gerenciador de Tarefas, Desempenho, Memória).

- [ ] A página mostra o quadro **Falta memória**, com "libere cerca de X GiB" e **Usando mais memória**: os programas que você abriu, somados por programa (`chrome 3.1 GiB (38 processos)`). Não aparecem svchost, explorer, Antimalware, vmmem nem o próprio shutup-and-run.
- [ ] Os números batem, mais ou menos, com a coluna **Memória** do Gerenciador de Tarefas (ela mostra a memória privada; a soma por programa pode ficar um pouco abaixo).
- [ ] Feche o programa maior: em até 10 s o quadro diminui ou some, e a tarefa seguinte começa sozinha.
- [ ] `shutup-and-run doctor` tem as linhas `memory` (`MEMORY_TO_FREE`, com o tempo de espera) e `programs` (`MEMORY_HELD_BY_PROGRAMS`). `shutup-and-run doctor --report` **não** tem os nomes dos programas, nem no relatório nem no link do issue.
- [ ] `C:\ProgramData\shutup-and-run\status.json` tem o objeto `memory` (`short_of`, `to_free_bytes`, `waited_today_secs`). (Rodada 3: sem nomes de programa nele nem no log do supervisor, ok.)
- [ ] Com memória sobrando, nenhuma lista de programas aparece na página.
- [ ] Depois de alguns minutos esperando, a linha **Esperou por memória** mostra "hoje N min", e o número continua depois de reiniciar o serviço (`shutup-and-run wsl stop` e `wsl start`).
- [ ] Deixe o PC três dias instalado, com a memória curta em horário de uso: depois disso a linha **Memória curta, 7 dias** mostra as horas (por exemplo `mon-fri 09-18`) e o doctor tem `MEMORY_SHORT_HOURS` com um `lend_hours` sugerido. Confira que esse texto passa no `shutup-and-run config set lend_hours "..."`, e não aplique sem o dono concordar.

### O WSL devolvendo a memória

- [ ] Num `.wslconfig` sem `autoMemoryReclaim`, `shutup-and-run wsl doctor` tem a linha `wsl-limits` com `WSL_MEMORY_NOT_RECLAIMED`. `shutup-and-run wsl limits --apply` acrescenta `autoMemoryReclaim=dropcache` em `[experimental]`, mantém o resto e cria o `.wslconfig.bak`; ele não muda um `memory=` que o dono deixou abaixo do orçamento.
- [ ] Depois de `wsl --shutdown` (num momento sem tarefa e sem outro uso do WSL), confira que o WSL ainda sobe e que o `autoMemoryReclaim` é aceito pela versão instalada: `wsl --version` e nenhum aviso de chave desconhecida ao abrir a distribuição. Anote a versão do WSL. Se a sua versão aceitar a chave em `[wsl2]` e não em `[experimental]`, registre isso.
- [ ] Um `.wslconfig` com `autoMemoryReclaim=disabled` (escolha do dono) fica como está depois do `--apply`.

## 0.2.21: o que a rodada 3 encontrou

Rodada 3 (0.2.20): ok o WSL devolvendo a memória (VmmemWSL de ~2,4 GiB para 720 MiB menos de 20 s depois do log `gave the memory of the last job back to Windows`) e o `SAFETY_MEMORY_CRITICAL` que bloqueou sem matar o job em curso. O que ela achou e mudou depois:

- [ ] **Relógio do WSL2.** Compare `wsl -d shutup-and-run -u root date +%s` com `[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()` no PowerShell: no máximo 5 s de diferença, mesmo logo depois de o notebook voltar da suspensão (espere 10 s). Quando o serviço acerta, o log tem `set the WSL2 clock to Windows' time` com a diferença. `status.json` tem `lending.clock_skew_secs` nulo; o doctor só tem a linha `clock` (`CLOCK_SKEW`) se o acerto falhar.
- [ ] **Horas na página sem JavaScript.** `(Invoke-WebRequest http://127.0.0.1:8787/).Content` mostra as horas da tabela de tarefas no fuso do Windows (`2026-11-18 15:30 UTC-03:00`), não em UTC. No navegador, a hora local como antes.
- [ ] **Pausa pela CLI.** `shutup-and-run pause --for day` diz `paused until ... (... 00:00 on the PC's clock)`; `pause --until 08:00` diz 08:00 do dia seguinte (ou de hoje, antes das 8); `--until 8:00` e `--until 24:00` são recusados com exit 2. `resume` depois de cada um.
- [ ] **Sem `.wslconfig`.** `shutup-and-run wsl doctor`: a linha `wsl-limits` (`WSL_LIMITS_ABOVE_BUDGET`) termina com `no autoMemoryReclaim either`; depois do `wsl limits --apply` as duas coisas estão no arquivo.
- [ ] **Aviso autorizado e outro novo.** Com `SAFETY_MEMORY_LOW` autorizado, quando aparecer um aviso que não está autorizado (por exemplo `SAFETY_CPU_BUSY`), o doctor tem `AUTHORIZATION_REQUIRED` com `(already authorized: SAFETY_MEMORY_LOW)` no fim. Texto e `--json` rodados no mesmo minuto concordam.
- [ ] **Memória apertada mas sem bloquear.** Com o Windows abaixo de 20% livre e `SAFETY_MEMORY_LOW` autorizado: não há o quadro **Falta memória**, e a linha da lista de programas se chama **Memória apertada, as tarefas ainda começam; usando mais**. Com o quadro (memória impedindo uma tarefa), ela volta a se chamar **Usando mais memória**.
- [ ] **Bateria no `status.json`.** Na bateria, abaixo e acima de 60%, `lending.battery_percent` tem a carga; na tomada, nulo. `on_battery_percent` continua como antes (só acima de 60%).
- [ ] **Consumo durante a tarefa.** Com um job de mais de 30 s rodando, a coluna **CPU / memória** da página e o `shutup-and-run jobs` (`used: ... so far`) já mostram números; no fim, os finais.
- [ ] **Horas de memória curta.** Com menos de três dias instalado, a linha **Memória curta, 7 dias** pode aparecer, mas o doctor não sugere `lend_hours` (sem `MEMORY_SHORT_HOURS`).

## 0.2.22: limpeza, disco do WSL2, status, resumo por imagem

O `check.ps1` cobre a maior parte; o que ele não mede:

- [ ] **Limpeza diária.** Uns 10 minutos depois de o serviço subir, a linha **Limpeza** da página e a linha `cleanup` do doctor dizem a hora da última passada (a primeira roda nesse momento). As imagens usadas nos últimos 30 dias continuam em `wsl -d shutup-and-run -u root docker images`. A remoção depois de 30 dias está nos testes; num PC instalado há mais de um mês, o log tem `removed what no job used for 30 days` quando há o que remover.
- [ ] **Imagens sem tag.** Rode duas vezes, com uma versão nova no meio, a mesma imagem `:latest`: a cópia velha (`<none>` em `docker images`) some na passada seguinte.
- [ ] **Disco do WSL2.** `shutup-and-run wsl doctor` tem a linha `wsl-disk` com o tamanho do `ext4.vhdx` e o que está em uso. Se aparecer `WSL_DISK_TO_COMPACT`, siga "Give the WSL2 disk's space back" do `docs/WINDOWS.md` como administrador, sem tarefa rodando, e anote o tamanho antes e depois.
- [ ] **status.** `shutup-and-run status` mostra as linhas novas quando há o que dizer: pausa com fim, `lend_hours`, espera, bateria, memória, última limpeza.
- [ ] **Resumo por imagem.** Depois de três tarefas da mesma imagem, `shutup-and-run jobs --summary` tem a linha dela com `ask for memory`.

## 0.2.23: rede limitada, rede por tarefa, saída ao vivo

O `check.ps1` mostra o custo da rede pelo Windows ao lado do `metered_network` do `status.json`, e com `-JobName` lê a saída da tarefa enquanto ela roda e mostra os bytes de rede no fim. O que ele não faz:

- [ ] **Roteador do celular.** Ligue o notebook no roteador de um celular (ou marque o Wi-Fi como limitado em Configurações, Rede, Propriedades): a página diz **ESPERANDO** com "rede limitada", `status.json` tem `metered_network: true`, o código é `METERED_NETWORK` e o mock não recebe polls. Uma tarefa que já rodava continua. Volte para a rede de antes: os polls voltam sozinhos.
- [ ] **Aceitar a rede limitada.** No roteador do celular, `shutup-and-run config set lend_on_metered true --apply` (ou a caixa **Emprestar também em rede limitada** na página): os polls voltam. `config unset lend_on_metered --apply` volta a esperar.
- [ ] **Rede por tarefa.** Depois de uma tarefa que baixa algo (um `curl` de um arquivo conhecido), `shutup-and-run jobs` tem `network ...` na linha `used:` com um tamanho próximo, e a coluna **CPU / memória / rede** da página o mesmo.
- [ ] **Saída ao vivo na página.** Com uma tarefa rodando, a página **Tarefas e logs** tem o link **até agora**; ele abre o que a tarefa imprimiu até aquele momento.


## 0.2.24: prazo do download, log limitado, disco por tarefa, digest, volta às, SIGTERM, processos, volumes, journal

O `check.ps1` com `-JobName` mostra também o disco gravado pela tarefa. O que ele não faz:

- [ ] **Download lento.** Numa conexão lenta, uma tarefa de `timeout_secs` 30 com uma imagem grande ainda não baixada começa depois do download e roda os seus 30 segundos; antes ela terminava em `start_failed` ("image pull timed out").
- [ ] **Log limitado.** Uma tarefa que imprime sem parar (`sh -c 'yes'` por um minuto): `docker inspect` do container, dentro da distribuição, tem `LogConfig` `json-file` com `max-size` 5m e `max-file` 2, e o arquivo de log dele em `/var/lib/docker/containers/` não passa de 5 MB.
- [ ] **Disco por tarefa.** Depois de uma tarefa que grava um arquivo conhecido (`dd if=/dev/zero of=/tmp/x bs=1M count=500`), `shutup-and-run jobs` tem `disk 500 MiB` na linha `used:`, e a coluna **CPU / memória / rede / disco** da página o mesmo.
- [ ] **Imagem por digest.** Com o Wi-Fi desligado, uma tarefa de uma imagem pedida como `repo@sha256:...` que já está no PC começa na hora, sem esperar o registro.
- [ ] **Volta às.** Fora de `lend_hours`, a página diz **ESPERANDO** e "As tarefas novas voltam sozinhas qua 19:00" com o dia e a hora da próxima janela; `status.json` tem `lending.resumes_at`.
- [ ] **SIGTERM antes do SIGKILL.** Uma tarefa `sh -c 'trap "echo saindo; exit 0" TERM; while true; do sleep 1; done'` com `timeout_secs` 20: ao fim, o log dela (`jobs logs ID`) tem `saindo`, e o `jobs` diz `timeout` com saída 137.
- [ ] **Limite de processos.** Dentro da distribuição, `docker inspect` de uma tarefa rodando tem `PidsLimit` 4096 e `Init` true.
- [ ] **Volumes.** Depois de uma tarefa de uma imagem com `VOLUME` (por exemplo `postgres`, com um comando curto), `docker volume ls` dentro da distribuição não tem um volume novo.
- [ ] **Journal.** Depois do `wsl update`, `journalctl --disk-usage` dentro da distribuição fica abaixo de 100 MB, e existe `/etc/systemd/journald.conf.d/shutup-and-run.conf`.

## 0.2.25: progresso do download, data-root, credential helper, aviso de versão nova, .wslconfig no uninstall

- [ ] **Progresso do download.** Uma tarefa de uma imagem grande ainda não baixada: enquanto baixa, `shutup-and-run jobs` mostra `downloading the image: X of Y` no lugar de `used:`, a página mostra `baixando a imagem: X de Y` na coluna de uso, e `shutup-and-run status` uma linha `downloading the image of ID`. Quando o container começa, a linha volta a ser `used:`.
- [ ] **Versão nova.** Com o `.exe` de uma versão anterior à última release, `shutup-and-run doctor` tem a linha `update` com `UPDATE_AVAILABLE` e a dica da linha de instalação; na última versão, `<versão> is the newest release`. Sem internet, a linha não aparece e o doctor não demora mais que 5 segundos por isso.
- [ ] **.wslconfig no uninstall.** Num PC sem `%UserProfile%\.wslconfig` e sem outra distribuição, `wsl install` escreve o arquivo e `%ProgramData%\shutup-and-run\wslconfig.sha256`; `wsl uninstall` diz `removed ... which the install wrote` e o arquivo some. Repita mudando uma linha do arquivo antes do uninstall: ele diz `kept ...: it changed` e o arquivo fica. Com outra distribuição (um Ubuntu): `kept ...: other WSL distributions are under it`.
