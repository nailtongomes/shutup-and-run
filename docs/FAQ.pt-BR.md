# Perguntas frequentes

[English](FAQ.md)

O que esperar de um worker no PC de um cliente, em respostas curtas. Cada uma diz onde conferir. Os detalhes estão no [README](../README.pt-BR.md), no [WINDOWS.md](WINDOWS.md) e, para um agente, no [FOR-AGENTS.md](FOR-AGENTS.md).

**Conteúdo**

- O PC: [Vai ficar lento?](#o-pc-vai-ficar-lento) · [Quanto uma tarefa ocupa?](#quanto-do-pc-uma-tarefa-ocupa) · [Notebook na bateria](#o-que-acontece-com-um-notebook-na-bateria) · [Reuniões e apresentações](#e-durante-uma-reunião-ou-apresentação) · [Falta memória](#o-que-fecho-quando-falta-memória) · [Só em alguns horários](#dá-para-emprestar-o-pc-só-à-noite) · [Meus arquivos](#uma-tarefa-consegue-ver-meus-arquivos)
- Uma tarefa rodando: [É pausada e retomada?](#um-container-em-execução-é-pausado-e-retomado) · [O que pode afetá-la?](#o-que-pode-afetar-uma-tarefa-que-já-está-rodando) · [Picos de CPU ou memória](#existe-tolerância-para-um-pico-curto-de-cpu-ou-memória) · [Desligar o PC](#posso-desligar-o-pc-com-uma-tarefa-rodando) · [Internet caiu](#e-se-a-internet-cair) · [Repetição](#uma-tarefa-que-falhou-roda-de-novo)
- Quando nada acontece: [Por que nenhuma tarefa começou?](#por-que-nenhuma-tarefa-começou) · [Esperando a sua autorização](#o-que-significa-esperando-a-sua-autorização) · [Parar de emprestar por um tempo](#como-paro-de-emprestar-por-um-tempo)
- Depois: [O que a tarefa imprimiu](#onde-vejo-o-que-uma-tarefa-imprimiu) · [Quanto a tarefa gastou](#quanto-uma-tarefa-gastou) · [O que o operador fica sabendo](#o-que-a-url-de-poll-fica-sabendo-deste-pc) · [Atualizar](#como-atualizo) · [Desinstalar](#como-desinstalo)

## O PC

### O PC vai ficar lento?

Pode ficar, um pouco; o worker foi feito para o trabalho da pessoa vir primeiro. No Windows as tarefas rodam dentro do WSL2, que pode usar no máximo cerca de 40% da RAM e metade dos processadores. Cada tarefa fica limitada a 1 CPU e 2 GiB (menos num PC pequeno), uma de cada vez. Nenhuma tarefa começa com memória, disco ou processador em falta, e nenhuma começa num PC pequeno ou ocupado sem o dono concordar. Isso reduz o risco, mas não o elimina: uma tarefa que já está rodando mantém a parte dela até terminar.

Onde conferir: a página local `http://127.0.0.1:8787/`, **Recursos do computador**.

### Quanto do PC uma tarefa ocupa?

No máximo os limites dela: por padrão 1 CPU e 2 GiB de memória, sem swap além disso. Uma tarefa que pede mais é recusada, nunca cortada. O que uma tarefa terminada usou de fato também fica registrado: veja [Quanto uma tarefa gastou?](#quanto-uma-tarefa-gastou).

Onde conferir: `shutup-and-run config show` (os limites em vigor), `shutup-and-run jobs`.

### O que acontece com um notebook na bateria?

- Na tomada: nada muda.
- Na bateria com 60% ou mais: as tarefas ainda começam. A página mostra **Este PC está na bateria** com a carga, e o doctor diz `ON_BATTERY`.
- Na bateria abaixo de 60%: nenhuma tarefa nova começa e o worker não pede trabalho, até o PC ir para a tomada. A página diz **ESPERANDO**; o código é `BATTERY_LOW`.

Uma tarefa que já está rodando nunca é interrompida pela bateria. Os 60% são fixos; nenhuma configuração muda isso. Um PC sem bateria nunca espera por isso.

### E durante uma reunião ou apresentação?

Enquanto o Windows disser que a pessoa está apresentando, ou com um programa em tela cheia (apresentação de slides, vídeo ou jogo em tela cheia, chamada em tela cheia), nenhuma tarefa nova começa. Quando isso acaba, as tarefas voltam sozinhas. Uma chamada em janela, fora da tela cheia, não é percebida. O código é `USER_PRESENTING`.

Para esperar também enquanto a pessoa digita ou usa o mouse: `shutup-and-run config set min_idle_secs 300 --apply` (cinco minutos sem uso).

### O que fecho quando falta memória?

Quando é a memória que impede a próxima tarefa, a página mostra **Falta memória** com quanto liberar, mais ou menos, e, no Windows, os programas que mais ocupam, somados por programa (`chrome 3.1 GiB (38 processos)`). Processos do próprio Windows, o antivírus, o WSL e o worker nunca aparecem. Feche um que não precisa agora, ou só espere: as tarefas voltam sozinhas. O worker nunca fecha nada. Quando quem está sem memória é o WSL2, ela está com uma tarefa em execução e volta quando ela termina.

A lista só é lida no PC enquanto a memória está apertada, e fica nele: não vai para o `status.json`, um log, o `doctor --report` nem para a URL de poll. O `shutup-and-run doctor` mostra (`MEMORY_TO_FREE`, e `MEMORY_HELD_BY_PROGRAMS` no Windows).

Quando a última tarefa termina, a VM do WSL2 descarta o cache que ela encheu, e o Windows recebe essa memória de volta na hora, não só quando o WSL reiniciar; `shutup-and-run wsl limits --apply` também põe `autoMemoryReclaim=dropcache` no `.wslconfig` pelo mesmo motivo.

A página também diz quanto tempo as tarefas novas esperaram por memória hoje e em 7 dias, e as horas da semana em que ela ficou curta. Quando essas horas caem dentro do horário em que o PC é emprestado, o doctor sugere um `lend_hours` que as evita (`MEMORY_SHORT_HOURS`); nada muda sozinho.

### Dá para emprestar o PC só à noite?

Dá, com `lend_hours`, no relógio do próprio PC (no Windows, o relógio que o Windows mostra, com o fuso dele):

```
shutup-and-run config set lend_hours "mon-fri 19-07; sat,sun" --apply
```

Isso é: noites de dias úteis e o fim de semana inteiro. Os dias são `mon` a `sun`. Uma janela cujo fim vem antes do início passa da meia-noite e pertence ao dia em que começa (`fri 19-07` termina no sábado às 7). Fora das janelas nenhuma tarefa nova começa, o worker não pede trabalho e a página diz **ESPERANDO** (`OUTSIDE_LEND_HOURS`). Uma tarefa em execução continua depois do fim da janela. `config unset lend_hours --apply` volta a emprestar a qualquer hora. A página e o `shutup-and-run doctor` mostram a janela e a hora agora com o fuso (`now wed 21:00, UTC-03:00`), então um relógio errado aparece logo.

### Uma tarefa consegue ver meus arquivos?

Não. Uma tarefa não recebe nenhuma pasta do PC: nenhum mount, nenhum drive do Windows (o WSL2 é instalado sem eles), nenhuma porta. Ela vê o próprio container e a rede. A única exceção é um certificado que o dono decide compartilhar, que toda tarefa passa a poder ler; isso fica desligado até o dono ligar, no próprio PC.

## Uma tarefa rodando

### Um container em execução é pausado e retomado?

Não. O worker nunca pausa, congela ou retoma um container. Uma tarefa roda do início até o fim ou até o tempo limite, de uma vez. O que pode parecer pausa:

- **O limite de CPU.** Uma tarefa que quer mais que o limite de CPU é freada pelo kernel, em fatias de 100 ms. Ela não para. Se isso acontece na maior parte do tempo, fica registrado como incidente (`cpu_throttled`).
- **O botão de pausa** (ou `shutup-and-run pause`). Ele só impede tarefas novas; uma tarefa rodando vai até o fim.
- **Suspensão ou hibernação do PC.** O WSL2 dorme junto com o Windows, e a tarefa fica congelada com todo o resto; ela continua quando o PC acorda. O tempo limite dela é contado no relógio, então depois de uma suspensão longa ela pode terminar como `timeout` logo depois de acordar.

### O que pode afetar uma tarefa que já está rodando?

Isto pode:

| O quê | O que acontece com a tarefa | Como é informado |
| --- | --- | --- |
| O tempo limite (`timeout_secs`) | é encerrada | `reason` `timeout` |
| O limite de memória | o kernel encerra na hora | `oom_killed: true` |
| O limite de CPU | fica mais lenta, não para | um incidente `cpu_throttled` |
| O PC desliga, reinicia ou sai da sessão | o Windows pergunta antes enquanto uma tarefa roda; "desligar assim mesmo" encerra | `reason` `interrupted`, informado uma vez, não roda de novo |
| Queda de energia, travamento do PC ou do Docker | ela termina | `interrupted` |
| `shutup-and-run wsl stop`, uma atualização, uma desinstalação | SIGTERM, 10 segundos, depois encerrada | `interrupted`, ou como ela saiu |
| Suspensão ou hibernação | congela e depois continua | veja acima |

Isto **não** interrompe uma tarefa em execução: a pessoa voltar a usar o PC, a memória ou a CPU do PC ficarem ocupadas, um bloqueio de segurança, uma autorização retirada, a bateria passar abaixo de 60%, uma apresentação começar, o fim do `lend_hours`, o botão de pausa, ou o serviço do próprio worker reiniciar (o container continua rodando e é retomado pelo serviço). Tudo isso só impede tarefas novas.

### Existe tolerância para um pico curto de CPU ou memória?

Para uma tarefa que já está rodando, não há o que tolerar: nenhuma leitura do PC a interrompe. Valem só os limites dela, e esses são rígidos: memória acima do limite é encerrada na hora, CPU acima do limite é freada.

Para uma tarefa nova, as leituras são feitas no momento em que ela começaria (e antes de cada poll), não acompanhadas no meio do caminho:

- O uso de CPU do PC é a média dos últimos 10 segundos; dentro do WSL2 ou no Linux é a carga média do último minuto. Um pico de um ou dois segundos quase não mexe em nenhuma das duas.
- Memória e disco são o que está livre naquele momento.
- Um pico entre dois desses momentos não muda nada. Um pico exatamente nesse momento faz a tarefa esperar, não falhar: um worker em pull simplesmente não pede trabalho e olha de novo no próximo poll, 30 segundos depois; um push recebe `503` e pode ser enviado de novo.
- Um nível crítico só passa depois de uma linha de recuperação, para o worker não ficar ligando e desligando em volta dele: os processadores do PC bloqueiam em 95% e liberam abaixo de 85%; a memória bloqueia abaixo de 10% disponível (ou 1 GiB) e libera em 15% (1,5 GiB); o disco bloqueia abaixo de 2 GiB livres e libera em 3 GiB.
- Três tarefas encerradas por memória em 30 minutos pausam as tarefas novas por 15 minutos.

Não existe regra de "precisa durar N segundos" além dessas médias, e os limiares são fixos.

### Posso desligar o PC com uma tarefa rodando?

Melhor não. Enquanto uma tarefa roda, o Windows mostra a tela de sempre, "este aplicativo está impedindo o desligamento", com o motivo. O melhor é esperar. Se desligar assim mesmo, a tarefa é encerrada e informada como `interrupted`, uma vez, e não roda de novo: o serviço que a enviou decide o que fazer. A página e o atalho da área de trabalho dizem se o PC pode ser desligado agora.

### E se a internet cair?

Uma tarefa em execução continua, a não ser que ela mesma precise da rede. O worker continua pedindo trabalho e falha em silêncio até a rede voltar. O resultado de uma tarefa que terminou é enviado de novo uma vez por minuto, por até uma hora; depois disso é dado como perdido, e a página e o `status` contam.

### Uma tarefa que falhou roda de novo?

Não. Uma tarefa roda uma vez. Como ela terminou vai para o callback, uma vez, e o serviço que a enviou decide se manda de novo. O worker não guarda fila.

## Quando nada acontece

### Por que nenhuma tarefa começou?

A primeira linha da página diz o porquê, assim como `shutup-and-run doctor` e `shutup-and-run status --json` (`waiting_for`). Os motivos comuns, com os códigos:

| A página diz | Código | Passa |
| --- | --- | --- |
| PAUSADO | `WORKER_PAUSED` | quando alguém retoma; uma pausa por um tempo, no fim dela; qualquer outra, também no próximo reinício do PC |
| ESPERANDO A SUA AUTORIZAÇÃO | `AUTHORIZATION_REQUIRED` | quando o dono decide, veja abaixo |
| TAREFAS NOVAS BLOQUEADAS POR SEGURANÇA | `SAFETY_*` | sozinho, quando o PC se recupera |
| ESPERANDO | `OUTSIDE_LEND_HOURS`, `BATTERY_LOW`, `USER_PRESENTING` | sozinho |
| esperando: o PC está em uso | `USER_ACTIVE` | sozinho (`min_idle_secs`) |
| esperando, recursos insuficientes | `LOW_MEMORY`, `LOW_DISK` | sozinho; **Falta memória** diz quanto liberar |
| ocupado, todas as vagas em uso | `WORKER_BUSY` | quando a tarefa em execução termina |
| máquina ainda não aprovada | `POLL_NOT_APPROVED` | quando a URL de poll aprova este PC |
| DOCKER PAROU | `DOCKER_UNAVAILABLE` | siga a dica do doctor |

Enquanto espera, o worker não pede trabalho, então nenhuma tarefa é retirada e depois descartada. Cada código tem um próximo passo de uma linha no [FOR-AGENTS.md](FOR-AGENTS.md).

### O que significa "esperando a sua autorização"?

O PC é pequeno (menos de 8 GiB de memória ou no máximo 2 processadores), ou está ocupado, ou o Windows não está informando os dados dele, então rodar uma tarefa é um risco real para quem o usa. O dono lê os riscos uma vez na página (**Segurança**) e decide; `shutup-and-run safety authorize` num terminal faz o mesmo. O sim fica guardado, e só é pedido de novo para um risco novo ou uma versão nova do aviso. Ele nunca libera um bloqueio.

Quando o dono já concordou no contrato, quem instala registra isso na própria máquina, e o PC nunca pergunta:

```
shutup-and-run wsl install --poll-url URL --owner-consent
shutup-and-run safety authorize --owner-consent      # num PC já instalado
```

O dono ainda pode retirar na página.

### Como paro de emprestar por um tempo?

O botão **Pausar** na página, ou `shutup-and-run pause`. Nenhuma tarefa nova começa; a que está rodando termina. `shutup-and-run resume`, ou o botão de novo, encerra a pausa; num PC com a página, um reinício também encerra, para uma pausa esquecida não durar para sempre.

Por um tempo: os botões **por 1 hora**, **por 3 horas** e **pelo resto do dia** (até a meia-noite no relógio do PC), ou `shutup-and-run pause --for 2h` (`30m`, `2h`, `1d`; no máximo uma semana). O worker volta a aceitar sozinho no fim, e a página diz quando. Essa pausa sobrevive a um reinício, porque não tem como ser esquecida. Para emprestar só em alguns horários, use `lend_hours`.

## Depois

### Onde vejo o que uma tarefa imprimiu?

`shutup-and-run jobs logs ID` (ou `--last`), ou **Tarefas e logs** na página, que também baixa o arquivo. Os últimos 256 KiB de cada tarefa ficam no PC por 7 dias, sem máscara, e nunca são enviados a lugar nenhum.

### Quanto uma tarefa gastou?

Cada tarefa terminada registra o tempo de CPU e o máximo de memória que ocupou: `shutup-and-run jobs` (uma linha `used:`), `jobs --json` (`cpu_ms`, `memory_peak_bytes`) e a coluna **CPU / memória** na página. Os números vêm de uma leitura a cada 30 segundos enquanto ela roda, então uma tarefa mais curta que isso pode não ter nenhum, e um pico curto pode escapar. `shutup-and-run jobs --summary` resume as últimas 20 e, com cinco medidas ou mais e espaço neste PC, sugere o `max_job_memory` e o `max_slots` que caberiam mais tarefas ao mesmo tempo; o `doctor` também diz (`ROOM_FOR_MORE_SLOTS`). Nada muda sozinho.

### O que a URL de poll fica sabendo deste PC?

Quando pede trabalho: o nome do worker, o sistema, o tipo de processador, a versão, quantas vagas tem e quantas estão livres, os processadores e a memória. Não a config, nenhum token, nem o histórico de tarefas, nem o que as tarefas imprimiram, nem o endereço. O que as tarefas gastaram também fica no PC.

### Como atualizo?

Rode a linha de instalação de novo (`irm .../install.ps1 | iex` no Windows). Ela para o worker, troca a versão, mantém a config, o token e o histórico, e põe a versão antiga de volta se a nova não iniciar. Detalhes: [WINDOWS.md](WINDOWS.md#update-and-repair).

### Como desinstalo?

`shutup-and-run wsl uninstall` remove a tarefa agendada e a distribuição WSL2 com os containers e o histórico; `--purge` também remove `C:\ProgramData\shutup-and-run`. No Linux, a desinstalação do [instalador](../README.pt-BR.md#instaladores).
