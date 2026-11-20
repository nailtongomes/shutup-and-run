# Internet sharing: lend only the PC's connection

A PC that cannot run containers (no WSL, too little memory, a weak machine) can still lend its **internet connection**. The operator's containers run in a datacenter; their requests and their Chrome leave through the client's home connection, so a court's or a site's system sees the client's residential IP, not the datacenter's.

```
container (datacenter) --HTTP proxy--> relay (operator's server) <--TCP, opened by the PC-- PC (client's home) --> site
```

The PC dials out to a relay the operator runs and keeps 4 idle connections open to it. Nothing is opened on the PC or its router, so it works behind NAT and CGNAT. When a container asks the relay's proxy for `host:port`, the relay sends that line down one idle connection; the PC resolves the name with its own DNS, dials the site and copies the bytes both ways. No Docker and no WSL: the `.exe` alone does it, and the same binary does it on Linux.

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

On a Linux server with Docker that the PCs can reach on one TCP port:

```bash
cd examples/relay
docker compose up -d --build        # the release's musl binary, checked against SHA256SUMS
docker compose logs -f relay        # "PC <id> connected from <ip>", one line per stream
```

- `7000` is published: the PCs dial in there. Open it in the server's firewall.
- `8080` is the proxy. It is **not** published: containers on the compose network `egress` use `http://ID:SECRET@relay:8080`. A container in another datacenter reaches it only over a private address or a VPN; never publish it on the internet.
- For Chrome, uncomment `--open=8101=${PC_ID}:${PC_SECRET}` and put `PC_ID` and `PC_SECRET` in a `.env` beside `compose.yml`; then Chrome uses `--proxy-server=http://relay:8101`.

The Dockerfile takes the latest release's binary, so it needs a release that has internet sharing (after 0.2.25). Before that, build the musl binary (AGENTS.md), copy it beside `compose.yml` and replace the Dockerfile's first stage with `COPY shutup-and-run /shutup-and-run`.

The relay keeps nothing on disk. A restart drops every stream; the PCs come back by themselves within seconds.

Without Docker: `shutup-and-run relay --agents 0.0.0.0:7000 --proxy 10.0.0.5:8080` (the proxy on a private address).

## The client's PC

The operator tells the client, or the client's AI agent, one line. Windows (PowerShell, as the user, no administrator, no WSL needed), with the `.exe` downloaded from the [latest release](https://github.com/nailtongomes/shutup-and-run/releases/latest):

```powershell
.\shutup-and-run-x86_64-pc-windows-msvc.exe internet on --relay relay.example.com:7000
```

It copies the `.exe` to `C:\ProgramData\shutup-and-run\`, registers the logon task `shutup-and-run-internet` (under `conhost --headless`, no window) and starts it. On a PC that already runs the worker, the same `.exe` does it; the worker and this mode are independent.

Linux (root): `shutup-and-run internet on --relay relay.example.com:7000` writes and starts the systemd unit `shutup-and-run-internet`. Without systemd, keep `shutup-and-run internet serve` running.

| Command | What it does |
| --- | --- |
| `internet on --relay HOST:PORT [--id NAME] [--hours "mon-fri 19-07"] [--metered]` | turn on, or change; keeps the id and password of a previous `on` |
| `internet status [--json]` | `ok`, or why not, with a `hint`; bytes carried. Exit 1 when on and not working |
| `internet credentials [--json]` | the proxy address with the password, for the operator |
| `internet off [--forget]` | stop and remove the task or unit; `--forget` also deletes the id and password |
| `http://127.0.0.1:8788/` | the same, on a page, refreshed every 10 seconds. Read-only |

