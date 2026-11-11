use shutup_and_run::bollard_rt::{spawn_exit_watcher, BollardRuntime};
use shutup_and_run::cert::fingerprint_file;
use shutup_and_run::config::Settings;
use shutup_and_run::launcher::{spawn_maintenance, Launcher};
use shutup_and_run::runtime::Runtime;
use shutup_and_run::store::Store;
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            usage();
            ExitCode::SUCCESS
        }
        Some("bans") => bans_cli(&args[1..]),
        None | Some("serve") => serve().await,
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "\
shutup-and-run serve
    listen for jobs (the default when no subcommand is given)

shutup-and-run bans
    list IPs banned for authentication failures

shutup-and-run bans lift <ip>
    remove a ban and its failure count"
    );
}

fn bans_cli(args: &[String]) -> ExitCode {
    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
    };
    let store = match Store::open(&settings.history_path) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match args.first().map(String::as_str) {
        None | Some("list") => match store.list_bans(now) {
            Ok(bans) if bans.is_empty() => {
                println!("no bans");
                ExitCode::SUCCESS
            }
            Ok(bans) => {
                for ban in bans {
                    let left = ban.banned_until.saturating_sub(now);
                    println!("{}\tuntil {}\t{left}s left", ban.ip, ban.banned_until);
                }
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("shutup-and-run: {err}");
                ExitCode::from(1)
            }
        },
        Some("lift") => {
            let Some(ip) = args.get(1) else {
                usage();
                return ExitCode::from(2);
            };
            if ip.parse::<IpAddr>().is_err() {
                eprintln!("shutup-and-run: {ip} is not an IP address");
                return ExitCode::from(2);
            }
            match store.lift_ban(ip) {
                Ok(true) => {
                    println!("lifted {ip}");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    println!("no ban for {ip}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("shutup-and-run: {err}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}

async fn serve() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
    };
    if !settings.http_plain && (settings.tls_cert.is_none() || settings.tls_key.is_none()) {
        eprintln!("shutup-and-run: set tls_cert and tls_key, or set http = true for plaintext on localhost");
        return ExitCode::from(1);
    }
    if settings.http_plain && settings.listen.ip().is_unspecified() {
        tracing::warn!("plain HTTP on every interface; the token travels in clear text");
    }

    let docker = match connect_docker() {
        Ok(docker) => docker,
        Err(err) => {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = docker.ping().await {
        eprintln!("shutup-and-run: docker did not answer: {err}");
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

    let listen = settings.listen;
    let http_plain = settings.http_plain;
    let tls_cert = settings.tls_cert.clone();
    let tls_key = settings.tls_key.clone();
    let slots = settings.max_slots;
    let launcher = match Launcher::new(settings, runtime) {
        Ok(launcher) => launcher,
        Err(err) => {
            eprintln!("shutup-and-run: job history: {err}");
            return ExitCode::from(1);
        }
    };
    launcher.reconcile().await;
    spawn_exit_watcher(docker, launcher.clone());
    spawn_maintenance(launcher.clone());

    let app =
        shutup_and_run::api::router(launcher).into_make_service_with_connect_info::<SocketAddr>();
    if http_plain {
        let listener = match TcpListener::bind(listen).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("shutup-and-run: could not listen on {listen}: {err}");
                return ExitCode::from(1);
            }
        };
        tracing::info!(%listen, max_slots = slots, "shutup-and-run is up (http)");
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    let cert = tls_cert.unwrap();
    let key = tls_key.unwrap();
    match fingerprint_file(&cert) {
        Ok(fingerprint) => tracing::info!(%fingerprint, "tls certificate sha256"),
        Err(err) => tracing::warn!(error = %err, "could not fingerprint the certificate"),
    }
    let config = match axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key).await {
        Ok(config) => config,
        Err(err) => {
            eprintln!("shutup-and-run: tls: {err}");
            return ExitCode::from(1);
        }
    };
    let handle = axum_server::Handle::new();
    let shutdown = handle.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown.graceful_shutdown(None);
    });
    tracing::info!(%listen, max_slots = slots, "shutup-and-run is up (https)");
    if let Err(err) = axum_server::bind_rustls(listen, config)
        .handle(handle)
        .serve(app)
        .await
    {
        eprintln!("shutup-and-run: {err}");
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
            "DOCKER_HOST={host} is not a local socket. shutup-and-run only talks to a unix socket."
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
