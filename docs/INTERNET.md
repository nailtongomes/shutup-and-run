# Internet sharing: lend only the PC's connection

A PC that cannot run containers (no WSL, too little memory, a weak machine) can still lend its **internet connection**. The operator's containers run in a datacenter; their requests and their Chrome leave through the client's home connection, so a court's or a site's system sees the client's residential IP, not the datacenter's.

```
container (datacenter) --HTTP proxy--> relay (operator's server) <--TCP, opened by the PC-- PC (client's home) --> site
```

The PC dials out to a relay the operator runs and keeps 16 idle connections open to it (Chrome opens 10 to 30 for one page). Nothing is opened on the PC or its router, so it works behind NAT and CGNAT. When a container asks the relay's proxy for `host:port`, the relay sends that line down one idle connection; the PC resolves the name with its own DNS, dials the site and copies the bytes both ways. No Docker and no WSL: the `.exe` alone does it, and the same binary does it on Linux.

Alpha. Read [Risks](#risks-and-what-is-left-out) before turning it on for a client.

## The choice

| Option | Why not, or why |
| --- | --- |
| Windows' own OpenSSH, `ssh -R 1080` (a SOCKS proxy on the server) | No code, encrypted. But `ssh.exe` is an optional feature that may be missing, needs a key per PC and an `sshd` with `GatewayPorts` on the server, gives a SOCKS proxy Chrome cannot log in to, and counts no bytes. Supervising and restarting it after sleep is code anyway. |
| chisel, frp, gost, rathole | Ready and fast, but a second program on the client's PC, its own update path, and Defender flags several of them as HackTool. A client finding a "hack tool" alert is worse than any saving. |
| WebSocket or TCP with multiplexing (yamux, HTTP/2) | Fewer connections, but a framing layer, flow control and a new crate on both sides. |
| **A reverse connection pool in the binary (chosen)** | One TCP connection per stream, no multiplexing: an idle connection waits for one `C host:port` line, then becomes the stream, and `tokio::io::copy_bidirectional` does the rest. About 1,600 lines of Rust on both sides (the CLI and the page included, the tests apart), no new crate, nothing more to install, bytes counted where they cross, and reconnection is the same loop as the first connection. The relay is the same binary: `shutup-and-run relay`. |

The relay speaks plain HTTP proxy (`CONNECT` for HTTPS, `http://` URLs for plain HTTP), which `requests`, `curl` and Chrome all use. The PC is chosen by the proxy login: user = the PC's id, password = its secret. Chrome takes no proxy login on its command line, so the relay can also open a port with no login that always goes out through one PC (`--open PORT=ID:SECRET`).

## The operator: the relay

A step-by-step guide in Portuguese, with the topology, the server's size and a two-minute test: [SERVIDOR.pt-BR.md](SERVIDOR.pt-BR.md).

On an Ubuntu server the PCs can reach on one TCP port, in one line, no Docker needed:

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 bash
```

It makes the token (`/etc/shutup-and-run/relay_token`), the systemd unit `shutup-and-run-relay` with PCs on `0.0.0.0:7000` and the proxy on Docker's bridge address (`172.17.0.1:8080`, loopback until Docker is there; `SHUTUP_AND_RUN_RELAY_PROXY` for a VPN address), and prints what to give the clients. Install the worker first if its jobs run on the same server. The plain install line restarts the relay on the new binary; `--uninstall` removes it.

Or with Docker, the release's image (about 20 MB, `ghcr.io/nailtongomes/shutup-and-run-relay`, pushed as each version and `latest`):

```bash
docker run -d --name relay --restart unless-stopped -p 7000:7000 -p 172.17.0.1:8080:8080 \
  -e SHUTUP_AND_RUN_RELAY_TOKEN=TOKEN ghcr.io/nailtongomes/shutup-and-run-relay
```

The token comes from `SHUTUP_AND_RUN_RELAY_TOKEN` (or `--token-file`); without one the relay accepts any PC. To add it to a compose file you already run, paste the `relay` service of `examples/relay/compose.yml` and put `RELAY_TOKEN` in its `.env`; on its own:

```bash
cd examples/relay
echo "RELAY_TOKEN=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')" > .env   # what every PC must send
docker compose up -d
docker compose logs -f relay        # "PC <id> connected from <ip>", a line per stream, a summary per minute
```

- The token keeps unknown PCs out: without it anyone who finds port 7000 can connect one. Give it to the client with the relay's address (`internet on --relay HOST:7000 --token TOKEN`), or let your poll URL hand both out (`internet on --discover`, [contract](INTEGRATION-CONTRACT.md#internet-discovery)).

- `7000` is published: the PCs dial in there, with the token. Open it in the server's firewall.
- `8080` is the proxy, published only on Docker's bridge address: services of the same compose file use `http://ID:SECRET@relay:8080`, and the jobs of a shutup-and-run worker on the same server (Docker's default network; a request cannot choose another) use `http://ID:SECRET@172.17.0.1:8080`. A container in another datacenter reaches it only over a VPN; never publish it on the internet.
- For Chrome with no login: uncomment the `command` line with `--open=8101=${PC_ID}:${PC_SECRET}` in `compose.yml`, put `PC_ID` and `PC_SECRET` in its `.env`, and add `- "172.17.0.1:8101:8101"` under `ports`; Chrome uses `--proxy-server=http://relay:8101` from the same compose file, `http://172.17.0.1:8101` from a worker's job. With `docker run`: `... ghcr.io/nailtongomes/shutup-and-run-relay --agents=0.0.0.0:7000 --proxy=0.0.0.0:8080 --open=8101=ID:SECRET` and `-p 172.17.0.1:8101:8101`.

The image is the release's own musl binary on `scratch` (`examples/relay/Dockerfile`), so what the relay speaks is what that release's PCs speak. A 0.2.27 relay refuses 0.2.26 PCs (`update it`): update the PCs with the relay. To run a build of your own, copy a musl binary (AGENTS.md) beside the Dockerfile as `shutup-and-run` and `docker build`.

The relay keeps nothing on disk. A restart drops every stream; the PCs come back by themselves within seconds. Once a minute the log has a line per PC whose numbers changed: `relay: PC <id>: 32 idle, 2 open`, and `, 3 refused at 140 streams` once it refused any (0.2.32) ("How many at once").

**Which PCs are there.** `GET /pcs` on the proxy port, with the relay's token as a bearer, lists every PC the relay knows, connected first: `id`, `connected`, `idle`, `open` (streams now), `streams` (0.2.32: places taken of the 140, streams still being set up included), `busy` (streams refused for the 140, since the relay started), `busy_secs_ago` (0.2.33: how long ago the last of those was, `null` for none), `seen_secs_ago`, and `sent_bytes` and `received_bytes` (the containers', since the relay started). Never a password or an address. A relay without a token answers `404`.

