# Recipes

Short answers for common cases. Each block is one command you can copy. `203.0.113.10` is the worker machine, `198.51.100.7` is your orchestrator, and `ghcr.io/you/worker` is an image you own.

## Only let the orchestrator in

The port answers anyone unless a firewall says otherwise. This is the one step that stops strangers from holding connections open. Run the installer with `SHUTUP_AND_RUN_NO_UFW=1` so it does not open the port to everyone, or delete that rule afterwards.

```bash
sudo ufw allow from 198.51.100.7 to any port 8787 proto tcp
```

```bash
sudo ufw delete allow 8787/tcp
```

## Call it with the certificate pinned

Verify the fingerprint the installer printed, once, from the caller:

```bash
openssl s_client -connect 203.0.113.10:8787 </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256
```

Then pin the key in curl. `-k` plus `--pinnedpubkey` accepts only that key, whatever name is in the certificate:

```bash
PIN=$(openssl s_client -connect 203.0.113.10:8787 </dev/null 2>/dev/null | openssl x509 -noout -pubkey | openssl pkey -pubin -outform DER | openssl dgst -sha256 -binary | base64)
```

```bash
curl -sk --pinnedpubkey "sha256//$PIN" -H "Authorization: Bearer $SAR_TOKEN" https://203.0.113.10:8787/health
```

Kestra and n8n cannot pin a fingerprint in the HTTP task. They either trust every certificate, which the examples do, or trust a certificate you install on the orchestrator. Copy `/etc/shutup-and-run/cert.pem` there if you want real verification. The certificate names the machine's own address, `127.0.0.1` and `localhost`. If callers use a different public address, replace the certificate with one that names it.

## Try a list of machines from a shell

The same rule the orchestrators use: `202` or `200` stops, `429` and `503` move on, anything else is an error.

```bash
for host in 203.0.113.10 203.0.113.11 203.0.113.12; do
  code=$(curl -sk -m 20 -o /tmp/job.json -w '%{http_code}' -X POST "https://$host:8787/v1/jobs" \
    -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' \
    -d '{"image":"ghcr.io/you/worker","job_key":"order-1001","timeout_secs":3600}')
  case "$code" in
    200|202) echo "accepted by $host"; cat /tmp/job.json; break ;;
    429|503) echo "$host is full or draining" ;;
    *) echo "$host answered $code; stopping"; break ;;
  esac
done
```

It stops on an unexpected answer instead of moving on, because a request that reached a machine and lost its response would run twice. `job_key` protects a retry on the same machine only.

## Cap what a job can use

Without `memory`, Docker does not limit the container. Send limits with every job when the machine is shared with anything else.

```bash
curl -sk -X POST "$SAR/v1/jobs" -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"image":"ghcr.io/you/worker","timeout_secs":900,"cpu":1,"memory":"1g","shm":"256m"}'
```

## A machine behind NAT

The machine asks for work instead of being called. Your orchestrator exposes one URL that returns a task or nothing. `examples/kestra-poll.yml` and `examples/n8n-poll.json` are that URL.

```bash
printf 'SHUTUP_AND_RUN_POLL_URL=https://orchestrator.example/poll\nSHUTUP_AND_RUN_POLL_TOKEN=%s\n' "$(openssl rand -hex 32)" | sudo tee -a /etc/shutup-and-run/env >/dev/null
```

```bash
sudo systemctl restart shutup-and-run
```

The poll token is generated here. Give the same value to the endpoint so it can check it. Keep offering a task with the same `job_key` until its callback arrives. With nobody calling in, you can also close the port: set `listen = "127.0.0.1:8787"` in `/etc/shutup-and-run/config.toml`.

## Take a machine out for maintenance

Drain, wait for `slots_used` to reach zero, do the work, undrain. Callers get `503` and move to the next machine.

```bash
curl -sk -X POST "$SAR/v1/drain" -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"draining":true}'
```

```bash
curl -sk -H "Authorization: Bearer $SAR_TOKEN" "$SAR/health"
```

