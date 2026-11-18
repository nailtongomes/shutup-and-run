# Teste manual de uma release

English: [TESTING.md](TESTING.md).

Uma VPS Linux (sua, confiável) e um PC com Windows 11 (tratado como PC de cliente). Cerca de 30 minutos. Cada passo tem o comando e depois o que esperar. Se um passo não bater, pare ali e guarde a saída.

## 0. A release existe

As instalações de uma linha baixam da última release do GitHub.

- [ ] `https://github.com/nailtongomes/shutup-and-run/releases/latest` é a versão do `Cargo.toml` (`v0.2.14` quando isto foi escrito) ou posterior e tem três arquivos: `shutup-and-run-x86_64-unknown-linux-musl`, `shutup-and-run-x86_64-pc-windows-msvc.exe`, `SHA256SUMS`.

Se não estiver lá: o workflow `release` roda sozinho quando o `main` recebe uma versão do `Cargo.toml` que ainda não tem release, cria a tag e publica. Veja a última execução em Actions; "Run workflow" no `main` repete uma que falhou. Ninguém cria tag à mão.

## 1. VPS Linux: push

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/nailtongomes/shutup-and-run-smoke bash
```

- [ ] Imprime a URL e a impressão digital do certificado, não o token.

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/smoke.sh | sudo SMOKE_LOCAL=1 bash
```

- [ ] As linhas são `PASS:` ou `SKIP:` com o motivo, nenhuma é `FAIL:`, e a última é `RESULT: N passed, 0 failed, ...`. Isso cobre o `202`, o `429` quando cheio, o kill por timeout, a `job_key` e o drain.

Instância única:

```bash
sudo SHUTUP_AND_RUN_CONFIG=/etc/shutup-and-run/config.toml SHUTUP_AND_RUN_TOKEN=x /usr/local/bin/shutup-and-run serve; echo "exit $?"
```

- [ ] `another shutup-and-run is already running (...jobs.lock is locked)` e `exit 1`. O serviço não é afetado: `systemctl is-active shutup-and-run` responde `active`.

O arquivo de status, na VPS:

```sh
sudo sed -n 's/.*"state":"\([^"]*\)".*/\1/p' /var/lib/shutup-and-run/status.json
```

- [ ] Imprime `idle` (ou `active` se um job do smoke ainda estiver rodando). O `updated_at` desse arquivo está dentro do último minuto.

## 2. Uma URL de poll para testar

Na VPS (ou em qualquer máquina que o PC Windows alcance). É um mock: HTTP sem TLS, só para teste.

```bash
curl -fsSLO https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/tests/poll-mock/poll_mock.py
python3 poll_mock.py --require-approval --tasks 3 --keep-serving
```

- [ ] Ele imprime `poll URL: http://<this machine's address>:8080/poll`. Abra a porta 8080 no firewall durante o teste. Deixe esse terminal visível.

## 3. Windows 11: instalar ocioso

Uma vez, no PowerShell **como administrador**, e depois reinicie:

```powershell
wsl --install --no-distribution
```

Depois, num PowerShell normal, com o usuário que vai ficar logado:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
shutup-and-run wsl install
```

- [ ] O SmartScreen pode avisar uma vez ("Mais informações", "Executar assim mesmo"). O `.exe` não é assinado.
- [ ] Passos `[1/8]` a `[8/8]`, alguns minutos (download de 340 MiB, depois apt).
- [ ] A tabela do doctor é toda `ok`, inclusive `container  hello-world ran: Hello from Docker!`.
- [ ] Última linha: `done. No poll URL: the worker is installed and idle.`
- [ ] Se o passo 7 imprimir `could not change the task settings`, anote a mensagem. A instalação funciona mesmo assim.

Sem janela, e a página de status:

```powershell
Get-Process wsl, shutup-and-run, conhost, WindowsTerminal, OpenConsole -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Format-Table Name, Id, MainWindowTitle
```

- [ ] Só aparece o terminal em que você está digitando. Nenhuma janela com título `wsl.exe` apareceu durante a instalação, e nenhuma aparece depois de sair e entrar de novo ou depois de `shutup-and-run wsl start`.
- [ ] `http://127.0.0.1:8787/` no navegador mostra `RODANDO / RUNNING`, `0 / 1` e a versão. O botão pausa (`PAUSADO / PAUSED`) e retoma.
- [ ] `(Get-Content C:\ProgramData\shutup-and-run\status.json -Raw | ConvertFrom-Json).state` imprime `idle`. Depois de `shutup-and-run wsl stop` imprime `stopped`.
- [ ] `shutup-and-run wsl doctor` logo depois da instalação diz `ok   service`, e não "not running".

