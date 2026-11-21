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

Or with Docker:

```bash
cd examples/relay
head -c 24 /dev/urandom | base64 | tr -d '/+=' > relay_token   # what every PC must send
docker compose up -d --build        # the release's musl binary, checked against SHA256SUMS
docker compose logs -f relay        # "PC <id> connected from <ip>", a line per stream, a summary per minute
```

- The token keeps unknown PCs out: without it anyone who finds port 7000 can connect one. Give it to the client with the relay's address (`internet on --relay HOST:7000 --token TOKEN`). To run without one, take `--token-file` and the `secrets` out of `compose.yml`.

- `7000` is published: the PCs dial in there, with the token. Open it in the server's firewall.
- `8080` is the proxy, published only on Docker's bridge address: containers on the compose network `egress` use `http://ID:SECRET@relay:8080`, and the jobs of a shutup-and-run worker on the same server (Docker's default network; a request cannot choose another) use `http://ID:SECRET@172.17.0.1:8080`. A container in another datacenter reaches it only over a VPN; never publish it on the internet.
- For Chrome, uncomment `--open=8101=${PC_ID}:${PC_SECRET}` and put `PC_ID` and `PC_SECRET` in a `.env` beside `compose.yml`; then Chrome uses `--proxy-server=http://relay:8101`. For a worker's jobs, also add `- "172.17.0.1:8101:8101"` under `ports` and use `http://172.17.0.1:8101`.

The Dockerfile takes the latest release's binary, which has the relay from 0.2.26 on. A 0.2.27 relay refuses 0.2.26 PCs (`update it`): update the PCs with the relay. To run a build of your own, build the musl binary (AGENTS.md), copy it beside `compose.yml` and replace the Dockerfile's first stage with `COPY shutup-and-run /shutup-and-run`.

The relay keeps nothing on disk. A restart drops every stream; the PCs come back by themselves within seconds. Once a minute the log has a line per PC whose numbers changed: `relay: PC <id>: 16 idle, 2 open`.

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

Linux (root): `shutup-and-run internet on --relay relay.example.com:7000` writes and starts the systemd unit `shutup-and-run-internet`. Without systemd, keep `shutup-and-run internet serve` running. `install.sh` takes the same `SHUTUP_AND_RUN_RELAY`, `SHUTUP_AND_RUN_RELAY_TOKEN` and `SHUTUP_AND_RUN_INTERNET_LOGIN`, beside the worker it installs, or alone with `SHUTUP_AND_RUN_INTERNET_ONLY=1` (no Docker).

**The login from the operator's backend.** `--login ID:PASSWORD` (the password 16 to 128 letters, digits, `-`, `_` or `.`) sets the id and the password the operator chose, so nothing has to come back from the PC: the task is created with `HTTPS_PROXY=http://ID:PASSWORD@relay:8080`, and the backend asks the relay's `GET /status` with the same login whether that PC is connected. Without it, the PC makes its own and `internet credentials` shows them. An agent sets it up in one command: `internet on --relay HOST:7000 --token TOKEN --login ID:PASSWORD --wait 60 --json`.

| Command | What it does |
| --- | --- |
| `fit [--json]` | before anything: `both` (containers and the internet) or `internet`, and why |
| `internet on --relay HOST:PORT [--login ID:PASSWORD \| --id NAME] [--token TOKEN] [--hours "mon-fri 19-07"] [--max-day 5g] [--metered] [--wait SECONDS]` | turn on, or change; keeps the id, the password and the token of a previous `on`. `--wait` returns once connected, or exits 1 with the code and the hint |
| `internet pause [--for 1h]`, `internet resume` | new streams wait (30m, 2h, 1d, `day`), or stop waiting; open ones go on |
| `internet status [--json]` | `ok`, or why not, with a `hint`; bytes carried, and today's against `--max-day`. Exit 1 when on and not working |
| `internet credentials [--json \| --env]` | the proxy address with the password, for the operator; `--env` as `HTTPS_PROXY=` and `HTTP_PROXY=` lines, for a task's env |
| `internet off [--forget]` | stop and remove the task or unit; `--forget` also deletes the id and password. `wsl uninstall` does both |
| `http://127.0.0.1:8788/` | the same, on a page, refreshed every 10 seconds, with two buttons for the person at the PC: pause 1 hour (or resume) and stop sharing |