`doctor` shows the same `internet` line on a machine where it was turned on. Codes: `INTERNET_NOT_RUNNING`, `INTERNET_RELAY_UNREACHABLE`, `INTERNET_REFUSED`, `INTERNET_WAITING` (hints in [FOR-AGENTS.md](FOR-AGENTS.md#codes-and-hints)).

The files are in `C:\ProgramData\shutup-and-run\` on Windows and `/var/lib/shutup-and-run/` on Linux: `internet.json` (the relay, the id, the password, the hours; mode 600 on Linux), `internet-status.json` (written every 5 to 30 seconds), `internet.log` (rotated at 10 MB, two files). `internet off --forget` deletes them.

## What it respects

| Control | In this mode |
| --- | --- |
| Owner's consent | Turning it on at the PC is the consent; `consent_at` in `internet.json` records when. Nothing remote turns it on. |
| `pause` of the worker | Yes: while the worker is paused (`status.json` says `draining`), new streams wait. |
| Hours | Yes, its own: `--hours`, in the syntax of `lend_hours`, on the Windows clock. |
| Battery under 60% | Yes (Windows). |
| Presenting or full screen | Yes (Windows). |
| Metered network | Yes (Windows): it waits unless `--metered`. |
| Memory, disk, CPU limits, `safety` | No: this mode takes a few MB of memory and no disk, and is meant for the PC that cannot lend those. |
| `min_idle_secs` | No. |

A wait closes the idle connections, so the relay answers `503` (or `502`, for a request already on its way) until it ends. **A stream already open is never cut** by any of them, as a running job is not.

Every stream to the PC's own network is refused: loopback, `10/8`, `172.16/12`, `192.168/16`, link-local, the carrier's `100.64/10`, IPv6 ULA and link-local. The PC checks the address it dials, not the name, so a name that resolves to the router cannot get through. The operator never reaches the client's router, printer, NAS or the worker's own page.

## Bytes

The PC counts what crosses the site's side of each stream: `bytes_received` (from the sites) and `bytes_sent` (to the sites), kept across restarts since `counting_since`, in `internet status --json`, the page and `internet-status.json`. The PC's network card sees about twice that: each byte comes in from one side and goes out on the other. The relay logs the same per stream.

## Reconnection

- The relay closes or restarts: each idle connection sees the end at once and dials again after 1 s, doubling up to 30 s, with up to 1 s at random so the four do not knock together.
- The PC sleeps, its Wi-Fi changes, the internet drops: an idle connection pings every 20 s and the relay echoes; 60 s without a word and it is dropped and dialed again. After a sleep the PC is back within about a minute; a stream that was open then is lost (the site's TCP connection went with it).
- The process itself: the logon task (Windows) or `Restart=always` (systemd).

`cargo test relay::` runs all three against a real relay in the process: one that restarts on the same port, one that greets and then goes silent, and a wait that closes and reopens.

## A test from another network

The proof that matters: from a container in the datacenter, the site sees the PC's IP.

1. Server: `docker compose up -d --build` in `examples/relay`, with `7000` open.
2. PC: `internet on --relay SERVER:7000`, then `internet status` says `ok ... connected`. `internet credentials` gives `ID` and `SECRET`.
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
- **The relay accepts any PC.** Anyone who finds port 7000 can connect a PC under an id that is not in use. They cannot take an id that is connected with another password, and a container only reaches a PC with its exact password. After a relay restart the first to connect sets an id's password.
- **The proxy port is the client's connection.** With the password, anyone who reaches `8080` uses it; an `--open` port needs no password at all. Keep both on a private network.
- **Chrome may leak the datacenter's IP through WebRTC** (UDP is not carried). `--force-webrtc-ip-handling-policy=disable_non_proxied_udp` stops that. UDP and QUIC do not go through the PC.
- **The password is readable on the PC** (`internet.json`, the page, `internet credentials`). Anyone at the PC can read it, as they can the poll token.

Left out on purpose: an allowlist of destinations, strong authentication, several operators or tenants, billing, and choosing the PC automatically.

## Next, for security

In this order, each a small patch:

1. TLS on the link, with the relay's certificate pinned in `internet.json` (`rustls` is already a dependency through `reqwest` and `axum-server`).
2. A relay token: a PC connects only with a token the operator gave, `internet on --relay ... --token ...`.
3. A password per container instead of per PC, issued by the relay, and the `--open` ports bound to one container's network.
4. Per-PC limits on the relay: streams at once, bytes per day; and a list of destinations if the contract asks for one.
