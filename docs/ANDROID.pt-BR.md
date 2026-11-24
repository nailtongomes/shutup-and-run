# Compartilhar a internet pelo celular (Android)

Passo a passo para quem vai deixar o celular emprestando a conexão. O celular empresta o Wi-Fi de casa e, só se você permitir, os dados móveis. Não precisa de root nem de app nosso: o programa roda no Termux, um app de terminal. Os detalhes técnicos e os riscos estão em inglês em [INTERNET.md](INTERNET.md#android-a-phone-in-termux).

O que você precisa receber do operador:

- o endereço do relay, como `relay.exemplo.com:7000`;
- o token do relay, se ele usar um;
- o login do proxy (`ID:SENHA`), se ele escolheu um.

Exemplos aqui usam valores fictícios; troque pelos que o operador passou.

## 1. Instalar os apps (uma vez)

1. Instale o **F-Droid** pelo site [f-droid.org](https://f-droid.org). O Termux da Play Store é outra versão e não serve.
2. No F-Droid, instale:
   - **Termux**;
   - **Termux:Boot**, que liga tudo de novo quando o celular reinicia. Abra uma vez depois de instalar;
   - **Termux:API** (opcional, mas recomendado): deixa o programa ver a bateria.
3. Confira que o celular é de 64 bits: no Termux, `uname -m` precisa mostrar `aarch64`. Quase todo celular vendido desde 2017 é.

## 2. Instalar e ligar (uma linha)

No Termux, cole a linha abaixo, com os valores do operador:

```sh
pkg install -y termux-api
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install-termux.sh \
  | SHUTUP_AND_RUN_RELAY=relay.exemplo.com:7000 SHUTUP_AND_RUN_RELAY_TOKEN=TOKEN bash
```

Ela baixa o programa, confere que o arquivo é o certo (se não for, não instala nada) e liga o compartilhamento. Termina com `connected to ...` quando o celular está conectado, ou diz o motivo.

Variáveis que podem ir antes de `bash`, separadas por espaço:

| Variável | Para quê |
| --- | --- |
| `SHUTUP_AND_RUN_INTERNET_LOGIN=ID:SENHA` | o login que o operador escolheu |
| `SHUTUP_AND_RUN_MOBILE_DATA=1` | emprestar os dados móveis também; só se você concordar em gastar o plano |
| `SHUTUP_AND_RUN_MAX_DAY=2g` | no máximo 2 GB por dia |

Se o Android perguntar se o Termux pode ignorar a otimização de bateria, **permita**.

Para atualizar, rode a mesma linha de novo: ela troca a versão antiga pela nova.

## 3. Não deixar o celular matar o programa

O Termux continua com a tela apagada enquanto a notificação dele estiver lá. O que o derruba é a economia de bateria do fabricante. Faça uma vez:

- **Todo celular:** Configurações, Apps, Termux, Bateria: **Sem restrições** (ou "Não otimizar"). O mesmo para o Termux:Boot e o Termux:API. Não feche o Termux arrastando na lista de apps recentes e não toque em "Exit" na notificação.
- **Samsung:** Configurações, Bateria, Limites de uso em segundo plano, **Apps que nunca entram em suspensão**: adicione Termux e Termux:Boot.
- **Xiaomi, Redmi, POCO:** nas configurações do app: **Início automático** ligado e Economia de bateria **Sem restrições**. Nos apps recentes, segure o Termux e toque no cadeado.
- **Motorola:** Configurações, Apps, Termux, Bateria: **Sem restrições**, e **Permitir uso em segundo plano** ligado.
- **Android 14 ou mais novo:** Opções do desenvolvedor, **Desativar restrições de processos filhos**. Em versões mais antigas, o operador pode fazer isso por um computador (o comando está no INTERNET.md, "phantom processes").

O site [dontkillmyapp.com](https://dontkillmyapp.com) tem o caminho para cada modelo.

## 4. Conferir

```sh
shutup-and-run internet status
```

Deve mostrar `connected to ...` e linhas como:

```text
network: Wi-Fi (wlan0); shares only on Wi-Fi
battery: 72%, on a charger; below 60% on its battery, new streams wait
```

Também dá para ver no navegador do próprio celular: `http://127.0.0.1:8788/`. A página tem os botões **Pausar 1 hora** e **Parar de compartilhar**.

Quando novas conexões esperam, e por quê (e um aviso que não para nada):

| O status diz | O que acontece | O que fazer |
| --- | --- | --- |
| `INTERNET_NOT_ON_WIFI` | o celular saiu do Wi-Fi e você não liberou os dados móveis | voltar ao Wi-Fi; volta sozinho |
| `BATTERY_LOW` | bateria abaixo de 60% e fora do carregador (só com o Termux:API) | ligar no carregador; volta em até 1 minuto |
| `TERMUX_API_MISSING` ou `TERMUX_API_NO_ANSWER` | a bateria não é lida, então a bateria fraca não segura nada (compartilha até descarregar) | instalar o Termux:API (passo 1) e `pkg install termux-api`; abrir o app uma vez |
| `INTERNET_NOT_RUNNING` | a economia de bateria fechou o Termux | o passo 3, e rodar `shutup-and-run internet on` de novo |

Uma conexão que já começou nunca é cortada por essas esperas.

## 5. Pausar e desligar

```sh
shutup-and-run internet pause --for 2h   # novas conexões esperam por 2 horas
shutup-and-run internet resume           # volta antes
shutup-and-run internet off              # desliga; não volta sozinho após reiniciar
```

`internet off --forget` também apaga o login guardado no celular. Desinstalar o Termux apaga tudo.

## Bom saber

- **Deixe no carregador**, num lugar ventilado. O programa mantém o celular acordado.
- **Dados móveis custam dinheiro:** cada página que o operador abre passa duas vezes pelo celular, então o plano gasta cerca do dobro. Em roaming, mais ainda. Use `SHUTUP_AND_RUN_MAX_DAY` se liberar os dados móveis.
- **Quem desbloquear o celular** consegue ver a senha do proxy no Termux.
- Depois de reiniciar, o programa liga sozinho quando o celular é desbloqueado pela primeira vez, em até um minuto.
- Ao relatar um problema ao operador, diga a marca, o modelo e a versão do Android, e cole a saída de `shutup-and-run internet status`. Nunca mande senha nem IP.