`doctor` and `status` show the same `internet` line on a machine where it was turned on; `status --json` has it as `internet`, and on a PC with no worker says `"worker": "not installed"`. Codes: `INTERNET_NOT_RUNNING`, `INTERNET_RELAY_UNREACHABLE`, `INTERNET_REFUSED`, `INTERNET_WAITING` (hints in [FOR-AGENTS.md](FOR-AGENTS.md#codes-and-hints)).

The files are in `C:\ProgramData\shutup-and-run\` on Windows and `/var/lib/shutup-and-run/` on Linux: `internet.json` (the relay, the id, the password, the hours; mode 600 on Linux), `internet-status.json` (written every 5 to 30 seconds), `internet.log` (rotated at 10 MB, two files). `internet off --forget` deletes them.

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
- The PC sleeps, its Wi-Fi changes, the internet drops: an idle connection pings every 10 s and the relay echoes; 30 s without a word and the PC drops it and dials again (the relay drops its end after 35 s).
- In between, the relay may still hold connections that are dead. It tries first the one heard from in the last 15 s, and a claimed connection must answer `A` within 3 s; one that does not is skipped for the next, up to 3, so a request right after a wake costs a few seconds, not a `502`.
- A stream with no byte either way for 10 minutes is closed on both sides: after a sleep its other end is gone, and it would hold one of the PC's 256 places for good.
- An id keeps its password on the relay for 10 minutes after its last connection is gone: a PC that slept gets its id back, not whoever connected first. A relay restart forgets it; the token is what keeps others out then.
- The process itself: the logon task (Windows), which also starts it again a minute after it ends with an error, up to 999 times; `Restart=on-failure` (systemd). The process exits cleanly only when it is turned off, so the page's stop is not undone.

`cargo test relay::` runs these against a real relay in the process: one that restarts on the same port, one that greets and then goes silent, a dead connection skipped for a live one, an id kept while its PC is away, and a wait that closes and reopens.

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

1. Server: `docker compose up -d --build` in `examples/relay`, with `7000` open.
2. PC: `internet on --relay SERVER:7000 --token TOKEN` (the content of `relay_token`), then `internet status` says `ok ... connected`. `internet credentials` gives `ID` and `SECRET`.
3. PC: `curl https://api.ipify.org` (Windows: `curl.exe`) prints the PC's public IP, say `177.x.y.z`.
4. Server, the datacenter's IP for comparison: `curl https://api.ipify.org`.
5. Server, through the PC:

   ```bash
   docker run --rm --network relay_egress curlimages/curl -s \
     -x http://ID:SECRET@relay:8080 https://api.ipify.org
   ```

   prints `177.x.y.z`, the PC's IP, not step 4's.
6. Chrome headless, through the port with no login (`--open=8101=ID:SECRET`, then `docker compose up -d`):

   ```bash
   docker run --rm --network relay_egress zenika/alpine-chrome \
     --no-sandbox --proxy-server=http://relay:8101 \
     --force-webrtc-ip-handling-policy=disable_non_proxied_udp \
     --dump-dom https://api.ipify.org
   ```

   prints the same IP inside `<body>`. With Selenium:

   ```python
   from selenium import webdriver
   options = webdriver.ChromeOptions()
   options.add_argument("--headless=new")
   options.add_argument("--proxy-server=http://relay:8101")
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
4. Per-PC limits on the relay: streams at once; and a list of destinations if the contract asks for one. (Bytes a day are limited on the PC: `--max-day`.)