Configurações da tarefa e instância única:

```powershell
(Get-ScheduledTask shutup-and-run).Settings | Format-List ExecutionTimeLimit, DisallowStartIfOnBatteries, StopIfGoingOnBatteries, MultipleInstances
shutup-and-run wsl serve
```

- [ ] `ExecutionTimeLimit : PT0S`, as duas opções de bateria `False`, `MultipleInstances : IgnoreNew`.
- [ ] `wsl serve` imprime `another shutup-and-run is already running` e volta na hora.

## 4. Windows 11: pré-cadastro e trabalho

```powershell
shutup-and-run wsl install --poll-url http://IP_DA_SUA_VPS:8080/poll
```

- [ ] `made a poll token for this machine` e, no fim, `This machine has a new token. It gets no work until the poll URL approves it.` O token nunca aparece.
- [ ] Na VPS, o mock imprime um `PENDING <hash> ip=<IP público do PC> body={"worker":"<nome do PC>","os":"windows",...}`.
- [ ] `shutup-and-run wsl logs` mostra `poll returned 403 Forbidden: the poll URL has not approved this machine`.

Aprove a máquina, na VPS, na pasta onde o mock roda, com a linha que o mock imprimiu:

```bash
echo <hash> >> approved.txt
```

- [ ] Em até cinco minutos (o worker espaça as consultas enquanto é recusado) o mock imprime `TASK mock-...-1` e depois `ok   1/3 ... reason=exited exit=0`, três vezes, com poucos segundos entre elas e não 30 (0.2.9: o worker pergunta de novo assim que uma tarefa termina), e então um `SUMMARY` com `ok 3`. Depois disso responde `204`, e os polls voltam a um a cada 30 segundos.
- [ ] `shutup-and-run wsl logs` mostra `poll started a job` três vezes.

Instale de novo, mudando uma opção:

```powershell
shutup-and-run wsl install --poll-url http://IP_DO_SEU_VPS:8080/poll --max-slots 2
```

- [ ] `kept this machine's poll token`, e o mock não imprime outro `PENDING`: é a mesma máquina.
- [ ] A linha `service` do doctor é `ok` e o JSON dela tem `"slots_total":2`: o serviço que responde leu a configuração nova. A instalação termina com `done` e `with 2 slot(s)`.
- [ ] `wsl -d shutup-and-run -u root -e /usr/local/bin/shutup-and-run bans` imprime `no bans`.

Abra um segundo PowerShell, rode `shutup-and-run wsl stop` e depois `shutup-and-run wsl serve`, e deixe aberto. No primeiro, instale de novo com o mesmo comando.

- [ ] Ou a instalação termina e o segundo terminal volta ao prompt, ou ela para antes do passo 3 com `a shutup-and-run wsl serve is still running`. Ela nunca imprime `done` com o serviço antigo ainda rodando. Anote qual dos dois aconteceu.

### 0.2.9: dicas, jobs, query de pareamento

