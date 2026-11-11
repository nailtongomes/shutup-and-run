use n3_launcher::bollard_rt::{spawn_exit_watcher, BollardRuntime};
use n3_launcher::config::Settings;
use n3_launcher::launcher::{spawn_reaper, Launcher};
use n3_launcher::runtime::Runtime;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("n3-launcher: {err}");
            return ExitCode::from(1);
        }
    };
    if settings.listen.ip().is_unspecified() {
        tracing::warn!("listen em todas as interfaces; use um IP privado ou Tailscale");
    }

    let docker = match connect_docker() {
        Ok(docker) => docker,
        Err(err) => {
            eprintln!("n3-launcher: {err}");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = docker.ping().await {
        eprintln!("n3-launcher: docker não respondeu: {err}");
        return ExitCode::from(1);
    }

    let runtime: Arc<dyn Runtime> = Arc::new(BollardRuntime::new(
        docker.clone(),
        settings.docker_config.clone(),
    ));
    match runtime.cleanup_created().await {
        Ok(0) => {}
        Ok(removed) => tracing::warn!(removed, "removi containers nossos que ficaram em created"),
        Err(err) => tracing::warn!(error = %err, "limpeza de containers created"),
    }

    let launcher = Launcher::new(settings, runtime);
    let listen = launcher.settings().listen;
    let slots = launcher.settings().max_slots;
    spawn_exit_watcher(docker, launcher.clone());
    spawn_reaper(launcher.clone());

    let app = n3_launcher::api::router(launcher);
    let listener = match TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("n3-launcher: não consegui escutar em {listen}: {err}");
            return ExitCode::from(1);
        }
    };
    tracing::info!(%listen, max_slots = slots, "n3-launcher no ar");
    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("n3-launcher: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn connect_docker() -> Result<bollard::Docker, String> {
    match std::env::var("DOCKER_HOST") {
        Ok(host) if host.starts_with("unix://") => {
            let path = host.trim_start_matches("unix://");
            bollard::Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|err| err.to_string())
        }
        Ok(host) => Err(format!(
            "DOCKER_HOST={host} não é um socket local. Este serviço só fala com um socket unix."
        )),
        Err(_) => bollard::Docker::connect_with_socket_defaults().map_err(|err| err.to_string()),
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::warn!(error = %err, "não instalei o handler de SIGTERM");
                    let _ = ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
    tracing::info!("sinal de parada; containers em execução continuam");
}
