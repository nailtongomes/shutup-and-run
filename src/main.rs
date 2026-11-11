use fairlead::bollard_rt::{spawn_exit_watcher, BollardRuntime};
use fairlead::config::Settings;
use fairlead::launcher::{spawn_maintenance, Launcher};
use fairlead::runtime::Runtime;
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
            eprintln!("fairlead: {err}");
            return ExitCode::from(1);
        }
    };
    if settings.listen.ip().is_unspecified() {
        tracing::warn!("listening on every interface; bind a private or Tailscale address");
    }

    let docker = match connect_docker() {
        Ok(docker) => docker,
        Err(err) => {
            eprintln!("fairlead: {err}");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = docker.ping().await {
        eprintln!("fairlead: docker did not answer: {err}");
        return ExitCode::from(1);
    }

    let runtime: Arc<dyn Runtime> = Arc::new(BollardRuntime::new(
        docker.clone(),
        settings.docker_config.clone(),
    ));
    match runtime.cleanup_created().await {
        Ok(0) => {}
        Ok(removed) => tracing::warn!(removed, "removed our containers left in created"),
        Err(err) => tracing::warn!(error = %err, "cleanup of created containers"),
    }

    let launcher = match Launcher::new(settings, runtime) {
        Ok(launcher) => launcher,
        Err(err) => {
            eprintln!("fairlead: job history: {err}");
            return ExitCode::from(1);
        }
    };
    launcher.reconcile().await;
    let listen = launcher.settings().listen;
    let slots = launcher.settings().max_slots;
    spawn_exit_watcher(docker, launcher.clone());
    spawn_maintenance(launcher.clone());

    let app = fairlead::api::router(launcher);
    let listener = match TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("fairlead: could not listen on {listen}: {err}");
            return ExitCode::from(1);
        }
    };
    tracing::info!(%listen, max_slots = slots, "fairlead is up");
    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("fairlead: {err}");
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
            "DOCKER_HOST={host} is not a local socket. fairlead only talks to a unix socket."
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
                    tracing::warn!(error = %err, "could not install the SIGTERM handler");
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
    tracing::info!("stopping; running containers are left alone");
}
