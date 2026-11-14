# Teste manual de uma release

English: [TESTING.md](TESTING.md).

Uma VPS Linux (sua, confiável) e um PC com Windows 11 (tratado como PC de cliente). Cerca de 30 minutos. Cada passo tem o comando e depois o que esperar. Se um passo não bater, pare ali e guarde a saída.

## 0. A release existe

As instalações de uma linha baixam da última release do GitHub.

- [ ] `https://github.com/nailtongomes/shutup-and-run/releases/latest` é a versão do `Cargo.toml` (`v0.2.8` quando isto foi escrito) ou posterior e tem três arquivos: `shutup-and-run-x86_64-unknown-linux-musl`, `shutup-and-run-x86_64-pc-windows-msvc.exe`, `SHA256SUMS`.

Se não estiver lá, crie a tag no `main`: `git tag v0.2.8 && git push origin v0.2.8` (a tag tem que ser a versão do `Cargo.toml`, porque o `.exe` baixa o binário Linux da própria versão), e espere o workflow `release`.

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

- [ ] Em até cinco minutos (o worker espaça as consultas enquanto é recusado) o mock imprime `TASK mock-...-1` e depois `ok   1/3 ... reason=exited exit=0`, três vezes, a cada 30 segundos, e então um `SUMMARY` com `ok 3`. Depois disso responde `204`.
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
