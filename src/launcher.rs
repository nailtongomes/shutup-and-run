use crate::allowlist::{image_allowed, validate_image_name};
use crate::clock::{Clock, SystemClock};
use crate::config::Settings;
use crate::runtime::{Inspected, Runtime, RuntimeError, StartSpec};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

const EXIT_CACHE: usize = 256;

#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub image: String,
    pub env: HashMap<String, String>,
    pub timeout_secs: Option<u64>,
    pub cpu: Option<f64>,
    pub memory_bytes: Option<i64>,
    pub shm_bytes: Option<i64>,
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub total: usize,
    pub used: usize,
    pub free: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Exited { exit_code: i64 },
    NotFound,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{0}")]
    BadRequest(String),
    #[error("imagem não permitida")]
    Forbidden,
    #[error("sem slot livre")]
    NoSlots { total: usize, used: usize },
    #[error("{0}")]
    Unavailable(String),
}

struct ExitLog {
    map: HashMap<String, i64>,
    order: VecDeque<String>,
}

pub struct Launcher {
    settings: Settings,
    runtime: Arc<dyn Runtime>,
    clock: Arc<dyn Clock>,
    /// shortcut: one process mutex around list+create. The count is still the
    /// Docker list, so a restart does not forget running jobs. Two launcher
    /// processes on the same daemon can exceed max_slots. Upgrade to a shared
    /// lock (flock or a locked volume) if you ever run more than one process.
    slots: Mutex<()>,
    /// shortcut: exit codes live in memory and vanish on restart. After that,
    /// an auto-removed container is `not_found`. Upgrade to a small log file
    /// if operators need the code across restarts.
    exits: Mutex<ExitLog>,
}

impl Launcher {
    pub fn new(settings: Settings, runtime: Arc<dyn Runtime>) -> Arc<Self> {
        Self::with_clock(settings, runtime, Arc::new(SystemClock))
    }