- [ ] `shutup-and-run jobs` lista as três tarefas, a mais nova primeiro, cada uma `ok`, `exit 0`, com duração. `shutup-and-run jobs --limit 1 --json` tem uma entrada com `id`, `image`, `started_at`, `ended_at`, `duration_secs`, `exit_code`, `status`, `failure`, e mais nada.
- [ ] `shutup-and-run pause`, depois `shutup-and-run doctor --json`: a linha `paused` tem `"code":"WORKER_PAUSED"` e um `"hint"` que cita `shutup-and-run resume`. `shutup-and-run config set language pt`, o doctor de novo: a dica vem em português. `shutup-and-run resume`.
- [ ] `shutup-and-run config set max_slots 0 --json` sai com 3, `"code":"INVALID_VALUE"` e um `"hint"`.
- [ ] `shutup-and-run config set poll_url "http://IP_DA_SUA_VPS:8080/poll?cpf=00000000000&email=nobody@example.com" --apply`. O mock continua respondendo. `shutup-and-run config show`, `shutup-and-run doctor --report` e a Configuração da página mostram `?cpf=***&email=***`; nenhum mostra `00000000000`. `shutup-and-run config get poll_url --reveal` mostra a URL inteira. Salvar o formulário da página sem mexer mantém a URL.
- [ ] `shutup-and-run config set poll_url "http://IP_DA_SUA_VPS:8080/nope" --json`, depois `shutup-and-run config check --json`: a linha `poll` é `POLL_HTTP_ERROR` com `HTTP 404`, não `POLL_OK`. Volte a URL certa com `--apply`.
- [ ] 0.2.10: `shutup-and-run status --json` tem `last_poll_at` (segundos atrás, não minutos) e `last_poll_status` (`nothing` com a fila vazia). `shutup-and-run doctor --json`: toda linha que não está ok tem um `hint`, e `fix` é o mesmo texto. Depois de `config set language pt --apply`, a página em `http://127.0.0.1:8787/` abre em português num navegador que nunca apertou o botão de idioma.
- [ ] `shutup-and-run wsl install --poll-interval-secs 5` sai com 3 (`$LASTEXITCODE`), diz `at least 30`, e não muda nada.