```bash
curl -sk -X POST "$SAR/v1/drain" -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"draining":false}'
```

## Stop it now

Whoever has a shell on the machine can end everything in two commands. Stopping the service refuses new work and leaves running jobs alone. The second command kills those jobs.

```bash
sudo systemctl stop shutup-and-run
```

```bash
sudo docker ps -q --filter label=shutup-and-run=1 | xargs -r sudo docker kill
```

Keep it off after a reboot:

```bash
sudo systemctl disable shutup-and-run
```

Remove it, and with `--purge` its token, certificate and history:

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash -s -- --uninstall --purge
```

While the service is stopped nothing enforces `timeout_secs`. Kill the jobs if you stop it for long.

## A client lends you a machine

The client runs the installer on their machine and sends you the URL, the fingerprint and the token. What both sides should know before that:

- The token lets its holder start any allowlisted image on that machine, with Docker's default privileges and outbound network. It is not a sandbox. The client is trusting your images.
- The client chooses the allowlist. They should list only your repositories, by full name.
- The client can watch (`sudo docker ps --filter label=shutup-and-run=1`, `journalctl -u shutup-and-run -f`) and can stop everything with the two commands above without asking you.
- You should send `cpu` and `memory` with every job so a job cannot starve the client's own work. The client sets `max_slots` to what they are willing to lend.
- The last log lines of each job are stored on the client's disk. Do not print secrets.
- Use one token per client machine. If the relationship ends, the client uninstalls and the token is dead.

Install with a small slot count and the firewall closed to everyone but you:

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker SHUTUP_AND_RUN_MAX_SLOTS=1 SHUTUP_AND_RUN_NO_UFW=1 bash
```

```bash
sudo ufw allow from 198.51.100.7 to any port 8787 proto tcp
```

A client machine behind NAT uses the poll recipe above instead of the firewall rule.

## Private images

Log the machine in once. The login is the machine's, not the caller's.

```bash
sudo docker login ghcr.io -u YOUR_USER
```

## Change the allowlist or the slot count

```bash
sudo nano /etc/shutup-and-run/config.toml
```

```bash
sudo systemctl restart shutup-and-run
```

A restart does not kill running jobs, and they still count as used slots afterwards.

## Rotate the token

```bash
sudo sed -i "s/^SHUTUP_AND_RUN_TOKEN=.*/SHUTUP_AND_RUN_TOKEN=$(openssl rand -hex 32)/" /etc/shutup-and-run/env
```

```bash
sudo systemctl restart shutup-and-run
```

Update the orchestrator's secret before its next call. Three calls with the old token ban the orchestrator's address. Lift it with `sudo shutup-and-run bans lift <ip>`.

## Renew the certificate

The installer's certificate lasts 825 days. Delete it and run the installer again. The fingerprint changes, so send the new one to the callers.

```bash
sudo rm /etc/shutup-and-run/cert.pem /etc/shutup-and-run/key.pem
```

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash
```

## Monitor it

`/health` and `/metrics` need the token. In Uptime Kuma, add an HTTP monitor for `https://203.0.113.10:8787/health` with the header `Authorization: Bearer <token>` and "ignore TLS error" on. A monitor with a wrong token gets its address banned after three checks.

## Keep the disk from filling

Turn on pruning of finished containers and of images this process pulled, once an hour:

```bash
echo 'prune_interval_secs = 3600' | sudo tee -a /etc/shutup-and-run/config.toml >/dev/null
```

```bash
sudo systemctl restart shutup-and-run
```

Container logs are Docker's. Limit them in `/etc/docker/daemon.json` with `"log-opts": {"max-size": "10m", "max-file": "3"}` and restart Docker.

## Check a machine after any change

`SMOKE_IMAGE` must be on the machine's allowlist. Use your worker if it has `sh` and `sleep`. Otherwise allowlist `ghcr.io/nailtongomes/shutup-and-run-smoke` (a small Python image, the default when `SMOKE_IMAGE` is unset) or `hello-world` (fewer checks, because it has no shell).

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/smoke.sh | sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker bash
```
