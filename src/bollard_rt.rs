use crate::launcher::{valid_container_id, Launcher};
use crate::registry::{
    auth_from_docker_config, auth_from_helper_output, default_docker_config_path, pull_reference,
    valid_helper_name, AuthSource, RegistryAuth,
};
use crate::runtime::{Inspected, Runtime, RuntimeError, StartSpec};
use crate::{DEADLINE_KEY, LABEL_KEY, LABEL_VALUE, NAME_PREFIX};
use async_trait::async_trait;
use bollard::auth::DockerCredentials;
use bollard::models::{
    ContainerCreateBody, ContainerState, ContainerStateStatusEnum, ContainerSummary, EventMessage,
    HostConfig, HostConfigLogConfig,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, EventsOptionsBuilder,
    InspectContainerOptions, InspectContainerOptionsBuilder, ListContainersOptionsBuilder,
    RemoveContainerOptionsBuilder, StartContainerOptions, StatsOptionsBuilder,
    StopContainerOptionsBuilder,
};
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct BollardRuntime {
    docker: Docker,
    docker_config: Option<PathBuf>,
}

impl BollardRuntime {
    pub fn new(docker: Docker, docker_config: Option<PathBuf>) -> Self {
        Self {
            docker,
            docker_config,
        }
    }

    fn config_path(&self) -> PathBuf {
        self.docker_config
            .clone()
            .unwrap_or_else(default_docker_config_path)
    }

    async fn credentials_for(&self, image: &str) -> Option<DockerCredentials> {
        let path = self.config_path();
        let image = image.to_string();
        tokio::task::spawn_blocking(move || load_credentials(&path, &image))
            .await
            .ok()
            .flatten()
    }

    async fn pull_once(
        &self,
        reference: &str,
        creds: Option<DockerCredentials>,
    ) -> Result<(), RuntimeError> {
        let options = CreateImageOptionsBuilder::new()
            .from_image(reference)
            .build();
        let mut stream = self.docker.create_image(Some(options), None, creds);
        while let Some(item) = stream.next().await {
            item.map_err(docker_err)?;
        }
        Ok(())
    }

    async fn image_present(&self, image: &str) -> bool {
        self.docker.inspect_image(image).await.is_ok()
    }

    async fn list_labeled(&self, status: &str) -> Result<Vec<ContainerSummary>, RuntimeError> {
        let filters = HashMap::from([
            (
                "label".to_string(),
                vec![format!("{LABEL_KEY}={LABEL_VALUE}")],
            ),
            ("status".to_string(), vec![status.to_string()]),
        ]);
        let options = ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        self.docker
            .list_containers(Some(options))
            .await
            .map_err(docker_err)
    }
}

