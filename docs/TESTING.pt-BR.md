# Teste manual de uma release

English: [TESTING.md](TESTING.md).

Uma VPS Linux (sua, confiável) e um PC com Windows 11 (tratado como PC de cliente). Cerca de 30 minutos. Cada passo tem o comando e depois o que esperar. Se um passo não bater, pare ali e guarde a saída.

## 0. A release existe

As instalações de uma linha baixam da última release do GitHub.

- [ ] `https://github.com/nailtongomes/shutup-and-run/releases/latest` é a `v0.2.3` ou posterior e tem três arquivos: `shutup-and-run-x86_64-unknown-linux-musl`, `shutup-and-run-x86_64-pc-windows-msvc.exe`, `SHA256SUMS`.

Se não estiver lá, crie a tag no `main`: `git tag v0.2.3 && git push origin v0.2.3`, e espere o workflow `release`.

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
curl -fsSLO https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/examples/mock-poll.py
MOCK_PUBLIC_URL=http://IP_DA_SUA_VPS:8080 python3 mock-poll.py
```

- [ ] `mock poll URL on 0.0.0.0:8080`. Abra a porta 8080 no firewall durante o teste. Deixe esse terminal visível.

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

- [ ] Em até cinco minutos (o worker espaça as consultas enquanto é recusado) o mock imprime `TASK 1/3` e depois `CALLBACK ... "status":"exited","exit_code":0,"reason":"exited"`, três vezes, a cada 15 segundos. Depois nada: o mock responde `204`.
- [ ] `shutup-and-run wsl logs` mostra `poll started a job` três vezes.

Instale de novo com o mesmo comando:

- [ ] `kept this machine's poll token`, e o mock não imprime outro `PENDING`: é a mesma máquina.

## 5. Windows 11: o atalho, parar, desligar

Ponha o atalho na área de trabalho:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/shutup-and-run.bat -OutFile "$env:USERPROFILE\Desktop\shutup-and-run.bat"
```

Dê dois cliques nele.

- [ ] `[RODANDO / RUNNING]  Tarefas agora / Jobs now: 0` e `PODE DESLIGAR O PC. / YOU CAN TURN THE PC OFF.`

Para ver a outra resposta, dê ao worker uma tarefa longa. Na VPS, pare o mock (Ctrl+C) e inicie com uma tarefa que dorme cinco minutos:

```bash
MOCK_IMAGE=ghcr.io/nailtongomes/shutup-and-run-smoke MOCK_COMMAND="sleep 300" MOCK_TIMEOUT=600 MOCK_TASKS=1 MOCK_PUBLIC_URL=http://IP_DA_SUA_VPS:8080 python3 mock-poll.py
```

Quando o mock imprimir `TASK 1/1`, espere meio minuto pelo download da imagem e dê dois cliques no atalho.

- [ ] `Jobs now: 1` e `NAO DESLIGUE AGORA. / DO NOT TURN OFF NOW.` Tecle `P`.
- [ ] `[PARADO / STOPPED]  PODE DESLIGAR O PC.` em uns 15 segundos.
- [ ] Dois cliques de novo: ele inicia o serviço e diz `RUNNING`. O mock então imprime um `CALLBACK` do job interrompido, com `"status":"exited"` e `exit_code` diferente de zero.

Inicie de novo uma tarefa longa, saia do Windows (logoff) enquanto ela roda e entre de novo.

- [ ] `shutup-and-run wsl logs` tem `the session is ending; stopping the jobs`, e o mock recebeu um `CALLBACK` desse job.

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