    pub fn with_clock(
        settings: Settings,
        runtime: Arc<dyn Runtime>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            runtime,
            clock,
            slots: Mutex::new(()),
            exits: Mutex::new(ExitLog {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn token_matches(&self, provided: &str) -> bool {
        self.settings.token.matches(provided)
    }

    pub async fn slots(&self) -> Result<Slots, LaunchError> {
        let used = self.runtime.running_count().await.map_err(unavailable)?;
        let total = self.settings.max_slots;
        Ok(Slots {
            total,
            used,
            free: total.saturating_sub(used),
        })
    }

    pub async fn launch(&self, req: LaunchRequest) -> Result<String, LaunchError> {
        validate_image_name(&req.image).map_err(LaunchError::BadRequest)?;
        if !image_allowed(&req.image, &self.settings.allowlist) {
            return Err(LaunchError::Forbidden);
        }
        let timeout = resolve_timeout(req.timeout_secs, &self.settings)?;
        let env = validate_env(req.env)?;
        let command = validate_command(req.command)?;
        let nano_cpus = req.cpu.map(nano_cpus).transpose()?;
        let spec = StartSpec {
            image: req.image.clone(),
            env,
            command,
            deadline_unix: self.clock.now_unix().saturating_add(timeout),
            nano_cpus,
            memory_bytes: req.memory_bytes,
            shm_bytes: req.shm_bytes,
        };

        self.runtime.pull(&spec.image).await.map_err(|e| {
            tracing::warn!(image = %spec.image, error = %e, "pull falhou");
            LaunchError::Unavailable("falha ao baixar a imagem".into())
        })?;

        let _guard = self.slots.lock().await;
        let used = self.runtime.running_count().await.map_err(unavailable)?;
        if used >= self.settings.max_slots {
            return Err(LaunchError::NoSlots {
                total: self.settings.max_slots,
                used,
            });
        }
        let id = self.runtime.start(&spec).await.map_err(|e| {
            tracing::warn!(image = %spec.image, error = %e, "start falhou");
            LaunchError::Unavailable("falha ao iniciar o container".into())
        })?;
        tracing::info!(%id, image = %spec.image, timeout_secs = timeout, "job aceito");
        Ok(id)
    }

    pub async fn job(&self, id: &str) -> Result<JobState, LaunchError> {
        if !valid_container_id(id) {
            return Ok(JobState::NotFound);
        }
        match self.runtime.inspect(id).await.map_err(unavailable)? {
            Inspected::Running => Ok(JobState::Running),
            Inspected::Exited { exit_code } => {
                self.record_exit(id, exit_code).await;
                Ok(JobState::Exited { exit_code })
            }
            Inspected::NotFound => {
                if let Some(exit_code) = self.exit_of(id).await {
                    Ok(JobState::Exited { exit_code })
                } else {
                    Ok(JobState::NotFound)
                }
            }
        }
    }

    pub async fn record_exit(&self, id: &str, code: i64) {
        if !valid_container_id(id) {
            return;
        }
        let mut log = self.exits.lock().await;
        if !log.map.contains_key(id) {
            log.order.push_back(id.to_string());
            while log.order.len() > EXIT_CACHE {
                if let Some(old) = log.order.pop_front() {
                    log.map.remove(&old);
                }
            }
        }
        // A later `die` event overwrites the 137 we guess at kill time.
        log.map.insert(id.to_string(), code);
    }

    async fn exit_of(&self, id: &str) -> Option<i64> {
        self.exits.lock().await.map.get(id).copied()
    }

    /// Kill containers whose deadline has passed. SIGKILL's conventional code is 137;
    /// a later Docker `die` event may overwrite it with the real code.
    pub async fn reap_once(&self) -> Result<Vec<String>, LaunchError> {
        let now = self.clock.now_unix();
        let ids = self.runtime.list_expired(now).await.map_err(unavailable)?;
        let mut killed = Vec::new();
        for id in ids {
            match self.runtime.kill(&id).await {
                Ok(()) => {
                    self.record_exit(&id, 137).await;
                    tracing::info!(%id, "timeout: container morto");
                    killed.push(id);
                }
                Err(e) => {
                    tracing::warn!(%id, error = %e, "falha ao matar container vencido");
                }
            }
        }
        Ok(killed)
    }
}

pub fn spawn_reaper(launcher: Arc<Launcher>) {
    let interval = launcher.settings.reaper_interval;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = launcher.reap_once().await {
                tracing::warn!(error = %e, "reaper");
            }
        }
    });
}

fn unavailable(err: RuntimeError) -> LaunchError {
    tracing::warn!(error = %err, "docker");
    LaunchError::Unavailable("docker indisponível".into())
}

fn resolve_timeout(requested: Option<u64>, settings: &Settings) -> Result<u64, LaunchError> {
    let timeout = requested.unwrap_or(settings.default_timeout_secs);
    if timeout == 0 {
        return Err(LaunchError::BadRequest(
            "timeout_secs deve ser maior que zero".into(),
        ));
    }
    if timeout > settings.max_timeout_secs {
        return Err(LaunchError::BadRequest(format!(
            "timeout_secs {timeout} acima do máximo {}",
            settings.max_timeout_secs
        )));
    }
    Ok(timeout)
}

fn validate_env(env: HashMap<String, String>) -> Result<Vec<String>, LaunchError> {
    if env.len() > 128 {
        return Err(LaunchError::BadRequest(
            "env aceita no máximo 128 variáveis".into(),
        ));
    }
    let mut out = Vec::with_capacity(env.len());
    for (key, value) in env {
        let ok = !key.is_empty()
            && key.len() <= 256
            && key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(LaunchError::BadRequest(format!(
                "variável de ambiente inválida: {key}"
            )));
        }
        if value.len() > 16 * 1024 || value.contains('\0') {
            return Err(LaunchError::BadRequest(format!(
                "valor inválido para {key}"
            )));
        }
        out.push(format!("{key}={value}"));
    }
    Ok(out)
}