#[async_trait]
impl Runtime for BollardRuntime {
    async fn pull(&self, image: &str) -> Result<(), RuntimeError> {
        let reference = pull_reference(image);
        // An image named by its digest cannot change: once here, the
        // registry is not asked again, so no network and no wait.
        if pinned(image)
            && (self.image_present(image).await
                || (reference != image && self.image_present(&reference).await))
        {
            return Ok(());
        }
        let creds = self.credentials_for(image).await;
        // shortcut: pull a tag on every job so it stays current, then fall back
        // to a local image if the registry is unreachable. Upgrade to
        // IfNotPresent for tags when registry latency dominates.
        let mut authed_err = None;
        if creds.is_some() {
            match self.pull_once(&reference, creds).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!(error = %err, %image, "authenticated pull failed; trying anonymous");
                    authed_err = Some(err);
                }
            }
        }
        match self.pull_once(&reference, None).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let err = authed_err.unwrap_or(err);
                let local = self.image_present(image).await
                    || (reference != image && self.image_present(&reference).await);
                if local {
                    tracing::warn!(error = %err, %image, "pull failed; using the local image");
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }

    async fn running_count(&self) -> Result<usize, RuntimeError> {
        let list = self.list_labeled("running").await?;
        Ok(list.iter().filter(|c| is_ours(&c.labels)).count())
    }

    async fn start(&self, id: &str, spec: &StartSpec) -> Result<(), RuntimeError> {
        let mut labels = HashMap::new();
        labels.insert(LABEL_KEY.to_string(), LABEL_VALUE.to_string());
        labels.insert(DEADLINE_KEY.to_string(), spec.deadline_unix.to_string());
        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            env: if spec.env.is_empty() {
                None
            } else {
                Some(spec.env.clone())
            },
            cmd: spec.command.clone(),
            labels: Some(labels),
            host_config: Some(HostConfig {
                // AutoRemove would delete the container before we can read logs.
                // We remove it ourselves after the log tail is stored.
                auto_remove: Some(false),
                nano_cpus: spec.nano_cpus,
                memory: spec.memory_bytes,
                // shortcut: MemorySwap == Memory disables extra swap. Upgrade if a
                // job must page. No restart policy: a failed container stays stopped.
                memory_swap: spec.memory_bytes,
                shm_size: spec.shm_bytes,
                binds: (!spec.binds.is_empty()).then(|| spec.binds.clone()),
                log_config: Some(log_config()),
                // A job that forks without end (a loop, a browser spawning
                // itself) stops at this many processes, not at the PC's
                // limit; and a small init reaps the zombies a browser leaves.
                pids_limit: Some(PIDS_LIMIT),
                init: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let options = CreateContainerOptionsBuilder::new()
            .name(&container_ref(id))
            .build();
        let created = self
            .docker
            .create_container(Some(options), body)
            .await
            .map_err(docker_err)?;
        if !created.warnings.is_empty() {
            tracing::warn!(id = %created.id, warnings = ?created.warnings, "create warnings");
        }
        if let Err(err) = self
            .docker
            .start_container(&created.id, None::<StartContainerOptions>)
            .await
        {
            let remove = RemoveContainerOptionsBuilder::new()
                .force(true)
                .v(true)
                .build();
            if let Err(rm) = self
                .docker
                .remove_container(&created.id, Some(remove))
                .await
            {
                tracing::warn!(id = %created.id, error = %rm, "could not remove the container that failed to start");
            }
            return Err(docker_err(err));
        }
        Ok(())
    }

    async fn inspect(&self, id: &str) -> Result<Inspected, RuntimeError> {
        match self
            .docker
            .inspect_container(&container_ref(id), None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => Ok(map_state(info.state)),
            Err(err) if is_status(&err, 404) => Ok(Inspected::NotFound),
            Err(err) => Err(docker_err(err)),
        }
    }

    async fn oom_killed(&self, id: &str) -> Result<bool, RuntimeError> {
        match self
            .docker
            .inspect_container(&container_ref(id), None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => Ok(info.state.and_then(|s| s.oom_killed).unwrap_or(false)),
            Err(err) if is_status(&err, 404) => Ok(false),
            Err(err) => Err(docker_err(err)),
        }
    }

    async fn disk_written(&self, id: &str) -> Result<Option<u64>, RuntimeError> {
        // Docker walks the container's layer to size it: bounded, since a
        // job that wrote millions of files must not hold its own end.
        let options = InspectContainerOptionsBuilder::new().size(true).build();
        let read = tokio::time::timeout(
            Duration::from_secs(20),
            self.docker
                .inspect_container(&container_ref(id), Some(options)),
        )
        .await;
        match read {
            Ok(Ok(info)) => Ok(info.size_rw.map(|bytes| bytes.max(0) as u64)),
            Ok(Err(err)) if is_status(&err, 404) => Ok(None),
            Ok(Err(err)) => Err(docker_err(err)),
            Err(_) => Ok(None),
        }
    }

    async fn stats(&self, id: &str) -> Result<Option<crate::runtime::Stats>, RuntimeError> {
        // One reading, not a stream: the counters are totals since the start.
        let options = StatsOptionsBuilder::default()
            .stream(false)
            .one_shot(true)
            .build();
        let mut stats = self.docker.stats(&container_ref(id), Some(options));
        let read = tokio::time::timeout(Duration::from_secs(5), stats.next()).await;
        match read {
            Ok(Some(Ok(stats))) => {
                let cpu = stats.cpu_stats.as_ref();
                let memory = stats.memory_stats.as_ref();
                // cgroup v2 calls the page cache inactive_file, v1 total_inactive_file.
                let cache = memory
                    .and_then(|m| m.stats.as_ref())
                    .and_then(|s| {
                        s.get("inactive_file")
                            .or_else(|| s.get("total_inactive_file"))
                    })
                    .copied()
                    .unwrap_or(0);
                Ok(Some(crate::runtime::Stats {
                    throttling: cpu
                        .and_then(|cpu| cpu.throttling_data.as_ref())
                        .and_then(|data| {
                            Some(crate::runtime::Throttling {
                                periods: data.periods?,
                                throttled_periods: data.throttled_periods?,
                            })
                        }),
                    cpu_ns: cpu
                        .and_then(|cpu| cpu.cpu_usage.as_ref())
                        .and_then(|usage| usage.total_usage)
                        .filter(|ns| *ns > 0),
                    memory_bytes: memory
                        .and_then(|m| m.usage)
                        .filter(|bytes| *bytes > 0)
                        .map(|bytes| bytes.saturating_sub(cache)),
                    network_bytes: stats.networks.as_ref().map(|networks| {
                        networks
                            .values()
                            .map(|n| n.rx_bytes.unwrap_or(0) + n.tx_bytes.unwrap_or(0))
                            .sum()
                    }),
                }))
            }
            Ok(Some(Err(err))) if is_status(&err, 404) => Ok(None),
            Ok(Some(Err(err))) => Err(docker_err(err)),
            Ok(None) => Ok(None),
            Err(_) => Err(RuntimeError::new("docker stats did not answer in 5 s")),
        }
    }

    async fn applied(&self, id: &str) -> Result<Option<crate::runtime::Applied>, RuntimeError> {
        match self
            .docker
            .inspect_container(&container_ref(id), None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => Ok(info.host_config.map(|host| crate::runtime::Applied {
                nano_cpus: host.nano_cpus.unwrap_or(0),
                memory_bytes: host.memory.unwrap_or(0),
                memory_swap: host.memory_swap.unwrap_or(0),
            })),
            Err(err) if is_status(&err, 404) => Ok(None),
            Err(err) => Err(docker_err(err)),
        }
    }

    async fn list_expired(&self, now_unix: u64) -> Result<Vec<String>, RuntimeError> {
        let list = self.list_labeled("running").await?;
        let mut ids = Vec::new();
        for container in list {
            if !is_ours(&container.labels) {
                continue;
            }
            let Some(id) = job_id_of(&container) else {
                continue;
            };
            let deadline = container
                .labels
                .as_ref()
                .and_then(|labels| labels.get(DEADLINE_KEY))
                .and_then(|value| value.parse::<u64>().ok());
            // No deadline would pin the slot until someone kills it by hand.
            if deadline.map(|value| value <= now_unix).unwrap_or(true) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    async fn kill(&self, id: &str) -> Result<(), RuntimeError> {
        // SIGTERM (or the image's STOPSIGNAL), then SIGKILL after the grace:
        // a job gets the chance to write its result and close its browser.
        let options = StopContainerOptionsBuilder::new()
            .t(KILL_GRACE_SECS)
            .build();
        match self
            .docker
            .stop_container(&container_ref(id), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            // Gone, or already stopped.
            Err(err) if is_status(&err, 404) || is_status(&err, 304) || is_status(&err, 409) => {
                Ok(())
            }
            Err(err) => Err(docker_err(err)),
        }
    }

    async fn cleanup_created(&self) -> Result<usize, RuntimeError> {
        // shortcut: only recovers a crash between create and start. Upgrade if
        // containers are ever created without an immediate start.
        let list = self.list_labeled("created").await?;
        let mut removed = 0usize;
        for container in list {
            if !is_ours(&container.labels) {
                continue;
            }
            let Some(id) = container.id else { continue };
            let options = RemoveContainerOptionsBuilder::new()
                .force(true)
                .v(true)
                .build();
            self.docker
                .remove_container(&id, Some(options))
                .await
                .map_err(docker_err)?;
            removed += 1;
        }
        Ok(removed)
    }

    async fn tail_logs(&self, id: &str, lines: usize) -> Result<String, RuntimeError> {
        let tail = lines.max(1).to_string();
        let options = bollard::query_parameters::LogsOptionsBuilder::new()
            .stdout(true)
            .stderr(true)
            .tail(&tail)
            .build();
        let mut stream = self.docker.logs(&container_ref(id), Some(options));
        let mut buf = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => buf.push_str(&chunk.to_string()),
                Err(err) if is_status(&err, 404) => break,
                Err(err) => return Err(docker_err(err)),
            }
        }
        Ok(buf)
    }

    async fn output(&self, id: &str, max_bytes: usize) -> Result<Vec<u8>, RuntimeError> {
        // All of it is read, and only the end is held: twice the limit at
        // most, trimmed as it grows.
        let options = bollard::query_parameters::LogsOptionsBuilder::new()
            .stdout(true)
            .stderr(true)
            .build();
        let mut stream = self.docker.logs(&container_ref(id), Some(options));
        let mut kept: Vec<u8> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    kept.extend_from_slice(&chunk.into_bytes());
                    if kept.len() > 2 * max_bytes {
                        kept.drain(..kept.len() - max_bytes);
                    }
                }
                Err(err) if is_status(&err, 404) => break,
                Err(err) => return Err(docker_err(err)),
            }
        }
        Ok(crate::joblogs::clip(&kept, max_bytes).to_vec())
    }

    async fn remove(&self, id: &str) -> Result<(), RuntimeError> {
        // `v`: the anonymous volumes an image's VOLUME made go with it, or
        // they would stay on the PC's disk after every job.
        let options = RemoveContainerOptionsBuilder::new()
            .force(true)
            .v(true)
            .build();
        match self
            .docker
            .remove_container(&container_ref(id), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if is_status(&err, 404) => Ok(()),
            Err(err) => Err(docker_err(err)),
        }
    }

    async fn list_stopped(&self) -> Result<Vec<String>, RuntimeError> {
        let mut ids = Vec::new();
        for status in ["exited", "dead"] {
            for container in self.list_labeled(status).await? {
                if !is_ours(&container.labels) {
                    continue;
                }
                if let Some(id) = job_id_of(&container) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    async fn image_in_use(&self, image: &str) -> Result<bool, RuntimeError> {
        let filters = HashMap::from([("ancestor".to_string(), vec![image.to_string()])]);
        let options = ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        let list = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(docker_err)?;
        Ok(!list.is_empty())
    }

    async fn remove_image(&self, image: &str) -> Result<(), RuntimeError> {
        let options = bollard::query_parameters::RemoveImageOptionsBuilder::new()
            .force(false)
            .build();
        match self.docker.remove_image(image, Some(options), None).await {
            Ok(_) => Ok(()),
            // Already gone, removed by hand or by an earlier pass.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(err) => Err(docker_err(err)),
        }
    }

    async fn prune_dangling(&self) -> Result<u64, RuntimeError> {
        // With no filter Docker prunes dangling images only.
        let answer = self
            .docker
            .prune_images(None::<bollard::query_parameters::PruneImagesOptions>)
            .await
            .map_err(docker_err)?;
        Ok(answer.space_reclaimed.unwrap_or(0).max(0) as u64)
    }
}

pub fn spawn_exit_watcher(docker: Docker, launcher: Arc<Launcher>) {
    tokio::spawn(async move {
        loop {
            if let Err(err) = watch_once(&docker, &launcher).await {
                tracing::warn!(error = %err, "docker event stream ended");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn watch_once(docker: &Docker, launcher: &Launcher) -> Result<(), RuntimeError> {
    let filters = HashMap::from([
        ("type".to_string(), vec!["container".to_string()]),
        ("event".to_string(), vec!["die".to_string()]),
        (
            "label".to_string(),
            vec![format!("{LABEL_KEY}={LABEL_VALUE}")],
        ),
    ]);
    let options = EventsOptionsBuilder::new().filters(&filters).build();
    let mut stream = docker.events(Some(options));
    while let Some(item) = stream.next().await {
        let event = item.map_err(docker_err)?;
        if let Some((id, code)) = exit_from_event(&event) {
            launcher.on_exit(&id, code).await;
        }
    }
    Ok(())
}

fn exit_from_event(event: &EventMessage) -> Option<(String, i64)> {
    let actor = event.actor.as_ref()?;
    let attrs = actor.attributes.as_ref()?;
    let by_label = attrs
        .get(LABEL_KEY)
        .is_some_and(|value| value == LABEL_VALUE);
    let name = attrs.get("name").map(|name| name.trim_start_matches('/'));
    let by_name = name.is_some_and(|name| name.starts_with(NAME_PREFIX));
    if !by_label && !by_name {
        return None;
    }
    let id = name
        .and_then(job_id_from_name)
        .or_else(|| actor.id.clone().filter(|id| valid_container_id(id)))?;
    let code = attrs.get("exitCode")?.parse().ok()?;
    Some((id, code))
}

fn is_ours(labels: &Option<HashMap<String, String>>) -> bool {
    labels
        .as_ref()
        .and_then(|labels| labels.get(LABEL_KEY))
        .is_some_and(|value| value == LABEL_VALUE)
}

fn map_state(state: Option<ContainerState>) -> Inspected {
    let Some(state) = state else {
        return Inspected::NotFound;
    };
    match state.status {
        Some(
            ContainerStateStatusEnum::RUNNING
            | ContainerStateStatusEnum::PAUSED
            | ContainerStateStatusEnum::RESTARTING
            | ContainerStateStatusEnum::CREATED,
        ) => Inspected::Running,
        _ => Inspected::Exited {
            exit_code: state.exit_code.unwrap_or(-1),
        },
    }
}

fn load_credentials(path: &std::path::Path, image: &str) -> Option<DockerCredentials> {
    let text = std::fs::read_to_string(path).ok()?;
    let source = match auth_from_docker_config(&text, image) {
        Ok(source) => source,
        Err(err) => {
            tracing::warn!(error = %err, "ignoring docker config.json");
            return None;
        }
    };
    let auth = match source {
        AuthSource::Anonymous => return None,
        AuthSource::Basic(auth) => auth,
        AuthSource::Helper { helper, server } => match run_helper(&helper, &server) {
            Ok(auth) => auth,
            Err(err) => {
                tracing::warn!(error = %err, "credential helper failed; anonymous pull");
                return None;
            }
        },
    };
    Some(to_docker_credentials(auth, image))
}

fn to_docker_credentials(auth: RegistryAuth, image: &str) -> DockerCredentials {
    DockerCredentials {
        username: auth.username,
        password: auth.password,
        identitytoken: auth.identitytoken,
        serveraddress: Some(crate::registry::registry_host(image).to_string()),
        ..Default::default()
    }
}

fn run_helper(helper: &str, server: &str) -> Result<RegistryAuth, String> {
    if !valid_helper_name(helper) {
        return Err("invalid credential helper name".into());
    }
    // shortcut: no timeout around the helper. Upgrade with a 5s cap if one hangs.
    // The server name goes on stdin, never on the command line.
    let bin = format!("docker-credential-{helper}");
    let mut child = std::process::Command::new(&bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run {bin}: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(server.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("credential helper: {err}"))?;
    if !output.status.success() {
        return Err(format!("credential helper exited with {}", output.status));
    }
    auth_from_helper_output(&String::from_utf8_lossy(&output.stdout))
}

/// The most processes one container may hold. A constant: it keeps a job
/// that forks without end from taking the PC with it.
const PIDS_LIMIT: i64 = 4096;

/// Seconds between SIGTERM and SIGKILL at a timeout or a stop.
pub const KILL_GRACE_SECS: i32 = 10;

/// `repo@sha256:...`: the one image reference that cannot change.
fn pinned(image: &str) -> bool {
    image.contains("@sha256:")
}

/// What Docker keeps of a container's output: two files of 5 MB at most,
/// so a job or a managed container that prints for days does not fill the
/// disk. The job's own log file is the last 256 KiB of it.
fn log_config() -> HostConfigLogConfig {
    HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(HashMap::from([
            ("max-size".to_string(), "5m".to_string()),
            ("max-file".to_string(), "2".to_string()),
        ])),
    }
}

/// The job id is chosen before the image is pulled, so the container is named
/// after it and addressed by that name. A 64 character id is a row written
/// before this change, when the job id was the container id itself.
fn container_ref(id: &str) -> String {
    if id.len() == 64 {
        id.to_string()
    } else {
        format!("{NAME_PREFIX}{id}")
    }
}

fn job_id_from_name(name: &str) -> Option<String> {
    name.trim_start_matches('/')
        .strip_prefix(NAME_PREFIX)
        .filter(|id| valid_container_id(id))
        .map(str::to_string)
}

fn job_id_of(container: &ContainerSummary) -> Option<String> {
    container
        .names
        .iter()
        .flatten()
        .find_map(|name| job_id_from_name(name))
        .or_else(|| container.id.clone())
}

fn is_status(err: &bollard::errors::Error, code: u16) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError { status_code, .. } if *status_code == code
    )
}

fn docker_err(err: bollard::errors::Error) -> RuntimeError {
    RuntimeError::new(err.to_string())
}

/// The local Docker socket. `DOCKER_HOST` may name another unix socket.
pub fn connect_docker() -> Result<bollard::Docker, String> {
    match std::env::var("DOCKER_HOST") {
        Ok(host) if host.starts_with("unix://") => {
            let path = host.trim_start_matches("unix://");
            bollard::Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|err| err.to_string())
        }
        Ok(host) => Err(format!(
            "DOCKER_HOST={host} is not a local socket. shutup-and-run only talks to a unix socket."
        )),
        Err(_) => bollard::Docker::connect_with_socket_defaults().map_err(|err| err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_digest_pins_an_image() {
        assert!(pinned("ghcr.io/acme/job@sha256:0123abcd"));
        assert!(pinned("alpine:3.20@sha256:0123abcd"));
        assert!(!pinned("alpine:3.20"));
        assert!(!pinned("alpine"));
    }

    #[test]
    fn a_container_keeps_at_most_ten_megabytes_of_output() {
        let config = log_config();
        assert_eq!(config.typ.as_deref(), Some("json-file"));
        let opts = config.config.unwrap();
        assert_eq!(opts["max-size"], "5m");
        assert_eq!(opts["max-file"], "2");
    }
}