```bash
curl -s -H "Authorization: Bearer $TOKEN" http://172.17.0.1:8080/pcs
# {"pcs":[{"busy":0,"busy_secs_ago":null,"connected":true,"id":"pc-cliente-01","idle":32,"open":3,"received_bytes":5120334,"seen_secs_ago":2,"sent_bytes":80211,"streams":3}]}
```

By hand: `shutup-and-run relay --agents 0.0.0.0:7000 --proxy 172.17.0.1:8080 --token-file FILE` (the proxy on a private address).

## The client's PC

The operator tells the client, or the client's AI agent, one line. Windows (PowerShell, as the user, no administrator, no WSL needed), with the `.exe` downloaded from the [latest release](https://github.com/nailtongomes/shutup-and-run/releases/latest):

```powershell
$env:SHUTUP_AND_RUN_RELAY = "relay.example.com:7000"
$env:SHUTUP_AND_RUN_RELAY_TOKEN = "TOKEN"     # only when the relay asks for one
$env:SHUTUP_AND_RUN_INTERNET_LOGIN = "ID:PASSWORD"   # optional: the login the operator chose for this PC
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

The install line downloads the `.exe`, checks it against `SHA256SUMS`, puts it on the PATH and runs `shutup-and-run internet on --relay ... --token ... --login ... --wait 60`: it returns once the PC is connected, or says why not (and keeps trying). With `$env:SHUTUP_AND_RUN_POLL_URL` too, it also installs the worker, where `shutup-and-run fit` says this PC can run containers (WSL2 already there, 8 GiB and more than 2 processors); elsewhere it lends the internet now and prints what the worker needs. Run again, it is the update, and it starts internet sharing again on the new `.exe`. `internet on` itself copies the `.exe` to `C:\ProgramData\shutup-and-run\`, registers the logon task `shutup-and-run-internet` (under `conhost --headless`, no window) and starts it. On a PC that already runs the worker, the same `.exe` does it; the worker and this mode are independent.

A phone (Android, in Termux, no root): [Android](#android-a-phone-in-termux).

Linux (root): `shutup-and-run internet on --relay relay.example.com:7000` writes and starts the systemd unit `shutup-and-run-internet`. Without systemd, keep `shutup-and-run internet serve` running. `install.sh` takes the same `SHUTUP_AND_RUN_RELAY`, `SHUTUP_AND_RUN_RELAY_TOKEN` and `SHUTUP_AND_RUN_INTERNET_LOGIN`, beside the worker it installs, or alone with `SHUTUP_AND_RUN_INTERNET_ONLY=1` (no Docker).

**The login from the operator's backend.** `--login ID:PASSWORD` (the password 16 to 128 letters, digits, `-`, `_` or `.`) sets the id and the password the operator chose, so nothing has to come back from the PC: the task is created with `HTTPS_PROXY=http://ID:PASSWORD@relay:8080`, and the backend asks the relay's `GET /status` with the same login whether that PC is connected. Without it, the PC makes its own and `internet credentials` shows them. An agent sets it up in one command: `internet on --relay HOST:7000 --token TOKEN --login ID:PASSWORD --wait 60 --json`.