fn validate_command(command: Option<Vec<String>>) -> Result<Option<Vec<String>>, LaunchError> {
    let Some(command) = command else {
        return Ok(None);
    };
    if command.is_empty() {
        return Ok(None);
    }
    if command.len() > 32 {
        return Err(LaunchError::BadRequest(
            "command aceita no máximo 32 argumentos".into(),
        ));
    }
    for arg in &command {
        if arg.is_empty() || arg.len() > 4096 || arg.contains('\0') {
            return Err(LaunchError::BadRequest(
                "argumento de command inválido".into(),
            ));
        }
    }
    Ok(Some(command))
}

fn nano_cpus(cpu: f64) -> Result<i64, LaunchError> {
    if !cpu.is_finite() || cpu <= 0.0 || cpu > 128.0 {
        return Err(LaunchError::BadRequest(
            "cpu deve ser um número entre 0 e 128".into(),
        ));
    }
    let nano = (cpu * 1_000_000_000.0).round();
    if nano < 1.0 || nano > i64::MAX as f64 {
        return Err(LaunchError::BadRequest("cpu inválido".into()));
    }
    Ok(nano as i64)
}

pub fn valid_container_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::config::Settings;
    use crate::mock::MockRuntime;

    fn settings(slots: usize) -> Settings {
        Settings::from_toml_and_env(
            &format!(
                r#"
                token = "test-token-value-ok"
                max_slots = {slots}
                allowlist = ["alpine", "ghcr.io/nailtongomes/rpa-worker-selenium"]
                default_timeout_secs = 30
                max_timeout_secs = 120
                "#
            ),
            &HashMap::new(),
        )
        .unwrap()
    }

    fn harness(slots: usize) -> (Arc<Launcher>, Arc<MockRuntime>, Arc<FakeClock>) {
        let mock = Arc::new(MockRuntime::new());
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::with_clock(settings(slots), runtime, clock.clone());
        (launcher, mock, clock)
    }

    fn job(image: &str, timeout: Option<u64>) -> LaunchRequest {
        LaunchRequest {
            image: image.into(),
            env: HashMap::new(),
            timeout_secs: timeout,
            cpu: None,
            memory_bytes: None,
            shm_bytes: None,
            command: Some(vec!["sleep".into(), "30".into()]),
        }
    }

    #[tokio::test]
    async fn preseeded_containers_count_as_slots() {
        let (launcher, mock, _) = harness(2);
        mock.seed_running("aa11", 1_700_000_100).await;
        mock.seed_running("bb22", 1_700_000_100).await;
        let err = launcher.launch(job("alpine", None)).await.unwrap_err();
        assert!(matches!(err, LaunchError::NoSlots { used: 2, total: 2 }));
        assert_eq!(mock.start_count().await, 0);
    }

    #[tokio::test]
    async fn timeout_kills_only_expired_and_remembers_the_code() {
        let (launcher, mock, clock) = harness(1);
        let id = launcher.launch(job("alpine", Some(10))).await.unwrap();
        clock.set(1_700_000_009);
        assert!(launcher.reap_once().await.unwrap().is_empty());
        assert_eq!(launcher.job(&id).await.unwrap(), JobState::Running);

        clock.set(1_700_000_010);
        assert_eq!(launcher.reap_once().await.unwrap(), vec![id.clone()]);
        assert_eq!(mock.killed().await, vec![id.clone()]);
        assert!(mock.running_ids().await.is_empty());
        assert_eq!(
            launcher.job(&id).await.unwrap(),
            JobState::Exited { exit_code: 137 }
        );

        let again = launcher.launch(job("alpine", Some(10))).await.unwrap();
        assert_ne!(again, id);
    }

    #[tokio::test]
    async fn natural_exit_is_remembered_after_removal() {
        let (launcher, _, _) = harness(1);
        launcher.record_exit("abc123", 0).await;
        assert_eq!(
            launcher.job("abc123").await.unwrap(),
            JobState::Exited { exit_code: 0 }
        );
        assert_eq!(launcher.job("nope").await.unwrap(), JobState::NotFound);
    }
}
