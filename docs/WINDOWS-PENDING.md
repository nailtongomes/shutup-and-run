# O que falta testar num Windows real

[TESTING.pt-BR.md](TESTING.pt-BR.md) tem o roteiro completo. Esta lista é só o que o CI não alcança: o runner do Windows instala, atualiza e faz rollback, mas não tem bateria, não tem tela, não tem programas de usuário abertos e não roda uma tarefa de verdade dentro do WSL2. Cada item diz o que conferir e onde. Ao terminar, anote o resultado (ok, falhou, com a saída) em [TESTING.pt-BR.md](TESTING.pt-BR.md) e apague daqui o que passou.

Um PC Windows 11 com WSL2, instalado ou atualizado para a 0.2.20 com `irm .../install.ps1 | iex` (`shutup-and-run version` diz 0.2.20), a página em `http://127.0.0.1:8787/` e o mock de poll do roteiro ([TESTING.pt-BR.md, seção 2](TESTING.pt-BR.md#2-uma-url-de-poll-para-testar)) servindo jobs curtos.

## Da 0.2.19 (ainda não testado)

A lista está em [TESTING.pt-BR.md, "0.2.19"](TESTING.pt-BR.md#0219-bateria-tela-cheia-horário-consumo-consentimento-do-contrato): bateria acima e abaixo de 60%, apresentação ou tela cheia, `lend_hours`, `used:` no `jobs`, e `wsl install --owner-consent`.

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
- [ ] `C:\ProgramData\shutup-and-run\status.json` tem o objeto `memory` (`short_of`, `to_free_bytes`, `waited_today_secs`) e **nenhum** nome de programa. O log do supervisor também não tem.
- [ ] Com memória sobrando, nenhuma lista de programas aparece na página.
- [ ] Depois de alguns minutos esperando, a linha **Esperou por memória** mostra "hoje N min", e o número continua depois de reiniciar o serviço (`shutup-and-run wsl stop` e `wsl start`).
- [ ] Deixe o PC um dia inteiro instalado, com a memória curta em horário de uso: no dia seguinte a linha **Memória curta, 7 dias** mostra as horas (por exemplo `mon-fri 09-18`) e o doctor tem `MEMORY_SHORT_HOURS` com um `lend_hours` sugerido. Confira que esse texto passa no `shutup-and-run config set lend_hours "..."`, e não aplique sem o dono concordar.

### O WSL devolvendo a memória

- [ ] Rode uma tarefa que encha o cache do Linux, por exemplo `sh -c 'dd if=/dev/zero of=/tmp/x bs=1M count=1500; cat /tmp/x > /dev/null; sleep 5'`. Durante ela, o processo **VmmemWSL** do Gerenciador de Tarefas cresce. Quando ela termina e não há outra rodando, o log do serviço tem `gave the memory of the last job back to Windows` e o VmmemWSL cai em até um minuto.
- [ ] Num `.wslconfig` sem `autoMemoryReclaim`, `shutup-and-run wsl doctor` tem a linha `wsl-limits` com `WSL_MEMORY_NOT_RECLAIMED`. `shutup-and-run wsl limits --apply` acrescenta `autoMemoryReclaim=dropcache` em `[experimental]`, mantém o resto e cria o `.wslconfig.bak`; ele não muda um `memory=` que o dono deixou abaixo do orçamento.
- [ ] Depois de `wsl --shutdown` (num momento sem tarefa e sem outro uso do WSL), confira que o WSL ainda sobe e que o `autoMemoryReclaim` é aceito pela versão instalada: `wsl --version` e nenhum aviso de chave desconhecida ao abrir a distribuição. Anote a versão do WSL. Se a sua versão aceitar a chave em `[wsl2]` e não em `[experimental]`, registre isso.
- [ ] Um `.wslconfig` com `autoMemoryReclaim=disabled` (escolha do dono) fica como está depois do `--apply`.