| Command | What it does |
| --- | --- |
| `fit [--json]` | before anything: `both` (containers and the internet) or `internet`, and why |
| `internet on (--relay HOST:PORT \| --discover URL) [--login ID:PASSWORD \| --id NAME] [--token TOKEN] [--hours "mon-fri 19-07"] [--max-day 5g] [--metered] [--wait SECONDS]` | turn on, or change; keeps the id, the password and the token of a previous `on`. `--discover` asks your URL (the poll URL serves) for the relay, the token and the login, once ([contract](INTEGRATION-CONTRACT.md#internet-discovery)). `--wait` returns once connected, or exits 1 with the code and the hint |
| `internet pause [--for 1h]`, `internet resume` | new streams wait (30m, 2h, 1d, `day`), or stop waiting; open ones go on |
| `internet status [--json]` | `ok`, or why not, with a `hint`; bytes carried, and today's against `--max-day`. Exit 1 when on and not working |
| `internet credentials [--json \| --env]` | the proxy address with the password, for the operator; `--env` as `HTTPS_PROXY=` and `HTTP_PROXY=` lines, for a task's env |
| `internet off [--forget]` | stop and remove the task or unit; `--forget` also deletes the id and password. `wsl uninstall` does both |
| `http://127.0.0.1:8788/` | the same, on a page, refreshed every 10 seconds, with two buttons for the person at the PC: pause 1 hour (or resume) and stop sharing |

`doctor` and `status` show the same `internet` line on a machine where it was turned on; `status --json` has it as `internet`, and on a PC with no worker says `"worker": "not installed"`. Codes: `INTERNET_NOT_RUNNING`, `INTERNET_RELAY_UNREACHABLE`, `INTERNET_REFUSED`, `INTERNET_WAITING` (hints in [FOR-AGENTS.md](FOR-AGENTS.md#codes-and-hints)).

The files are in `C:\ProgramData\shutup-and-run\` on Windows and `/var/lib/shutup-and-run/` on Linux: `internet.json` (the relay, the id, the password, the hours; mode 600 on Linux), `internet-status.json` (written every 5 to 30 seconds), `internet.log` (rotated at 10 MB, two files). `internet off --forget` deletes them.

## Android: a phone in Termux

A phone can lend its connection too (0.2.30): the home's Wi-Fi, or its mobile data when the person allows it. It is the same program, built for Android (`shutup-and-run-aarch64-linux-android` in each release), run in [Termux](https://termux.dev), a terminal app. No root, no app of ours, no store. Only internet sharing runs there: the worker needs Docker, and the commands that need it, WSL or systemd answer `ANDROID_NO_CONTAINERS` and exit 1. The choice and what an app of our own would take: [DECISIONS.md](DECISIONS.md#a-phone-shares-its-connection-from-termux-0230).

What is different from a PC:

- **Wi-Fi only, unless the person allows mobile data.** Every 5 seconds the phone reads which network its traffic leaves through: the address the kernel picks for the default route, and the interface that has it. `wlan0` is Wi-Fi; `rmnet_data0`, `ccmni0`, `seth_lte0` and the like are mobile data. On mobile data new connections wait (`INTERNET_NOT_ON_WIFI`) unless `--mobile-data`. A network it cannot name (a VPN's `tun0`, a maker's own name) counts as mobile data: the cost would fall on the person. `internet status` and the page show it: `network: Wi-Fi (wlan0); shares only on Wi-Fi`.
- **A new network dials the relay at once.** When the interface or the address changes (Wi-Fi to mobile data, another Wi-Fi, the carrier gives a new IP, the signal comes back), the idle connections are closed and dialed again at once, and those waiting to retry stop waiting. Android itself closes the connections of a network that goes away. A stream open at that moment ends with its network, and the consumer's request fails; there is no failover.
- **The battery, with Termux:API (0.2.32).** With the **Termux:API** app and Termux's `termux-api` package (`pkg install termux-api`), the phone reads its battery once a minute (`termux-battery-status`, a Java process each time, so not every 5 seconds): off its charger and under 60%, new connections wait (`BATTERY_LOW`), as on a Windows laptop. `internet status` and the page show it: `battery: 72%, on a charger`. Without Termux:API there is no reading, and no reading never holds: keep it on its charger. Since 0.2.33 `internet status` and the page say so (`TERMUX_API_MISSING`: no `termux-battery-status`; `TERMUX_API_NO_ANSWER`: it gave no reading in 15 seconds, so the app is missing or stopped), as something to know, not a failure. A reading older than 3 minutes counts as none.
- **What it does not read.** Presenting and Windows' metered flag come from Windows. `--hours`, `--max-day`, `pause` and the page's buttons work as on a PC.
- **Files.** `~/.shutup-and-run/` in Termux (`internet.json` mode 600, the status, the log). Uninstalling Termux deletes them.

The binary is for 64-bit ARM, which nearly every phone sold since 2017 is (`uname -m` says `aarch64`). It links only Android's own C library, never a static one: a static binary resolves names from `/etc/resolv.conf`, which Android does not have.

### Install, once

1. Install **Termux** from [F-Droid](https://f-droid.org/packages/com.termux/) or its [GitHub releases](https://github.com/termux/termux-app/releases), not from Google Play: the Play build is another one, and these steps were written for the F-Droid and GitHub builds. Install **Termux:Boot** from the same place, and open it once, so Android lets it start at boot. For the battery, **Termux:API** from the same place too, and `pkg install termux-api` in Termux. In Portuguese, step by step for the person with the phone: [ANDROID.pt-BR.md](ANDROID.pt-BR.md).
2. In Termux, one line installs it and turns it on (0.2.31):

   ```sh
   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install-termux.sh \
     | SHUTUP_AND_RUN_RELAY=relay.example.com:7000 SHUTUP_AND_RUN_RELAY_TOKEN=TOKEN bash
   ```

   It downloads the Android binary and `SHA256SUMS` of the latest release, refuses a binary whose sum does not match, installs it as `$PREFIX/bin/shutup-and-run` and runs `internet on ... --wait 60`, which returns once the phone is connected or says why not. `SHUTUP_AND_RUN_INTERNET_LOGIN=ID:PASSWORD` passes the operator's login, `SHUTUP_AND_RUN_MOBILE_DATA=1` allows mobile data and `SHUTUP_AND_RUN_MAX_DAY=2g` caps a day. Without `SHUTUP_AND_RUN_RELAY` it only installs. The same line is the update: `internet on` stops the process of the old version and starts the new one.

   By hand, the same steps: `curl -fLO` the asset `shutup-and-run-aarch64-linux-android` and `SHA256SUMS` from the [latest release](https://github.com/nailtongomes/shutup-and-run/releases/latest), `grep ' shutup-and-run-aarch64-linux-android$' SHA256SUMS | sha256sum -c -` (it must say `OK`), `install -m 755 shutup-and-run-aarch64-linux-android "$PREFIX/bin/shutup-and-run"`.

### Turn it on, look, turn it off

```sh
shutup-and-run internet on --relay relay.example.com:7000 --token TOKEN --wait 60
```

with `--login ID:PASSWORD` when the operator chose the login, and `--mobile-data` (best with `--max-day 2g`) only when the person agrees to spend mobile data. It writes `~/.shutup-and-run/internet.json`, starts `internet serve` in the background, takes Termux's wake lock (`termux-wake-lock`: Termux's notification says so, and Android may ask to let Termux ignore battery optimization; allow it) and writes `~/.termux/boot/shutup-and-run-internet`, which Termux:Boot runs at every boot. Run again, it changes the settings and keeps the id, the password and the token.

| On the phone | |
| --- | --- |
| `shutup-and-run internet status` | connected or why not, the network, the bytes |
| `shutup-and-run internet credentials` | the proxy login, for the operator |
| `http://127.0.0.1:8788/` in the phone's browser | the same page as on a PC, with pause 1 hour and stop |
| `shutup-and-run internet pause --for 2h`, `internet resume` | new connections wait, or stop waiting |
| `shutup-and-run internet off` | stops within 5 seconds, removes the boot script, releases the wake lock; `--forget` also deletes the id and the password |

### With the screen off, after a reboot, and the battery savers

Termux keeps running with the screen off while it holds the wake lock (a CPU lock and a Wi-Fi lock) and its notification is there. What stops it is the phone's own battery saving, which differs by maker ([dontkillmyapp.com](https://dontkillmyapp.com) has every model):

- **Every phone:** Settings, Apps, Termux, Battery: **Unrestricted** (or "Don't optimize"). The same for Termux:Boot. Do not swipe Termux away, and do not tap "Exit" on its notification.
- **Samsung:** Settings, Battery, Background usage limits, **Never sleeping apps**: add Termux and Termux:Boot.
- **Xiaomi, Redmi, POCO:** the app's settings: **Autostart** on, Battery saver **No restrictions**; in recent apps, hold Termux and lock it.
- **Huawei, Honor:** Settings, Battery, App launch, Termux: manage manually, with the three switches on.
- **OnePlus, OPPO, realme:** the app's Battery: **Allow background activity**, and **Auto launch** on.
- **Android 12 and later** may kill programs started from Termux ("phantom processes"). On Android 14 and later: Developer options, **Disable child process restrictions**. On 12L and 13, once, from a computer with adb: `adb shell settings put global settings_enable_monitor_phantom_procs false`. On 12: `adb shell device_config set_sync_disabled_for_tests persistent` and `adb shell device_config put activity_manager max_phantom_processes 2147483647`.

After a reboot, Termux:Boot starts it once the phone is unlocked for the first time; `internet status` says `connected` within a minute. A process that ends with an error, or is killed alone, starts again a minute later: the boot script runs it in a loop until it ends well (`off`). If a battery saver killed Termux itself, `internet status` says `INTERNET_NOT_RUNNING`, and `internet on` with the same relay starts it again.

### Risks on a phone

The [risks](#risks-and-what-is-left-out) of a PC, and:

- **The carrier's IP changes and is shared.** On mobile data the address is usually the carrier's (CGNAT): it changes when the phone reconnects, and many of the carrier's customers share it, so a site may already limit or block it, and a session tied to one IP may end when it changes.
- **Mobile data costs money.** Every byte a consumer loads crosses the phone twice: about twice the traffic on the plan. Roaming costs more. `--mobile-data` is the person's choice, at the phone; `--max-day` caps it.
- **Battery and heat.** The wake lock keeps the phone awake. On its charger, in a ventilated place.
- **Anyone with the phone unlocked** can read the password in Termux, as on a PC.
- **The network is guessed from the interface's name.** A Wi-Fi that is itself another phone's hotspot is still Wi-Fi to it, and a maker's unusual name is `unknown` until `--mobile-data`.

### Testing it on a phone

`tests/internet/termux.sh` runs in CI: the Android build in termux/termux-docker (Android's C library and Termux's files, on x86_64), with `fit`, the worker's commands refused, `on --wait`, `status` with the network, curl over HTTP and HTTPS through the relay, the bytes, the container's network removed (offline) and back (dialed again, curl works), its interface renamed `rmnet0` (mobile data: new connections wait, then `--mobile-data` shares), without Termux:API (`TERMUX_API_MISSING` in `status`), a `termux-battery-status` that says 45% off the charger (`BATTERY_LOW`, then on a charger it shares again), a new version, `kill -9` and `off`. It cannot test a phone's radio, battery savers or IP. On a phone, with a relay on the server (the `docker run` above, `--open=8101=ID:PASSWORD` and `-p 172.17.0.1:8101:8101` for Chrome):

1. Phone on Wi-Fi. In Termux: `curl https://api.ipify.org` prints the home's IP, `IP_WIFI`. `internet on --relay SERVER:7000 --token TOKEN --wait 60` says connected; `internet status` says `network: Wi-Fi (wlan0)`.
2. Server: `curl https://api.ipify.org` prints the datacenter's IP. Through the phone, `docker run --rm curlimages/curl -s -x http://ID:PASSWORD@172.17.0.1:8080 https://api.ipify.org` prints `IP_WIFI`.
3. Selenium through the phone: `docker run -d --name chrome -p 127.0.0.1:4444:4444 --shm-size 2g selenium/standalone-chromium`, then

   ```python
   from selenium import webdriver
   options = webdriver.ChromeOptions()
   options.add_argument("--proxy-server=http://172.17.0.1:8101")
   options.add_argument("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
   driver = webdriver.Remote("http://127.0.0.1:4444", options=options)
   driver.get("https://api.ipify.org")
   print(driver.find_element("tag name", "body").text)  # IP_WIFI
   driver.quit()
   ```

4. Wi-Fi off on the phone: within 10 seconds `internet status` says `INTERNET_NOT_ON_WIFI` and `network: mobile data (...)`; step 2 answers `503` after 10 seconds.
5. `internet on --relay SERVER:7000 --mobile-data --max-day 1g --wait 60`: connected. Termux's `curl https://api.ipify.org` prints the carrier's IP, `IP_MOBILE`; steps 2 and 3 print `IP_MOBILE`.
6. On the server, `while sleep 5; do curl -s -m 20 -x http://ID:PASSWORD@172.17.0.1:8080 https://api.ipify.org; echo; done`, and turn Wi-Fi on: `IP_MOBILE`, a few failures for at most about 20 seconds, then `IP_WIFI`. `~/.shutup-and-run/internet.log` has `network mobile data (...) -> Wi-Fi (wlan0)`. Airplane mode for a minute and off again: `offline`, then back the same way.
7. Screen off for 30 minutes, on the charger and then on the battery: the loop goes on. If it stops, the battery section above. With Termux:API, `internet status` says `battery: ..%, on its battery`; under 60% off the charger, step 2 answers `503`, and on the charger it works again within about a minute.
8. Reboot the phone and unlock it without opening Termux: within a minute the loop prints the IP again.
9. `internet status` counts the bytes; the operator's `GET /pcs` too. `internet off`: Termux's wake lock notification goes, and `/pcs` says `"connected":false`.

Write down the phone's maker, model and Android version with the result; never the IPs.

## What it respects

| Control | In this mode |
| --- | --- |
| Owner's consent | Turning it on at the PC is the consent; `consent_at` in `internet.json` records when. Nothing remote turns it on. |
| `pause` of the worker | Yes: while the worker is paused (`status.json` says `draining`), new streams wait. |
| Its own pause | `internet pause --for 1h`, or the page's button: new streams wait until then (`INTERNET_PAUSED`). `internet resume`, or `on` again, ends it. |
| Hours | Yes, its own: `--hours`, in the syntax of `lend_hours`, on the Windows clock. |
| Battery under 60% | Yes (Windows). |
| Presenting or full screen | Yes (Windows). |
| Metered network | Yes (Windows): it waits unless `--metered`. |
| Wi-Fi only | Yes (Android): a phone waits on mobile data unless `--mobile-data`. |
| Data a day | Yes, its own: `--max-day 5g`. Past it, new streams wait until midnight on the PC's clock (`INTERNET_DAILY_LIMIT`). Off unless set. |
| Memory, disk, CPU limits, `safety` | No: this mode takes a few MB of memory and no disk, and is meant for the PC that cannot lend those. |
| `min_idle_secs` | No. |

A wait closes the idle connections, so the relay answers `503` (or `502`, for a request already on its way) until it ends. **A stream already open is never cut** by any of them, as a running job is not.

The PC dials a site's IPv4 addresses first, 3 seconds each, and its IPv6 ones only when no IPv4 answers: a home with broken IPv6 costs no delay, and the address a site sees is the one `api.ipify.org` shows (`api64.ipify.org` shows the IPv6 one).

Every stream to the PC's own network is refused: loopback, `10/8`, `172.16/12`, `192.168/16`, link-local, the carrier's `100.64/10`, IPv6 ULA and link-local. The PC checks the address it dials, not the name, so a name that resolves to the router cannot get through. The operator never reaches the client's router, printer, NAS or the worker's own page.

## Bytes

The PC counts what crosses the site's side of each stream: `bytes_received` (from the sites) and `bytes_sent` (to the sites), kept across restarts since `counting_since`, in `internet status --json`, the page and `internet-status.json`. The PC's network card sees about twice that: each byte comes in from one side and goes out on the other. The relay logs the same per stream.

## Reconnection and what keeps it working

- The relay closes or restarts: each idle connection sees the end at once and dials again after 1 s, doubling up to 30 s, with up to 1 s at random so they do not knock together.
- A phone moves to another network or gets a new address: the idle connections are dialed again at once ([Android](#android-a-phone-in-termux)).
- The PC sleeps, its Wi-Fi changes, the internet drops: an idle connection pings every 10 s and the relay echoes; 30 s without a word and the PC drops it and dials again (the relay drops its end after 35 s).
- In between, the relay may still hold connections that are dead. It tries first the one heard from in the last 15 s, and a claimed connection must answer `A` within 3 s; one that does not is skipped for the next, up to 3, so a request right after a wake costs a few seconds, not a `502`.
- A stream with no byte either way for 10 minutes is closed on both sides: after a sleep its other end is gone, and it would hold one of the PC's 512 places for good.
- An id keeps its password on the relay for 10 minutes after its last connection is gone: a PC that slept gets its id back, not whoever connected first. A relay restart forgets it; the token is what keeps others out then.
- The process itself: the logon task (Windows), which also starts it again a minute after it ends with an error, up to 999 times; `Restart=on-failure` (systemd). The process exits cleanly only when it is turned off, so the page's stop is not undone.

`cargo test relay::` runs these against a real relay in the process: one that restarts on the same port, one that greets and then goes silent, a dead connection skipped for a live one, an id kept while its PC is away, and a wait that closes and reopens.

## How many at once

A PC keeps 32 idle connections to the relay and carries up to 512 streams at once. The relay lets one PC carry **140 streams at once** (0.2.31): 7 consumers at the same time (containers, Selenium or Playwright browsers), at about 20 streams each, which is what one headless Chromium held at most while loading pages. One more stream waits up to 10 seconds for one to end, then gets `503` (`PC <id> carries 140 streams at once`). A client's home or phone upload is shared by all of them, and one consumer out of control must not take it all. A browser opens 10 to 30 streams for one page and keeps some open a while, so the limit counts streams, not consumers: `curl` and `requests` take one per request.

Measured with `SHUTUP_AND_RUN_LOAD=1 tests/internet/local.sh` (relay and PC on one Linux server in a datacenter, 10 headless Chromium at once, each opening five sites three times):

| PC | runs | pages | failed | median page | 95% under | most streams open at once |
| --- | --- | --- | --- | --- | --- | --- |
| 32 idle, 512 streams (0.2.29) | 2 | 150 each | 0 | 1.5 and 1.7 s | 5.2 and 5.9 s | 192 and 191 |
| 16 idle, 256 streams (up to 0.2.28) | 1 | 150 | 0 | 1.8 s | 5.9 s | 192 |

With the relay's limit (0.2.31), same server:

| Relay limit | browsers | pages | failed | median page | 95% under | most streams open at once |
| --- | --- | --- | --- | --- | --- | --- |
| none | 1 | 15 | 0 | 0.3 s | 1.0 s | 20 |
| none | 3 | 45 | 0 | 0.5 s | 1.6 s | 58 |
| 7 streams | 1 | 15 | 13 | | | 7 |
| 140 streams | 7 | 105 | 0 | 1.1 s | 4.0 s | 135 |
| 140 streams | 10 | 150 | 1 (`503`) | 1.3 s | 8.6 s | 140 |

Seven streams were not enough for one browser: Chrome keeps several tunnels open between pages, and the next ones waited and failed. 140 carried seven browsers with no failure; ten went past it and one page of 150 got the `503`. Whether 140 is enough for real tasks shows on the relay: `streams` and `busy` in `GET /pcs`, and the log line's `refused at 140 streams`. A `busy` that grows means more consumers than the PC carries; send them to another PC. `busy_secs_ago` says whether that is now or was days ago.

Before the limit, both carried 10 browsers with no failure and in about the same time: more idle connections did not make pages faster here. What changed is the margin: 10 browsers held up to 192 streams at once, three quarters of the old 256, past which a PC starts no new idle connection; 512 leaves room for heavier pages or a few more consumers. On a client's PC the limit that counts is the home connection's upload: every page a consumer loads leaves the relay as the PC's download and comes back as its upload. A 10 Mbit/s upload is about 1 MB/s for all of them together; ten browsers on heavy pages share that. The relay's own cost is small: an idle connection is a socket and a ping every 10 s.

## A task that goes out through a client's connection

The case this is for: an agent or a person makes a task where the client works, and the client's PC cannot run it. The task goes to a worker of the operator, anywhere (a server in Germany will do), and still reaches the court from the client's home address. A request has no proxy field, and needs none: the address goes in the task's `env`.

1. On the client's PC, once: `internet on` (or the install line above), then `shutup-and-run internet credentials --env` prints

   ```
   HTTPS_PROXY=http://ID:SECRET@relay.example.com:8080
   HTTP_PROXY=http://ID:SECRET@relay.example.com:8080
   ```

   with the relay's host as the PC knows it. Replace it with the address the worker's containers reach the proxy at (below).
2. The agent sends the task to the operator's worker with those in `env`, as any push or poll task:

   ```json
   {"image": "registry.example.com/court-robot:1",
    "env": {"HTTPS_PROXY": "http://ID:SECRET@10.8.0.1:8080",
            "HTTP_PROXY": "http://ID:SECRET@10.8.0.1:8080",
            "PROXY_STATUS_URL": "http://ID:SECRET@10.8.0.1:8080/status"},
    "timeout_secs": 1800}
   ```

   `requests`, `httpx`, `curl` and most HTTP libraries read `HTTPS_PROXY` by themselves. Chrome does not: the image starts it with `--proxy-server` (the relay's `--open` port for that PC), or logs in from Playwright, Puppeteer or SeleniumBase. [Examples](#examples-curl-requests-selenium-playwright-puppeteer-node-n8n) has each.
3. When the worker starts it, the task can first ask whether the PC is there: `curl -s $PROXY_STATUS_URL` answers `{"id":"ID","connected":true,"idle":16,"open":0,"seen_secs_ago":4}`, or `401` for a wrong login. Not connected: the proxy answers `503` for 10 seconds per request; the task waits and tries again, or ends and is sent again later. The worker does not retry a job.

The worker's containers must reach the proxy port. On the relay's own host, the relay's install line and the compose file put it on Docker's bridge, and the worker's containers there use `172.17.0.1:8080`. On another server, put the relay and the worker on a VPN (WireGuard, Tailscale) and bind the proxy on the VPN address. Published on the internet, the password is all that guards the client's connection, and it crosses in the clear.

The proxy password then sits in the job's env on the worker. A Linux worker is the operator's and trusted; never send it to a task on another client's PC.

## Examples: curl, requests, Selenium, Playwright, Puppeteer, Node, n8n

The values are made up. `ID:PASSWORD` is the PC's login (`internet credentials`, or the `--login` the operator chose). `PROXY_HOST:8080` is where your containers reach the relay's proxy: `relay:8080` on the compose network, `172.17.0.1:8080` on the relay's host, a VPN address elsewhere. Each example was run against a relay and a PC from this repository (0.2.27, Python 3.13, Node 22, Chrome 153), except the ones marked *not tested here*.

**Is the PC there?** Before a long task: `curl -s http://ID:PASSWORD@PROXY_HOST:8080/status` answers `{"connected":true,...}`. While the PC is away (asleep, paused, outside its hours), the proxy answers `503` after 10 seconds: wait and try again, as for any flaky site.

### Without code: the task's env

```json
"env": {"HTTPS_PROXY": "http://ID:PASSWORD@PROXY_HOST:8080",
        "HTTP_PROXY": "http://ID:PASSWORD@PROXY_HOST:8080"}
```

`curl`, `wget`, Python's `requests` and `httpx`, and Go's `net/http` read these by themselves. Node's built-in `fetch` reads them only with `NODE_USE_ENV_PROXY=1` in the env too (it worked on Node 22.23, with a warning that it is experimental). Chrome, Selenium, Playwright and Puppeteer do not read them: below. If the image or the machine also sets the lowercase `https_proxy`, that one wins in `curl` and `requests`: set both, or neither.

### curl

```bash
curl -x http://ID:PASSWORD@PROXY_HOST:8080 https://api.ipify.org
```

### Python: requests and httpx

```python
import requests, httpx

proxy = "http://ID:PASSWORD@PROXY_HOST:8080"
print(requests.get("https://api.ipify.org", proxies={"https": proxy, "http": proxy}, timeout=60).text)

with httpx.Client(proxy=proxy, timeout=60) as client:
    print(client.get("https://api.ipify.org").text)
```

With `HTTPS_PROXY` in the env, plain `requests.get(url)` and `httpx.get(url)` go the same way.

### Selenium (Chrome)

Chrome's `--proxy-server` takes no login. Use the relay's port with no login for that PC (`relay --open 8101=ID:PASSWORD`, kept on a private network):

```python
from selenium import webdriver

options = webdriver.ChromeOptions()
options.add_argument("--headless=new")
options.add_argument("--proxy-server=http://PROXY_HOST:8101")
options.add_argument("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
driver = webdriver.Chrome(options=options)
driver.get("https://api.ipify.org")
print(driver.find_element("tag name", "body").text)  # the PC's IP
driver.quit()
```

Or SeleniumBase, which logs in by itself (*not tested here*):

```python
from seleniumbase import SB

with SB(proxy="ID:PASSWORD@PROXY_HOST:8080", headless2=True) as sb:
    sb.open("https://api.ipify.org")
    print(sb.get_text("body"))
```

Selenium's own BiDi login (`driver.network.add_auth_handler`) hung on the first page in our test; do not rely on it. For a new robot, Playwright or Puppeteer log in with no extra port.

### Playwright

Node:

```js
import { chromium } from "playwright";

const browser = await chromium.launch({
  proxy: { server: "http://PROXY_HOST:8080", username: "ID", password: "PASSWORD" },
  args: ["--force-webrtc-ip-handling-policy=disable_non_proxied_udp"],
});
const page = await browser.newPage();
await page.goto("https://api.ipify.org");
console.log(await page.textContent("body"));
await browser.close();
```

Python is the same call, `p.chromium.launch(proxy={"server": "http://PROXY_HOST:8080", "username": "ID", "password": "PASSWORD"})` (*not tested here*; the Node one was).

### Puppeteer

```js
import puppeteer from "puppeteer";

const browser = await puppeteer.launch({
  args: ["--proxy-server=http://PROXY_HOST:8080",
         "--force-webrtc-ip-handling-policy=disable_non_proxied_udp"],
});
const page = await browser.newPage();
await page.authenticate({ username: "ID", password: "PASSWORD" });
await page.goto("https://api.ipify.org");
console.log(await page.$eval("body", (b) => b.innerText));
await browser.close();
```

### Node: fetch, axios, node:https

```js
import { ProxyAgent, fetch } from "undici";          // npm i undici
import { HttpsProxyAgent } from "https-proxy-agent"; // npm i https-proxy-agent
import axios from "axios";

const proxy = "http://ID:PASSWORD@PROXY_HOST:8080";

const res = await fetch("https://api.ipify.org", { dispatcher: new ProxyAgent(proxy) });
console.log(await res.text());

const agent = new HttpsProxyAgent(proxy);            // also for https.get(url, { agent }, ...)
const { data } = await axios.get("https://api.ipify.org", { httpsAgent: agent, proxy: false });
console.log(data);
```

axios's own `proxy: {host, port, auth}` option also went through in our test; the agent is the form that works the same with every Node HTTP library.

### n8n (*not tested here*)

- **HTTP Request node**: Options, Add option, **Proxy**: `http://ID:PASSWORD@PROXY_HOST:8080`. Only that node goes out through the client's PC.
- `HTTPS_PROXY` and `HTTP_PROXY` on the n8n container send every request of every workflow through the PC, the calls to your own APIs and credentials included. Prefer the node option.
- An n8n that runs in a client's container (a task on a worker) is the same: the node option, with the address that container reaches.

### Browsers: what does not go through

UDP does not cross the relay. Chrome falls back from QUIC to TCP by itself; WebRTC would show the datacenter's IP unless `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`, as in the examples above. Name resolution happens on the PC, so a site's DNS answer is the one the client's home gets.

## A test from another network

The proof that matters: from a container in the datacenter, the site sees the PC's IP.

1. Server: the relay (the `docker run` above, or `docker compose up -d` in `examples/relay`), with `7000` open.
2. PC: `internet on --relay SERVER:7000 --token TOKEN` (the relay's token), then `internet status` says `ok ... connected`. `internet credentials` gives `ID` and `SECRET`.
3. PC: `curl https://api.ipify.org` (Windows: `curl.exe`) prints the PC's public IP, say `177.x.y.z`.
4. Server, the datacenter's IP for comparison: `curl https://api.ipify.org`.
5. Server, through the PC:

   ```bash
   docker run --rm curlimages/curl -s \
     -x http://ID:SECRET@172.17.0.1:8080 https://api.ipify.org
   ```

   prints `177.x.y.z`, the PC's IP, not step 4's.
6. Chrome headless, through the port with no login (`--open=8101=ID:SECRET` and `-p 172.17.0.1:8101:8101`, above):

   ```bash
   docker run --rm zenika/alpine-chrome \
     --no-sandbox --proxy-server=http://172.17.0.1:8101 \
     --force-webrtc-ip-handling-policy=disable_non_proxied_udp \
     --dump-dom https://api.ipify.org
   ```

   prints the same IP inside `<body>`. With Selenium:

   ```python
   from selenium import webdriver
   options = webdriver.ChromeOptions()
   options.add_argument("--headless=new")
   options.add_argument("--proxy-server=http://172.17.0.1:8101")
   options.add_argument("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
   driver = webdriver.Chrome(options=options)
   driver.get("https://api.ipify.org")
   print(driver.find_element("tag name", "body").text)  # the PC's IP
   ```

   And with `requests`: `requests.get(url, proxies={"https": "http://ID:SECRET@relay:8080", "http": "http://ID:SECRET@relay:8080"})`.
7. Back on the PC, `internet status` counts the bytes, and the relay's log has a line per stream.
8. Reconnection: restart the relay (`docker compose restart relay`), or put the PC to sleep for two minutes and wake it. Within a minute `internet status` is `ok` again and step 5 works, with nothing done on the PC.

On one Linux machine, `sudo bash tests/internet/local.sh target/release/shutup-and-run` runs steps 2, 5 (curl) and the reconnection against a local relay. It proves the path, not the IP: both ends have the same one.

## Risks and what is left out

The client accepted these in the contract; this is the list to point at.

- **The client's IP is on every request.** A site that blocks or reports the traffic blocks or reports the client's address. The client's ISP may forbid sharing the connection in its terms. Nothing here limits which sites are reached (no allowlist of destinations).
- **The link from the PC to the relay is plain TCP.** HTTPS stays encrypted end to end, but the password, the names of the sites and any plain-`http://` page cross the internet readable. An observer between the PC and the relay could copy the password and use the client's connection through the relay.
- **Without `--token-file`, the relay accepts any PC.** Anyone who finds port 7000 can connect a PC under an id that is not in use. They cannot take an id that is connected with another password, or was in the last 10 minutes, and a container only reaches a PC with its exact password. After a relay restart, the first to connect sets an id's password. The token closes that; it crosses in the clear, like the password, until TLS.
- **The proxy port is the client's connection.** With the password, anyone who reaches `8080` uses it; an `--open` port needs no password at all. Keep both on a private network.
- **Chrome may leak the datacenter's IP through WebRTC** (UDP is not carried). `--force-webrtc-ip-handling-policy=disable_non_proxied_udp` stops that. UDP and QUIC do not go through the PC.
- **The password is readable on the PC** (`internet.json`, the page, `internet credentials`). Anyone at the PC can read it, as they can the poll token.
- **With `--login` the operator's backend holds every PC's password**, and the install line carries it in an environment variable of that PowerShell session. Keep it with the poll tokens, out of logs and tickets.

Left out on purpose: an allowlist of destinations, strong authentication, several operators or tenants, billing, and choosing the PC automatically.

## Next, for security

In this order, each a small patch:

1. TLS on the link, with the relay's certificate pinned in `internet.json` (`rustls` is already a dependency through `reqwest` and `axum-server`).
2. ~~A relay token~~: done, `relay --token-file` and `internet on --token`.
3. A password per container instead of per PC, issued by the relay, and the `--open` ports bound to one container's network.
4. ~~Per-PC streams at once on the relay~~: done, 140 (0.2.31). A list of destinations if the contract asks for one. (Bytes a day are limited on the PC: `--max-day`.)