O mesmo cadastro, fila e callback contra o n8n, sem o mock: [examples/n8n/README.md](../examples/n8n/README.md#7-smoke-test).

### Limites de PC compartilhado

Esta parte só rodou no Linux e em testes unitários. No PC Windows:

```powershell
shutup-and-run wsl limits
Get-Content $env:USERPROFILE\.wslconfig
shutup-and-run status --json
```

- [ ] `wsl limits` imprime a RAM e os processadores deste PC, e um orçamento de cerca de 40% da RAM e metade dos processadores. Anote se a instalação escreveu o `.wslconfig`, ou por que não escreveu (já havia um arquivo, ou existe outra distribuição).
- [ ] `status --json` tem `"limits"` com `"profile":"shared-pc"`, `"max_job_cpus":1.0`, e `"waiting_for":null`.
- [ ] Com uma tarefa do mock rodando: `wsl -d shutup-and-run -u root docker inspect -f '{{.HostConfig.NanoCpus}} {{.HostConfig.Memory}} {{.HostConfig.MemorySwap}}' $(wsl -d shutup-and-run -u root docker ps -q)` imprime `1000000000` e o mesmo número duas vezes (2147483648 num PC de 16 GiB).
- [ ] Inicie o mock com `--memory 8g`: nenhuma tarefa começa, e `shutup-and-run wsl logs` tem `poll task rejected`.
- [ ] `shutup-and-run config set min_free_memory 900g --apply`: `status` mostra `waiting_for: low on memory`, o mock não recebe mais polls, e a linha `resources` do doctor é um aviso com `fix:`. `shutup-and-run config unset min_free_memory --apply` e os polls voltam.
- [ ] Com um `.wslconfig` seu, com outras chaves e `memory=64GB`: `wsl limits` sai com 1; `wsl limits --apply` muda só `memory=` e `processors=`, e `.wslconfig.bak` é o arquivo antigo. O doctor diz `not in effect yet` até o PC reiniciar ou você mesmo rodar `wsl --shutdown`.

### 0.2.11: espera por ociosidade, pergunta ao desligar, tarefas interrompidas

Nada disto rodou num PC Windows de verdade antes da release: o CI compila e roda os testes unitários, e não consegue encerrar uma sessão.

- [ ] `http://127.0.0.1:8787/` tem **Recursos do computador**: "Este PC (Windows)" com processadores e memória do PC, "WSL2" com números menores, o limite por tarefa, "PC sem uso há" contando enquanto você não mexe no PC, e "Pode começar tarefa nova? sim".
- [ ] `shutup-and-run config set min_idle_secs 120 --apply`. Enquanto você usa o PC: `shutup-and-run status` tem `waiting_for: the PC is in use`, a página diz "esperando: o PC está em uso", e o mock não recebe polls. Deixe o PC parado por dois minutos: os polls voltam e uma tarefa roda. Mexa no mouse enquanto ela roda: a tarefa continua. `shutup-and-run config unset min_idle_secs --apply`.
- [ ] Nenhuma janela apareceu em momento algum (a linha `Get-Process` de [WINDOWS.md](WINDOWS.md#for-the-person-at-the-pc)).
- [ ] Sem tarefa rodando, Iniciar > Desligar > Reiniciar: o PC reinicia sem perguntar nada.
- [ ] Com uma tarefa de cinco minutos rodando (seção 5), Iniciar > Sair: o Windows mostra `shutup-and-run` e "Há uma tarefa em execução neste PC..." (com `language` `pt`). Aperte **Cancelar**: `shutup-and-run wsl logs` tem `the shutdown was cancelled; the jobs go on`, e a tarefa termina bem depois (`ok` no mock).
- [ ] O mesmo, e aperte **Sair mesmo assim**. Entre de novo: `shutup-and-run jobs` mostra essa tarefa como `interrupted`, o mock imprimiu uma linha para ela com `reason=interrupted`, a página conta 1 tarefa interrompida, e nenhum container foi iniciado de novo para ela (`wsl -d shutup-and-run -u root -- docker ps -a` vazio).
- [ ] Com uma tarefa rodando e o mock parado, reinicie o PC. Suba o mock de novo dentro de uma hora: o callback chega numa tentativa posterior, e "Entrega de callbacks" na página volta a 0 aguardando.

### 0.2.13: segurança de recursos e a autorização do dono

Automatizado no Linux: `cargo test` cobre admissão saudável, cada leitura crítica, leituras ausentes, o consentimento e o seu alcance, as respostas não interativas, o disjuntor e o fim dele, OOM contra um kill comum, throttling e o perfil `none`, tudo com leituras simuladas. Contra um Docker real, sem estressar a máquina:

```bash
cargo build --release --locked
sudo bash tests/safety/linux.sh target/release/shutup-and-run
```

Ele prende o próprio worker a dois processadores para que veja um computador pequeno, autoriza por um pseudo-terminal como uma pessoa faria, segura uma tarefa em um quinto de processador, para e reinicia o `dockerd` sob uma tarefa em execução e estoura três vezes um limite de 16 MiB. 34 verificações; cerca de um minuto e meio. Não está no CI: termina com o disjuntor aberto.

| Simulado no Linux | Como | Observado |
| --- | --- | --- |
| Máquina pequena | worker preso a 2 de 4 processadores | `authorization_required`, push `503`, nenhum container |
| Autorização não interativa | `--json`; a palavra enviada por pipe, sem terminal | saída 1, `AUTHORIZATION_REQUIRED`, nada mudou |
| Pessoa em um terminal | pseudo-terminal, palavra errada, depois a palavra | não; depois registrado com canal `terminal`, e as tarefas começam |
| Pouca memória, CPU saturada, disco cheio, leituras velhas ou ausentes | leituras simuladas nos testes unitários | `503` `SAFETY_BLOCKED` com o código; nada gravado; libera depois da linha de recuperação |
| Throttling de CPU | tarefa real com `cpu: 0.2` por 40 s | saída 0, um incidente `cpu_throttled` (99% do tempo) |
| Falha do daemon do Docker | `dockerd` parado sob uma tarefa em execução, depois iniciado | push `502`, worker vivo, `state: failed`; tarefas aceitas de novo sem reiniciar; a tarefa perdida fechada uma vez |
| Kill comum | `docker kill` | saída 137, sem `oom_killed` |
| OOM de container, repetido | tarefa real, `memory: 16m`, `tail /dev/zero`, três vezes | saída 137, `oom_killed: true`, um incidente cada; depois `SAFETY_OOM_BREAKER`, que uma autorização não libera |

Não simulado no Linux: falta real de memória ou disco no host (só as leituras são falsas), e tudo do lado do Windows.

#### Um PC Windows pequeno: 2 processadores, 5 GB

Uma VM Windows 11 com 2 vCPU e 5 GB de RAM, com virtualização aninhada. Nada abaixo foi executado no Windows: as leituras de CPU e disco do supervisor são novas e só foram compiladas. **Não considere esta configuração segura até que os números abaixo digam isso.** Deixe o Gerenciador de Tarefas (Desempenho) aberto e anote os valores.

1. Antes de instalar, com a VM ociosa por dois minutos: memória **disponível** e % de CPU no Gerenciador de Tarefas.

   | | Memória disponível | CPU | Sensação |
   | --- | --- | --- | --- |
   | Ociosa, antes de instalar | | | |
   | Instalado, sem tarefa | | | |
   | Uma tarefa rodando | | | |

2. Instale (seção 3), com o mock da seção 2 servindo três tarefas.
   - [ ] A instalação termina com saída 0. O doctor tem uma linha `--  safety` com `SAFETY_SMALL_MACHINE` e uma `hint:` que diz para não responder pelo dono.
   - [ ] `http://127.0.0.1:8787/` diz **ESPERANDO A SUA AUTORIZAÇÃO** (ou em inglês) e "computador pequeno". O mock imprimiu `WORKER` no máximo uma vez e nenhum `TASK`.
   - [ ] `shutup-and-run safety --json` tem `"admission":"authorization_required"`; o `detail` do motivo traz os processadores e a memória reais deste PC.
   - [ ] `shutup-and-run safety authorize` sai com 1 e `AUTHORIZATION_REQUIRED`, e nada muda. O mesmo com `--json`.
3. Na página, abra **Segurança deste computador**.
   - [ ] Os riscos, o aviso (ele diz que as proteções não eliminam o risco, e que autorizar não muda as responsabilidades do fornecedor), uma caixa, um botão. O link no canto superior direito mostra a página só em um idioma por vez.
   - [ ] O botão sem a caixa não faz nada (o navegador pede a caixa).
   - [ ] Marque a caixa, **Autorizar**: "Autorizado." A página volta para **RODANDO**, o mock imprime `TASK` e a tarefa termina `ok`. Preencha as duas últimas linhas da tabela enquanto uma tarefa roda. O mouse está suave, uma aba do navegador abre em tempo normal?
   - [ ] As duas tarefas seguintes começam sem nova pergunta. `shutup-and-run safety --json` tem `"authorization"` com `"channel":"page"`.
4. Pressão de memória no PC. Em uma janela do PowerShell (ocupa cerca de 1,5 GB e segura):

   ```powershell
   $hold = @(); 1..15 | ForEach-Object { $b = New-Object byte[] 100MB; for ($i = 0; $i -lt $b.Length; $i += 4096) { $b[$i] = 1 }; $hold += ,$b }
   ```

   - [ ] Quando o Gerenciador de Tarefas mostrar menos de 1 GB disponível: em até 15 segundos a página diz **TAREFAS NOVAS BLOQUEADAS POR SEGURANÇA**, memória em nível crítico, "this PC: ... available". O mock não recebe polls. Uma tarefa que já estava rodando continua e termina.
   - [ ] O botão Autorizar não é oferecido para isso, e `shutup-and-run safety authorize` diz que nada está esperando.
   - [ ] Feche essa janela do PowerShell. Nada muda até haver 1,5 GB disponível de novo; então a página volta sozinha e os polls retornam. "Últimos incidentes" tem `blocked` e `recovered`.
   - [ ] Se o Gerenciador de Tarefas nunca mostrou 1 GB disponível, nem antes do script, este PC fica bloqueado a maior parte do tempo: anote. Essa é a resposta para "5 GB bastam?", não um defeito.
5. Saturação do processador. `1..2 | ForEach-Object { Start-Job { while ($true) {} } }`.
   - [ ] Em cerca de 20 segundos: bloqueado, "O processador está saturado", "this PC: processors 9x% busy". `Get-Job | Remove-Job -Force`: volta sozinho quando ficar abaixo de 85%.
6. Uma tarefa que estoura a memória. Reinicie o mock com `--tasks 3 --image alpine --command "tail /dev/zero" --memory 64m --expect-failures 3`.
   - [ ] Cada uma termina em segundos. `shutup-and-run jobs` diz "ran out of memory ... (confirmed by Docker)" para cada; a memória disponível no Gerenciador de Tarefas não caiu mais que os 64 MB da tarefa.
   - [ ] Depois da terceira: bloqueado, "Tarefas estouraram a memória repetidamente". Quinze minutos depois termina sozinho.
7. Na página, **Retirar a autorização**: **ESPERANDO A SUA AUTORIZAÇÃO** de novo, e o mock não recebe polls.
8. Nenhuma janela apareceu em momento algum, e `shutup-and-run wsl logs` não tem linha com token nem com o env de uma tarefa.

O que relatar: a tabela, quais caixas falharam, e se os passos 4 e 5 bloquearam e se recuperaram nos valores que o Gerenciador de Tarefas mostrou. Se o PC ocioso já fica abaixo de 1 GB disponível, ou a VM fica lenta com uma tarefa nos limites padrão, este PC não deve emprestar recursos, autorize o dono o que autorizar.

### 0.2.15: atualizar por cima de um worker rodando

O CI instala a última release no WSL2 do runner Windows e a atualiza, sem job, pausa ou autorização. Isto é o resto, num PC que roda uma release anterior com um job.

- [ ] Com um job de cinco minutos rodando, rode a linha de instalação. Ela mostra `stopping the worker`, depois os passos do `wsl update`, e termina com `updated to`. O callback do job chega dizendo como ele terminou (parado, não `ok`).
- [ ] `shutup-and-run version --json` e o `status.json` mostram a mesma `version` nova; `state` é `idle`; a linha `version` do doctor está ok.
- [ ] `C:\ProgramData\shutup-and-run` não tem `*.tmp`, `SHA256SUMS` nem `shutup-and-run.exe.old`.
- [ ] `shutup-and-run config show` está como antes (URL de poll, slots, nome do worker), e o mock vê o mesmo token.
- [ ] Um worker pausado antes do update continua pausado; um PC cujo dono tinha autorizado continua autorizado.
- [ ] Coloque o binário antigo de volta à mão (`wsl -d shutup-and-run -u root -- cp ...`): o doctor diz `VERSION_MISMATCH` com a dica `shutup-and-run wsl update`, e esse comando resolve.

### 0.2.19: bateria, tela cheia, horário, consumo, consentimento do contrato

Num notebook Windows, com o mock servindo jobs curtos (`--image alpine --command "sh -c 'yes > /dev/null & sleep 90'"`).

- [ ] Na tomada: a linha **Energia** da página diz na tomada. Tire da tomada acima de 60%: aparece o quadro **Este PC está na bateria** com a carga, e os jobs ainda começam. O doctor tem uma linha `lending` com `ON_BATTERY`.
- [ ] Deixe descarregar abaixo de 60% (ou teste quando estiver assim): a página diz **ESPERANDO**, o mock não recebe poll, um job que já roda termina normalmente, e o `shutup-and-run doctor` diz `BATTERY_LOW`. Ligue na tomada: os polls voltam em até um minuto.
- [ ] Inicie uma apresentação do PowerPoint (ou um vídeo em tela cheia): **ESPERANDO**, "apresentando ou com um programa em tela cheia", sem polls. Saia: os polls voltam. Uma chamada do Teams em janela não é espera; anote se uma em tela cheia é.
- [ ] `shutup-and-run config set lend_hours "mon-fri 19-07" --apply` num horário fora dela: **ESPERANDO**, `OUTSIDE_LEND_HOURS`, e a linha **Empresta** mostra a janela e a hora agora, que é o relógio do PC. `config unset lend_hours --apply`: os polls voltam.
- [ ] Depois de um job de 90 segundos, `shutup-and-run jobs` tem `used: cpu ... s, memory peak ...`, cerca de um minuto de CPU e alguns MB; `jobs --json` tem `cpu_ms` e `memory_peak_bytes`; a tabela de tarefas da página mostra o mesmo em **CPU / memória**.
- [ ] Num PC pequeno (ou num em que a página pede autorização), `shutup-and-run wsl uninstall` e depois `shutup-and-run wsl install --poll-url URL --owner-consent`: ele mostra `recorded the owner's consent from the contract`, a página fica **RODANDO** e nunca pergunta, e **Segurança** mostra a autorização pelo `contract`. **Retirar** na página: volta a perguntar.

Resultado da rodada 3 (0.2.20, notebook na bateria, modo gerenciado): ok a bateria acima de 60% (quadro, linha **Energia**, `ON_BATTERY`, job começou com 95%) e abaixo (`BATTERY_LOW` com o Windows em 58%: a carga do `GetSystemPowerStatus` pode ficar 1% atrás do `Win32_Battery`, e vale a primeira); ok o `used:` (`cpu 173.4 s, memory peak 1.1 GiB`, igual no `jobs --json` e na página) e o `jobs logs --last`; `USER_PRESENTING` só visto no log. Ficaram para a próxima rodada: um job rodando ao cruzar 60%, voltar à tomada, Teams em tela cheia, `lend_hours` e `--owner-consent`.


O que nenhum CI alcança num PC Windows real desde então (o relógio do Windows, a pausa por um tempo, a memória e os programas que a ocupam, o cache do WSL2) está em [WINDOWS-PENDING.md](WINDOWS-PENDING.md).

### Repasse de certificado e as demos

Ainda não rodou no Windows. No PC Windows, com um `.pfx` de teste (nunca um real para isto) numa pasta com espaço e acento no nome:

```powershell
shutup-and-run version --json
shutup-and-run config check
shutup-and-run demo
shutup-and-run demo sign
$env:SHUTUP_AND_RUN_CERT_PASSWORD = "a-senha-de-teste"
shutup-and-run certificate set --file "C:\Users\VOCE\Área de Trabalho\cert teste.pfx" --accept-risk --apply
shutup-and-run demo sign --configured
```

- [ ] `version --json` diz `"platform":"windows"` e `"runtime":"wsl2"`.
- [ ] `demo` e `demo sign` terminam com `passed`, e `shutup-and-run-demo-signed.pdf` está na pasta atual do Windows e abre.
- [ ] Sem `--accept-risk`, `certificate set` imprime o aviso e sai com 2. Com senha errada sai com 3 e `CERTIFICATE_PASSWORD_WRONG`; com um `.txt` renomeado para `.pfx`, `CERTIFICATE_INVALID`; com um caminho que não existe, o caminho e saída 1.
- [ ] Depois do `set`: `certificate status` diz ON; `shutup-and-run config show` tem `certificate_enabled` e nenhuma senha; `shutup-and-run doctor --report` tem `certificate passthrough: ON` e não tem a senha.
- [ ] Na página local, Configuração avançada, Certificado: o mesmo com o seletor de arquivo; salvar sem marcar a caixa é recusado; o campo de senha volta vazio.
- [ ] Enfileire duas tarefas no mock (`--max-slots 2`) com o comando `sh -c 'ls -l $SHUTUP_AND_RUN_CERT_PATH; echo x >> $SHUTUP_AND_RUN_CERT_PATH || echo read-only'`: as duas listam o arquivo e imprimem `read-only`.
- [ ] Com uma tarefa longa rodando, `certificate disable --apply`: a tarefa termina; a próxima não tem `/run/shutup-and-run/certificate`.
- [ ] `wsl -d shutup-and-run -u root ls /run/shutup-and-run/jobs` fica vazio sem tarefa rodando, também depois de uma tarefa que estourou o tempo e depois de `wsl stop` / `wsl start`.
- [ ] `shutup-and-run wsl reset`: a instalação diz que o repasse foi desligado; `certificate status` diz off.

## 5. Windows 11: o atalho, parar, desligar

Ponha o atalho na área de trabalho:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/shutup-and-run.bat -OutFile "$env:USERPROFILE\Desktop\shutup-and-run.bat"
```

Dê dois cliques nele.

- [ ] `[RODANDO / RUNNING]  Tarefas agora / Jobs now: 0` e `PODE DESLIGAR O PC. / YOU CAN TURN THE PC OFF.`

Para ver a outra resposta, dê ao worker uma tarefa longa. Na VPS, pare o mock (Ctrl+C) e inicie com uma tarefa que dorme cinco minutos:

```bash
python3 poll_mock.py --require-approval --image ghcr.io/nailtongomes/shutup-and-run-smoke --command "sleep 300" --timeout 600 --tasks 1 --keep-serving
```

Quando o mock imprimir `TASK mock-...-1`, espere meio minuto pelo download da imagem e dê dois cliques no atalho.

- [ ] `Jobs now: 1` e `NAO DESLIGUE AGORA. / DO NOT TURN OFF NOW.` Tecle `P`.
- [ ] `[PARADO / STOPPED]  PODE DESLIGAR O PC.` em uns 15 segundos.
- [ ] Dois cliques de novo: ele inicia o serviço e diz `RUNNING`. O mock então imprime `FAIL 1/1` para o job interrompido, com `exit` diferente de zero.

Inicie de novo uma tarefa longa, saia do Windows (logoff) enquanto ela roda e entre de novo.

- [ ] `shutup-and-run wsl logs` tem `the session is ending; stopping the jobs`, e o mock imprimiu uma linha `FAIL` para esse job.

Reinicie o PC e faça logon.

- [ ] Sem mexer em nada, o atalho diz `RUNNING`.
- [ ] `wsl -d shutup-and-run -u root -- docker ps -a` não lista container sobrando.

## 6. Remover

```powershell
shutup-and-run wsl uninstall --purge
```

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash -s -- --uninstall --purge
```

- [ ] `wsl -l -v` não lista mais `shutup-and-run`, e a tarefa agendada sumiu. Feche a porta 8080 de novo.

## Se algo falhar

Mande isto:

```powershell
shutup-and-run wsl doctor
shutup-and-run wsl logs --lines 300
wsl --version
```

```bash
journalctl -u shutup-and-run -n 200 --no-pager
```

Nenhum dos dois logs contém token.

### 0.2.14: containers gerenciados

Automatizado no Linux (`cargo test managed`, com runtime simulado e relógio falso): validação da definição, persistência, intervalo (de início a início, sem sobreposição, uma execução depois de intervalos perdidos), o backoff do `always` em uma hora simulada, iniciar e parar à mão, desativar, worker reiniciado, container que sumiu, falha ao iniciar, Docker sem responder, pouca memória, pausa, slots cheios, bloqueio de segurança, os dois modos e a recusa quando ocupado, a página e seus logins, e que um segredo não aparece em nada do que é mostrado.

Contra um Docker real, à mão, cerca de quatro minutos:

```bash
cargo build --release --locked
sudo bash tests/managed/linux.sh target/release/shutup-and-run
```

Não exercitado: nada no Windows. Lá os comandos são encaminhados para a distribuição WSL2, como `config` e `jobs`. Em um PC com Windows 11, confira à mão:

- [ ] `shutup-and-run managed apply C:\caminho\rpa-selenium.json`, depois `managed secret rpa-selenium API_TOKEN` com o valor por pipe, depois `managed mode on`: `managed list` mostra `running` e a página `http://127.0.0.1:8787/local/managed` concorda.
- [ ] Com ele rodando, saia da sessão do Windows: a tela de desligamento cita `shutup-and-run`, como em qualquer job.
- [ ] `managed mode off` com ele rodando é recusado com `MANAGED_BUSY`; depois de `managed stop`, é aceito e o worker volta a fazer poll.
- [ ] Um container desabilitado não começa, nem com `managed start` (é proposital). Para uma execução de um desabilitado: `managed enable NOME`, `managed start NOME`, `managed disable NOME`; a execução em curso termina.

O modo gerenciado também é o jeito mais fácil de testar um PC Windows sem URL de poll: os itens de [WINDOWS-PENDING.md](WINDOWS-PENDING.md) que precisam de uma tarefa rodando aceitam um container `once` iniciado com `managed start`.
