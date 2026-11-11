# Sizing

How much RAM and CPU the shutup-and-run process uses, measured with `scripts/size.sh`. This is the launcher, not the job containers. The script is not part of CI.

## Machine

| | |
| --- | --- |
| OS | Linux 6.12.113+, x86_64, KVM |
| CPU | 4 vCPU, Intel Xeon (family 6, model 207), 1 thread per core |
| RAM | 16,387,328 KiB (`MemTotal`, about 15.6 GiB) |
| Docker | 29.1.3, cgroupfs, overlayfs. This daemon was started with iptables off, because the machine has no systemd. |
| Binary | release, `x86_64-unknown-linux-musl`, static-pie, stripped. rustc 1.99.1. |

CPU below is percent of one core, from `utime+stime` in `/proc/<pid>/stat`. RSS is `VmRSS`. Each scenario is a fresh process except the slot ladder, which adds containers to one process. Jobs were `busybox:1.36` running `sleep`, with `memory` set to `32m`.

## Launcher

| Scenario | RSS avg | RSS max | CPU avg | CPU max in 1s |
| --- | --- | --- | --- | --- |
| Idle, reaper every 2s, no jobs | 7.2 MiB | 7.2 MiB | 0.00% | 0.00% |
| 200 POSTs at once, `max_slots` 4 (4 accepted, 196 `429`) | 7.3 MiB | 7.4 MiB | 0.62% | 4.97% |
| 1 running slot | 7.2 MiB | 7.2 MiB | 0.00% | 0.00% |
| 8 running slots | 7.3 MiB | 7.3 MiB | 0.00% | 0.00% |
| 32 running slots | 7.3 MiB | 7.3 MiB | 0.00% | 0.00% |
| 64 running slots | 7.4 MiB | 7.4 MiB | 0.00% | 0.00% |
| Poll every 1s, empty `{}` | 8.0 MiB | 8.0 MiB | 0.13% | 0.99% |
| Reaper every 1s, 32 jobs with `timeout_secs` 2 (all killed in the window) | 7.4 MiB | 7.5 MiB | 0.33% | 1.98% |
| 10,000 finished rows in SQLite, no reads | 7.1 MiB | 7.1 MiB | 0.00% | 0.00% |
| Same history, reading `GET /v1/jobs/{id}?logs=true` as fast as one client can | 9.3 MiB | 9.4 MiB | 49.60% | 63.54% |

The history file was 47,407,104 bytes for 10,000 rows, each with 4,608 bytes of logs. Opening it did not raise RSS. The read row is 22,176 GETs in about 8 seconds from localhost, plus `/metrics`. That is a hammer, not what an orchestrator does. A 15 second idle sample recorded no CPU tick at all.

## Docker, per running slot

`containerd-shim` RSS, and the growth of `dockerd` plus `containerd` over the idle baseline of 169,396 KiB. The container column is the job's own cgroup usage (`docker stats`), which for `sleep` is not the launcher and not the shim.

| Slots | Shim total | Shim each | Daemon above idle | Container RSS avg |
| --- | --- | --- | --- | --- |
| 1 | 10.7 MiB | 10.7 MiB | 11.0 MiB | 0.4 MiB |
| 8 | 85.4 MiB | 10.7 MiB | 11.9 MiB | 0.4 MiB |
| 32 | 344 MiB | 10.7 MiB | 23.3 MiB | 0.4 MiB |
| 64 | 688 MiB | 10.7 MiB | 39.3 MiB | 0.4 MiB |

The shim is about 11 MiB for every slot. The daemon's extra cost is about 0.6 MiB per slot once it is already running several. The first slot moved the daemon by 11 MiB, which did not keep growing at that rate.

## What to reserve

Reserve **64 MiB of RAM and 0.25 CPU** for the shutup-and-run process on a worker. The process stayed under 10 MiB RSS in every scenario here, and under 5% of one core except while a local client read logs as fast as it could. 64 MiB is about seven times the measured RSS. 0.25 CPU covers the `429` burst and a busy status poller, and it is more than the reaper or an empty poll used.

Keep that separate from the jobs. Each running slot also costs about **12 MiB** of Docker (one shim, plus a share of the daemon) before the container uses any of its `memory` limit. A host with `max_slots = 32` and a 2 GiB limit on each job needs room for the 32 limits, about 384 MiB of Docker overhead, and the 64 MiB above. The launcher does not get smaller or larger with `max_slots`.
