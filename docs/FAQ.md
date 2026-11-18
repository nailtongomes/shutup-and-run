# FAQ

[Português](FAQ.pt-BR.md)

What to expect from a worker on a customer's PC, in short answers. Each one says where to check. The details are in [README.md](../README.md), [WINDOWS.md](WINDOWS.md) and, for an agent, [FOR-AGENTS.md](FOR-AGENTS.md).

**Contents**

- The PC: [Will it get slow?](#will-the-pc-get-slow) · [How much does a job take?](#how-much-of-the-pc-does-a-job-take) · [Laptop on battery](#what-happens-on-a-laptop-on-its-battery) · [Meetings and presentations](#and-during-a-meeting-or-a-presentation) · [Only at some hours](#can-it-lend-the-pc-only-at-night) · [My files](#can-a-job-see-my-files)
- A running job: [Is it paused and resumed?](#is-a-running-container-paused-and-resumed) · [What can affect it?](#what-can-affect-a-job-that-is-already-running) · [Spikes of CPU or memory](#is-there-a-tolerance-for-a-short-spike-of-cpu-or-memory) · [Turning the PC off](#can-i-turn-the-pc-off-with-a-job-running) · [Internet down](#what-if-the-internet-goes-down) · [Retries](#is-a-failed-job-run-again)
- When nothing happens: [Why did no job start?](#why-did-no-job-start) · [Waiting for your authorization](#what-does-waiting-for-your-authorization-mean) · [Stop lending for a while](#how-do-i-stop-lending-for-a-while)
- Afterwards: [What a job printed](#where-do-i-see-what-a-job-printed) · [What a job used](#how-much-did-a-job-use) · [What the operator learns](#what-does-the-poll-url-learn-about-this-pc) · [Update](#how-do-i-update) · [Uninstall](#how-do-i-uninstall)

## The PC

### Will the PC get slow?

It can, a little; the worker is built so the person's work comes first. On Windows the jobs run inside WSL2, which may take at most about 40% of the RAM and half of the processors. Each job is held to 1 CPU and 2 GiB (less on a small PC), one job at a time. No job starts while memory, disk or processor are short, and none starts on a small or busy PC unless its owner agreed. This reduces the risk; it does not remove it: a job that is already running keeps its share until it ends.

Check: the local page `http://127.0.0.1:8787/`, **Computer resources**.

### How much of the PC does a job take?

At most its limits: by default 1 CPU and 2 GiB of memory, with no swap beyond that. A job that asks for more is refused, never cut down. What a finished job really used is recorded too: see [How much did a job use?](#how-much-did-a-job-use).

Check: `shutup-and-run config show` (the limits in force), `shutup-and-run jobs`.

### What happens on a laptop on its battery?

- Plugged in: nothing changes.
- On the battery at 60% or more: jobs still start. The page shows **This PC is on its battery** with the charge, and the doctor says `ON_BATTERY`.
- On the battery below 60%: no new job starts and the worker does not ask for work, until the PC is plugged in. The page says **WAITING**; the code is `BATTERY_LOW`.

A job that is already running is never stopped by the battery. 60% is fixed; no setting changes it. A PC with no battery is never held by this.

### And during a meeting or a presentation?

While Windows says the person is presenting, or has a program in full screen (a slide show, a video or a game in full screen, a call in full screen), no new job starts. When that ends, jobs start again by themselves. A call in a window, not in full screen, is not noticed. The code is `USER_PRESENTING`.

To also wait while the person is typing or using the mouse: `shutup-and-run config set min_idle_secs 300 --apply` (five minutes without input).

### Can it lend the PC only at night?

Yes, with `lend_hours`, on the PC's own clock:

```
shutup-and-run config set lend_hours "mon-fri 19-07; sat,sun" --apply
```

That is weekday nights and all weekend. A window whose end is before its start runs past midnight and belongs to the day it starts (`fri 19-07` ends Saturday at 7). Outside the windows no new job starts, the worker does not ask for work, and the page says **WAITING** (`OUTSIDE_LEND_HOURS`). A running job goes on past the end of the window. `config unset lend_hours --apply` lends at any time again. The page shows the window and the time now, so a wrong clock is easy to see.

### Can a job see my files?

No. A job gets no folder of the PC: no mount, no Windows drive (WSL2 is installed without them), no port. It sees its own container and the network. The one exception is a certificate the owner chooses to share, which every job can then read; it is off unless the owner turns it on, on the PC.

## A running job

### Is a running container paused and resumed?

No. The worker never pauses, freezes or resumes a container. A job runs from its start to its end or to its time limit, in one go. What can look like a pause:

- **The CPU limit.** A job that wants more than its CPU limit is slowed down by the kernel, in slices of 100 ms. It is not stopped. If that happens most of its time, it is recorded as an incident (`cpu_throttled`).
- **The pause button** (or `shutup-and-run pause`). It stops new jobs only; a running job goes on to its end.
- **Sleep or hibernation of the PC.** WSL2 sleeps with Windows, and the job is frozen with everything else; it carries on when the PC wakes. Its time limit is counted on the clock, so after a long sleep it may end as `timeout` soon after waking.

### What can affect a job that is already running?

These can:

| What | What happens to the job | How it is reported |
| --- | --- | --- |
| Its time limit (`timeout_secs`) | killed | `reason` `timeout` |
| Its memory limit | killed by the kernel at once | `oom_killed: true` |
| Its CPU limit | slowed down, not stopped | an incident `cpu_throttled` |
| The PC shuts down, restarts or signs out | Windows asks first while a job runs; "shut down anyway" ends it | `reason` `interrupted`, reported once, not run again |
| A power cut, a crash of the PC or of Docker | it ends | `interrupted` |
| `shutup-and-run wsl stop`, an update, an uninstall | SIGTERM, then 10 seconds, then killed | `interrupted`, or how it exited |
| Sleep or hibernation | frozen, then goes on | see above |

These do **not** stop a running job: the person coming back to the PC, the PC's memory or CPU getting busy, a safety block, an authorization withdrawn, the battery going under 60%, a presentation starting, the end of `lend_hours`, the pause button, or a restart of the worker's own service (the container keeps running and is picked up again). They only keep new jobs from starting.

### Is there a tolerance for a short spike of CPU or memory?

For a job that is running, there is nothing to tolerate: no reading of the PC ever stops it. Only its own limits apply, and those are hard: memory over the limit is killed at once, CPU over the limit is slowed.

For a new job, the readings are taken at the moment it would start (and before each poll), not watched in between:

- The PC's CPU use is the average of the last 10 seconds; inside WSL2 or on Linux it is the load average of the last minute. A burst of a second or two barely moves either.
- Memory and disk are what is free at that moment.
- A spike between two of those moments changes nothing. A spike at that moment makes the job wait, not fail: a pull worker simply does not ask for work and looks again at the next poll, 30 seconds later; a push gets `503` and can be sent again.
- A critical level clears only past a recovery line, so the worker does not flip on and off around it: the PC's processors block at 95% and clear under 85%; memory blocks under 10% available (or 1 GiB) and clears at 15% (1.5 GiB); disk blocks under 2 GiB free and clears at 3 GiB.
- Three jobs killed for memory within 30 minutes pause new jobs for 15 minutes.

There is no "must last N seconds" rule beyond those averages, and the thresholds are fixed.

### Can I turn the PC off with a job running?

Better not. While a job runs, Windows shows the usual "this app is preventing shutdown" screen with the reason. Waiting is best. If you shut down anyway, the job is stopped and reported as `interrupted`, once, and it is not run again: the service that sent it decides what to do. The page and the desktop shortcut say whether the PC can be turned off now.

### What if the internet goes down?

A running job goes on, unless it needs the network itself. The worker keeps asking for work and fails quietly until the network is back. The result of a job that ended is sent again once a minute, for up to an hour; after that it is given up, and the page and `status` count it.

### Is a failed job run again?

No. A job runs once. How it ended goes to the callback, once, and the service that sent it decides whether to send it again. The worker keeps no queue.

## When nothing happens

### Why did no job start?

The page's first line says why, and so do `shutup-and-run doctor` and `shutup-and-run status --json` (`waiting_for`). The usual reasons, with their codes:

| The page says | Code | It clears |
| --- | --- | --- |
| PAUSED | `WORKER_PAUSED` | when someone resumes, or at the next restart of the PC |
| WAITING FOR YOUR AUTHORIZATION | `AUTHORIZATION_REQUIRED` | when the owner decides, see below |
| NEW JOBS BLOCKED FOR SAFETY | `SAFETY_*` | by itself, when the PC recovers |
| WAITING | `OUTSIDE_LEND_HOURS`, `BATTERY_LOW`, `USER_PRESENTING` | by itself |
| waiting: the PC is in use | `USER_ACTIVE` | by itself (`min_idle_secs`) |
| waiting, not enough resources | `LOW_MEMORY`, `LOW_DISK` | by itself |
| busy, every slot is taken | `WORKER_BUSY` | when the running job ends |
| machine not approved yet | `POLL_NOT_APPROVED` | when the poll URL approves this PC |
| DOCKER IS DOWN | `DOCKER_UNAVAILABLE` | follow the doctor's hint |

While it waits, the worker does not ask for work, so no task is taken and then dropped. Every code has a one-line next step in [FOR-AGENTS.md](FOR-AGENTS.md).

### What does "waiting for your authorization" mean?

The PC is small (under 8 GiB of memory or 2 processors or fewer), or busy, or Windows is not reporting it, so running a job is a real risk for whoever uses it. The owner reads the risks once on the page (**Safety**) and decides; `shutup-and-run safety authorize` in a terminal does the same. The yes is kept, and asked again only for a new risk or a new version of the notice. It never lifts a block.

When the owner already agreed in the contract, whoever installs records that at the machine, and the PC never asks:

```
shutup-and-run wsl install --poll-url URL --owner-consent
shutup-and-run safety authorize --owner-consent      # on a PC already installed
```

The owner can still withdraw it on the page.

### How do I stop lending for a while?

The **Pause** button on the page, or `shutup-and-run pause`. No new job starts; a running one finishes. `shutup-and-run resume`, or the button again, ends it; on a PC with the page, a restart ends it too, so a forgotten pause does not last forever. To lend only at some hours, use `lend_hours` instead.

## Afterwards

### Where do I see what a job printed?

`shutup-and-run jobs logs ID` (or `--last`), or **Jobs and logs** on the page, which can also download it. The last 256 KiB of each job are kept on the PC for 7 days, not masked, and are never sent anywhere.

### How much did a job use?

Each finished job records its CPU time and the most memory it held: `shutup-and-run jobs` (a `used:` line), `jobs --json` (`cpu_ms`, `memory_peak_bytes`) and the **CPU / memory** column on the page. They come from a reading every 30 seconds while it runs, so a job shorter than that may have none, and a short peak can be missed. Compare them with the limits to see whether the PC has room for more, or for a second slot.

### What does the poll URL learn about this PC?

When it asks for work: the worker's name, the system, the processor type, the version, how many slots it has and has free, its processors and its memory. Not the config, not a token, not the job history, not what jobs printed, not the address. What jobs used stays on the PC too.

### How do I update?

Run the install line again (`irm .../install.ps1 | iex` on Windows). It stops the worker, swaps it, keeps the config, the token and the history, and puts the old version back if the new one does not start. Details: [WINDOWS.md](WINDOWS.md#update-and-repair).

### How do I uninstall?

`shutup-and-run wsl uninstall` removes the scheduled task and the WSL2 distribution with its containers and history; `--purge` also removes `C:\ProgramData\shutup-and-run`. On Linux, the uninstall of [the installer](../README.md#installers).
