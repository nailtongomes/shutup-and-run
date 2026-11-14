# Examples

Each file is something to import and change, not a library. The runner has no code for any orchestrator: all of these speak the contract in [docs/INTEGRATION-CONTRACT.md](../docs/INTEGRATION-CONTRACT.md).

No orchestrator yet? `shutup-and-run demo` runs one container on the machine with nothing else, and `shutup-and-run job validate task.json` checks a task before any of these sends it.

## End to end, in five steps

| Step | n8n | Kestra |
| --- | --- | --- |
| 1. Dispatch a container job | `n8n/push-to-linux.json`: **Job**, **POST /v1/jobs** | `kestra-e2e.yml`: `dispatch` |
| 2. Supply a job key | `job_key` in the **Job** node | `job_key` is `execution.id` |
| 3. Receive the completion callback | `n8n/worker-orchestrator.json`: webhook `n3-callback` | `kestra-e2e-callback.yml`: the Webhook trigger |
| 4. Inspect the status | `n8n/push-to-linux.json`: **Wait 20 s**, **GET /v1/jobs/{id}** | `kestra-e2e.yml`: `wait`, `status` |
| 5. Succeeded or failed | **Succeeded, failed or still running** | `failed`, `succeeded` |

A job failed when `reason` is `start_failed` or `timeout_kill`, or `exit_code` is not `0`. `202` only means the machine took the job.

Step 4 needs the orchestrator to reach the runner, so it is for a Linux machine. A Windows worker is behind the PC's NAT and pulls: there the callback (step 3) is the only way the result comes back.

**The least you have to set**

- n8n: import the two files in `n8n/`, put the runner's URL and token in the **Job** node, and the callback secret where `REPLACE_WITH_CALLBACK_SECRET` is. Steps, a queue with worker approval for pull mode, and a smoke test: [n8n/README.md](n8n/README.md).
- Kestra: create the secret `SHUTUP_AND_RUN_TOKEN`, import `kestra-e2e.yml` and `kestra-e2e-callback.yml`, replace `REPLACE_WITH_WEBHOOK_KEY` in both, and set the `runner` input to the machine's address.

The n8n files were run against a local n8n and a local runner. The Kestra files were written against Kestra's documented tasks and were not run here; they are for Kestra 0.20 or newer.

## Everything here

| File | |
| --- | --- |
| `n8n/push-to-linux.json` | Push one job, route on the answer, read its status. |
| `n8n/worker-orchestrator.json` | A poll URL in n8n: approval of machines, a queue, a manifest, the callback receiver. |
| `n8n-dispatch.json`, `kestra-dispatch.yml` | Push to the first of two machines with a free slot. |
| `n8n-poll.json`, `kestra-poll.yml` | The smallest poll URL: one task, then nothing. |
| `kestra-e2e.yml`, `kestra-e2e-callback.yml` | The five steps above. |
| `smoke-image/` | The small image `scripts/smoke.sh` runs. |
| `selenium-probe/` | A browser job that reports its own peak memory, for sizing. |
