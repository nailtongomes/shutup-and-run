use shutup_and_run::bollard_rt::{spawn_exit_watcher, BollardRuntime};
use shutup_and_run::cert::fingerprint_file;
use shutup_and_run::config::Settings;
use shutup_and_run::launcher::{spawn_maintenance, Launcher};
use shutup_and_run::poll::spawn_poller;
use shutup_and_run::runtime::Runtime;
use shutup_and_run::store::Store;
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    // On Windows the config, the job history and the service are inside the
    // distribution, so these run there. `status` reads the host's copy.
    if cfg!(windows) {
        match args.first().map(String::as_str) {
            Some("config" | "bans" | "pause" | "resume") => {
                return shutup_and_run::wsl::forward(&args)
            }
            Some("doctor") => {
                let mut wsl = vec!["doctor".to_string()];
                wsl.extend_from_slice(rest);
                return shutup_and_run::wsl::cli(&wsl).await;
            }
            _ => {}
        }
    }
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            usage();
            ExitCode::SUCCESS
        }
        Some("bans") => bans_cli(rest),
        Some("config") => shutup_and_run::cli::config_cli(rest),
        Some("status") => shutup_and_run::cli::status_cli(rest),
        Some("doctor") => shutup_and_run::cli::doctor_cli(rest).await,
        Some("pause") => shutup_and_run::cli::pause_cli(true, rest),
        Some("resume") => shutup_and_run::cli::pause_cli(false, rest),
        Some("wsl") => shutup_and_run::wsl::cli(rest).await,
        None | Some("serve") => {
            let code = serve().await;
            if shutup_and_run::status::restart_wanted() {
                restart();
            }
            code
        }
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

shutup-and-run config show|get|set|unset|validate|apply|reset|keys
shutup-and-run status
shutup-and-run doctor [--report]
shutup-and-run pause | resume
    for an agent or a script: no questions, --json, stable exit codes.
    `shutup-and-run config --help` documents all of them.

shutup-and-run bans
    list IPs banned for authentication failures

shutup-and-run bans lift <ip>
    remove a ban and its failure count

shutup-and-run wsl <doctor|install|reset|serve|start|stop|logs|uninstall>
    Windows 11: run the service inside a WSL2 distribution of its own.
    `shutup-and-run wsl --help` lists the options."
    );
}

fn bans_cli(args: &[String]) -> ExitCode {
    let history_path = match Settings::history_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
    };
    if !history_path.exists() {
        eprintln!(
            "shutup-and-run: {} does not exist. Run as root, with the same SHUTUP_AND_RUN_CONFIG as the service.",
            history_path.display()
        );
        return ExitCode::from(1);
    }
    let store = match Store::open(&history_path) {
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

    // Before anything touches Docker or polls: a second start on the same
    // history file must not reconcile, poll, or take a slot.
    let _lock = match shutup_and_run::lock::acquire(&settings.history_path.with_extension("lock")) {
        Ok(lock) => lock,
        Err(err) => {
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
    };

    let docker = match shutup_and_run::bollard_rt::connect_docker() {
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
    // Where a person at the machine can pause the worker, a pause does not
    // outlive a restart: one forgotten before a reboot or a re-install would
    // leave the machine lending nothing. A server without the page keeps its
    // drain flag, as `POST /v1/drain` documents.
    if launcher.settings().status_page && launcher.draining().unwrap_or(false) {
        let _ = launcher.set_draining(false);
        tracing::info!("resumed: a pause does not outlive a restart where the local page is on");
    }
    if let Some(warning) = &launcher.settings().config_warning {
        tracing::warn!("{warning}");
    }
    launcher.reconcile().await;
    let pending = launcher.clone();
    let status_file = shutup_and_run::status::spawn(launcher.clone());
    spawn_exit_watcher(docker, launcher.clone());
    spawn_maintenance(launcher.clone());
    spawn_poller(launcher.clone());

    let app =
        shutup_and_run::api::router(launcher).into_make_service_with_connect_info::<SocketAddr>();
    if http_plain {
        let listener = match TcpListener::bind(listen).await {
            Ok(listener) => listener,
            Err(err) => {
                status_file
                    .finish(
                        &pending,
                        shutup_and_run::status::End::Failed(err.to_string()),
                    )
                    .await;
                eprintln!("shutup-and-run: could not listen on {listen}: {err}");
                return ExitCode::from(1);
            }
        };
        tracing::info!(%listen, max_slots = slots, "shutup-and-run is up (http)");
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            status_file
                .finish(
                    &pending,
                    shutup_and_run::status::End::Failed(err.to_string()),
                )
                .await;
            eprintln!("shutup-and-run: {err}");
            return ExitCode::from(1);
        }
        finish_starts(&pending).await;
        status_file
            .finish(&pending, shutup_and_run::status::End::Stopped)
            .await;
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
            status_file
                .finish(
                    &pending,
                    shutup_and_run::status::End::Failed(err.to_string()),
                )
                .await;
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
    // HTTP/1.1 only, with a deadline for the first request and for the headers
    // of every later one. axum-server leaves both off. A client that finished
    // the TLS handshake and then said nothing kept its descriptor for as long
    // as it liked, and about a thousand of those stopped the process from
    // reaching Docker. HTTP/2 is off because an idle HTTP/2 connection has no
    // such deadline. The handshake itself already times out after 10 seconds.
    let mut tls = (*config.get_inner()).clone();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls));
    let mut server = axum_server::bind_rustls(listen, config)
        .http1_only()
        .first_request_timeout(HEADER_READ_TIMEOUT);
    server
        .http_builder()
        .http1()
        .header_read_timeout(HEADER_READ_TIMEOUT);
    if let Err(err) = server.handle(handle).serve(app).await {
        status_file
            .finish(
                &pending,
                shutup_and_run::status::End::Failed(err.to_string()),
            )
            .await;
        eprintln!("shutup-and-run: {err}");
        return ExitCode::from(1);
    }
    finish_starts(&pending).await;
    status_file
        .finish(&pending, shutup_and_run::status::End::Stopped)
        .await;
    ExitCode::SUCCESS
}

/// A job answered with 202 may still be pulling its image. Give those up to
/// 30 seconds to become containers, which then outlive this process.
async fn finish_starts(launcher: &Launcher) {
    let wait = std::time::Duration::from_secs(30);
    if tokio::time::timeout(wait, launcher.settled())
        .await
        .is_err()
    {
        tracing::warn!("stopping with a job still pulling its image; it will be recorded as ended");
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
        // SIGHUP, or a save on the local page: stop the same way, then start
        // again on the config file as it is now.
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
        let hang_up = async {
            match hup.as_mut() {
                Some(hup) => {
                    hup.recv().await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
            _ = hang_up => shutup_and_run::status::request_restart(),
            _ = shutup_and_run::status::restart_requested() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
    if shutup_and_run::status::restart_wanted() {
        tracing::info!("restarting on the config file; running containers are left alone");
        return;
    }
    tracing::info!("stopping; running containers are left alone");
}

/// Become a new process of the same program, with the same arguments. The pid
/// is kept, so systemd and the Windows supervisor see nothing, and the lock
/// and the listener are closed by the exec.
fn restart() {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut args = std::env::args_os();
        // argv[0] as it was started. /proc/self/exe names a deleted file
        // after the binary was replaced by an upgrade.
        let program = args
            .next()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::current_exe().ok());
        if let Some(program) = program {
            let err = std::process::Command::new(program).args(args).exec();
            eprintln!("shutup-and-run: could not restart: {err}");
            std::process::exit(1);
        }
    }
}
