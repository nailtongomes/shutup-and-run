//! The operator's side of internet sharing (`src/internet.rs`): a PC dials
//! in and leaves idle connections here; a container asks this HTTP proxy for
//! `host:port` and gets one of them, so its traffic leaves from the PC.
//!
//! `shutup-and-run relay` runs on a server the PCs can reach, usually in a
//! container beside the operator's own (examples/relay). Two ports:
//!
//! - agents (7000): where PCs dial in. Any PC may; an id already connected
//!   with another password is refused.
//! - proxy (8080, on loopback unless told otherwise): HTTP proxy, `CONNECT`
//!   and plain `http://` requests. The PC is chosen by the proxy login:
//!   user = the PC's id, password = its secret. Chrome takes no login on the
//!   command line, so `--open PORT=ID:SECRET` adds a port with no login that
//!   always goes out through that PC. Keep both on a network only the
//!   operator's containers reach.
//!
//! It keeps nothing on disk. Alpha: plain TCP, see docs/INTERNET.md.

use crate::internet::{
    read_line, read_until_capped, take_line, valid_target, valid_word, Counted, HELLO,
    REPLY_TIMEOUT,
};
use base64::Engine;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Notify};

/// How long a container's request waits for a PC's idle connection.
const CLAIM_WAIT: Duration = Duration::from_secs(10);
/// An idle connection that says nothing for this long is gone.
const SILENT_MAX: Duration = Duration::from_secs(60);
/// Idle connections one PC may leave here.
const IDLE_MAX: usize = 64;
const HEAD_MAX: usize = 16 * 1024;

type Link = BufReader<TcpStream>;
type Claim = oneshot::Sender<oneshot::Sender<Link>>;

/// One PC: its password and its idle connections, each held by a task that
/// hands the connection over when asked.
struct Pool {
    secret: String,
    idle: Vec<Claim>,
    arrived: Arc<Notify>,
}

#[derive(Default)]
pub(crate) struct Relay {
    pools: Mutex<HashMap<String, Pool>>,
    proxy_port: u16,
}

#[derive(Debug, PartialEq, Eq)]
enum ClaimError {
    Login,
    NotConnected,
}

impl Relay {
    pub(crate) fn new(proxy_port: u16) -> Arc<Self> {
        Arc::new(Relay {
            pools: Mutex::new(HashMap::new()),
            proxy_port,
        })
    }

    /// Takes in an idle connection, or says why not. The first connection
    /// of an id, or the first after all of its last ones closed, sets its
    /// password.
    fn register(
        &self,
        id: &str,
        secret: &str,
    ) -> Result<(oneshot::Receiver<oneshot::Sender<Link>>, bool), &'static str> {
        let mut pools = self.pools.lock().unwrap();
        let pool = pools.entry(id.to_string()).or_insert_with(|| Pool {
            secret: secret.to_string(),
            idle: Vec::new(),
            arrived: Arc::new(Notify::new()),
        });
        pool.idle.retain(|claim| !claim.is_closed());
        let first = pool.idle.is_empty();
        if !same(&pool.secret, secret) {
            if !first {
                return Err("id in use by another PC");
            }
            pool.secret = secret.to_string();
        }
        if pool.idle.len() >= IDLE_MAX {
            return Err("too many connections");
        }
        let (tx, rx) = oneshot::channel();
        pool.idle.push(tx);
        pool.arrived.notify_one();
        Ok((rx, first))
    }

    /// An idle connection of the PC `id`, when `secret` is its password.
    async fn claim(&self, id: &str, secret: &str) -> Result<Link, ClaimError> {
        let deadline = tokio::time::Instant::now() + CLAIM_WAIT;
        loop {
            let (next, arrived) = {
                let mut pools = self.pools.lock().unwrap();
                let pool = pools
                    .get_mut(id)
                    .filter(|pool| same(&pool.secret, secret))
                    .ok_or(ClaimError::Login)?;
                (pool.idle.pop(), pool.arrived.clone())
            };
            match next {
                Some(claim) => {
                    let (tx, rx) = oneshot::channel();
                    if claim.send(tx).is_ok() {
                        if let Ok(link) = rx.await {
                            return Ok(link);
                        }
                    }
                }
                None => {
                    if tokio::time::timeout_at(deadline, arrived.notified())
                        .await
                        .is_err()
                    {
                        return Err(ClaimError::NotConnected);
                    }
                }
            }
        }
    }
}

fn same(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

// ---------------------------------------------------------------------------
// PCs

pub(crate) async fn accept_agents(relay: Arc<Relay>, listener: TcpListener) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        let relay = relay.clone();
        tokio::spawn(async move { agent(relay, stream, peer).await });
    }
}

async fn agent(relay: Arc<Relay>, stream: TcpStream, peer: SocketAddr) {
    let _ = stream.set_nodelay(true);
    let mut link = BufReader::new(stream);
    let Ok(Ok(hello)) = tokio::time::timeout(REPLY_TIMEOUT, read_line(&mut link)).await else {
        return;
    };
    let words: Vec<&str> = hello.split(' ').collect();
    let (id, secret) = match words.as_slice() {
        [word, id, secret]
            if *word == HELLO && valid_word(id, 1..=64) && valid_word(secret, 16..=128) =>
        {
            (id.to_string(), secret.to_string())
        }
        _ => {
            let _ = link.write_all(b"NO not a shutup-and-run PC\n").await;
            return;
        }
    };
    let claimed = match relay.register(&id, &secret) {
        Ok((claimed, first)) => {
            if first {
                println!("relay: PC {id} connected from {}", peer.ip());
            }
            claimed
        }
        Err(reason) => {
            println!("relay: refused PC {id} from {}: {reason}", peer.ip());
            let _ = link.write_all(format!("NO {reason}\n").as_bytes()).await;
            return;
        }
    };
    if link
        .write_all(format!("OK {}\n", relay.proxy_port).as_bytes())
        .await
        .is_err()
    {
        return;
    }
    hold_idle(link, claimed).await;
}

enum Event {
    Claimed(Result<oneshot::Sender<Link>, oneshot::error::RecvError>),
    Read(Result<Result<bool, String>, tokio::time::error::Elapsed>),
}

/// Answers the PC's pings until a container claims the connection.
async fn hold_idle(mut link: Link, mut claimed: oneshot::Receiver<oneshot::Sender<Link>>) {
    let mut buf = Vec::new();
    loop {
        let event = tokio::select! {
            claim = &mut claimed => Event::Claimed(claim),
            read = tokio::time::timeout(SILENT_MAX, read_until_capped(&mut link, &mut buf)) => Event::Read(read),
        };
        match event {
            Event::Claimed(Ok(reply)) => {
                let _ = reply.send(link);
                return;
            }
            Event::Claimed(Err(_)) => return,
            Event::Read(Ok(Ok(true))) => {
                if take_line(&mut buf) != "P" || link.write_all(b"P\n").await.is_err() {
                    return;
                }
            }
            Event::Read(_) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Containers

pub(crate) async fn accept_proxy(
    relay: Arc<Relay>,
    listener: TcpListener,
    login: Option<(String, String)>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let relay = relay.clone();
        let login = login.clone();
        tokio::spawn(async move { proxy(relay, stream, login).await });
    }
}

struct Request {
    connect: bool,
    target: String,
    /// The head to send on, for a plain `http://` request.
    head: Vec<u8>,
    login: Option<(String, String)>,
}

/// The request's head: `CONNECT host:port`, or `GET http://host/path` turned
/// into `GET /path` with the proxy's own headers left out.
fn parse_head(lines: &[String]) -> Result<Request, &'static str> {
    let first = lines.first().ok_or("empty request")?;
    let parts: Vec<&str> = first.split(' ').collect();
    let [method, uri, version] = parts.as_slice() else {
        return Err("bad request line");
    };
    let mut login = None;
    let mut kept = Vec::new();
    for line in &lines[1..] {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name_lower = name.trim().to_ascii_lowercase();
        match name_lower.as_str() {
            "proxy-authorization" => {
                login = value
                    .trim()
                    .strip_prefix("Basic ")
                    .and_then(|b64| {
                        base64::engine::general_purpose::STANDARD
                            .decode(b64.trim())
                            .ok()
                    })
                    .and_then(|raw| String::from_utf8(raw).ok())
                    .and_then(|text| {
                        text.split_once(':')
                            .map(|(user, pass)| (user.to_string(), pass.to_string()))
                    });
            }
            "proxy-connection" | "connection" | "keep-alive" => {}
            _ => kept.push(line.clone()),
        }
    }
    if *method == "CONNECT" {
        if !valid_target(uri) {
            return Err("CONNECT needs host:port");
        }
        return Ok(Request {
            connect: true,
            target: uri.to_string(),
            head: Vec::new(),
            login,
        });
    }
    let rest = uri
        .strip_prefix("http://")
        .ok_or("only CONNECT and http:// URLs")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let target = if authority
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    if !valid_target(&target) {
        return Err("bad host");
    }
    // One request per connection: a client must not reuse it for another site.
    let mut head = format!("{method} {path} {version}\r\n");
    for line in kept {
        head.push_str(&line);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    Ok(Request {
        connect: false,
        target,
        head: head.into_bytes(),
        login,
    })
}

async fn read_head(client: &mut Link) -> Result<Vec<String>, &'static str> {
    let mut lines = Vec::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        let n = (&mut *client)
            .take((HEAD_MAX - total) as u64)
            .read_line(&mut line)
            .await
            .map_err(|_| "read error")?;
        if n == 0 {
            return Err("closed or too long");
        }
        total += n;
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            return Ok(lines);
        }
        lines.push(line);
    }
}

async fn answer(client: &mut Link, status: &str, extra: &str, text: &str) {
    let body = format!("{text}\n");
    let _ = client
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\n{extra}Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
}

async fn proxy(relay: Arc<Relay>, stream: TcpStream, preset: Option<(String, String)>) {
    let _ = stream.set_nodelay(true);
    let mut client = BufReader::new(stream);
    let lines = match tokio::time::timeout(REPLY_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(lines)) => lines,
        _ => return,
    };
    let request = match parse_head(&lines) {
        Ok(request) => request,
        Err(why) => return answer(&mut client, "400 Bad Request", "", why).await,
    };
    let Some((id, secret)) = preset.or(request.login.clone()) else {
        return answer(
            &mut client,
            "407 Proxy Authentication Required",
            "Proxy-Authenticate: Basic realm=\"shutup-and-run\"\r\n",
            "user: the PC's id; password: its secret",
        )
        .await;
    };
    let mut link = match relay.claim(&id, &secret).await {
        Ok(link) => link,
        Err(ClaimError::Login) => {
            return answer(
                &mut client,
                "407 Proxy Authentication Required",
                "Proxy-Authenticate: Basic realm=\"shutup-and-run\"\r\n",
                "no PC with that id and password",
            )
            .await
        }
        Err(ClaimError::NotConnected) => {
            return answer(
                &mut client,
                "503 Service Unavailable",
                "",
                &format!("PC {id} is not connected now"),
            )
            .await
        }
    };
    if link
        .write_all(format!("C {}\n", request.target).as_bytes())
        .await
        .is_err()
    {
        return answer(
            &mut client,
            "502 Bad Gateway",
            "",
            "the PC's connection closed",
        )
        .await;
    }
    let reply = loop {
        match tokio::time::timeout(REPLY_TIMEOUT, read_line(&mut link)).await {
            Ok(Ok(line)) if line == "P" => continue,
            Ok(Ok(line)) => break line,
            _ => break "NO the PC did not answer".to_string(),
        }
    };
    if reply != "OK" {
        let why = reply.strip_prefix("NO ").unwrap_or(&reply);
        return answer(
            &mut client,
            "502 Bad Gateway",
            "",
            &format!("PC {id}: {why}"),
        )
        .await;
    }
    let sent = if request.connect {
        client
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
    } else {
        link.write_all(&request.head).await
    };
    if sent.is_err() {
        return;
    }
    let (sent, received) = (AtomicU64::new(0), AtomicU64::new(0));
    let mut link = Counted {
        inner: link,
        read: &received,
        written: &sent,
    };
    // A reset at the end is how many sites close; the bytes still count.
    let _ = tokio::io::copy_bidirectional(&mut client, &mut link).await;
    println!(
        "relay: {id} {} sent {} received {}",
        request.target,
        sent.load(Ordering::Relaxed),
        received.load(Ordering::Relaxed)
    );
}

// ---------------------------------------------------------------------------
// CLI

pub fn usage() -> &'static str {
    "\
shutup-and-run relay [--agents 0.0.0.0:7000] [--proxy 127.0.0.1:8080] [--open PORT=ID:SECRET,...]
    the operator's side of `internet on`: PCs dial in on --agents; containers
    use --proxy as an HTTP proxy with user = the PC's id, password = its
    secret. --open adds a port with no login that always goes out through
    one PC (for Chrome, which takes no proxy login), on the address of
    --proxy. The proxy listens on loopback unless told otherwise: put it on
    a network only your containers reach. docs/INTERNET.md."
}

/// `8101=pc-natal:SECRET`.
fn parse_open(text: &str) -> Result<(u16, String, String), String> {
    let (port, login) = text.split_once('=').ok_or("PORT=ID:SECRET")?;
    let (id, secret) = login.split_once(':').ok_or("PORT=ID:SECRET")?;
    let port: u16 = port.parse().map_err(|_| format!("{port} is not a port"))?;
    if !valid_word(id, 1..=64) || !valid_word(secret, 16..=128) {
        return Err(format!("{port}: not a PC's id and secret"));
    }
    Ok((port, id.to_string(), secret.to_string()))
}

pub async fn cli(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("shutup-and-run relay: {e}");
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let flags =
        crate::wsl::parse_flags(args, &["--agents", "--proxy", "--open"], &[]).map_err(|e| e.0)?;
    let agents_addr = flags.get("--agents").unwrap_or("0.0.0.0:7000");
    let proxy_addr: SocketAddr = flags
        .get("--proxy")
        .unwrap_or("127.0.0.1:8080")
        .parse()
        .map_err(|e| format!("--proxy: {e}"))?;
    let mut open = Vec::new();
    for item in flags
        .get("--open")
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
    {
        open.push(parse_open(item).map_err(|e| format!("--open: {e}"))?);
    }
    let agents = TcpListener::bind(agents_addr)
        .await
        .map_err(|e| format!("{agents_addr}: {e}"))?;
    let proxy = TcpListener::bind(proxy_addr)
        .await
        .map_err(|e| format!("{proxy_addr}: {e}"))?;
    let relay = Relay::new(proxy_addr.port());
    println!(
        "relay {}: PCs on {agents_addr}, proxy on {proxy_addr}",
        env!("CARGO_PKG_VERSION")
    );
    for (port, id, secret) in open {
        let addr = SocketAddr::new(proxy_addr.ip(), port);
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("{addr}: {e}"))?;
        println!("relay: {addr} goes out through PC {id}, with no login");
        tokio::spawn(accept_proxy(relay.clone(), listener, Some((id, secret))));
    }
    tokio::spawn(accept_agents(relay.clone(), agents));
    accept_proxy(relay, proxy, None).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internet::{spawn_links, Opts, Settings, Shared};
    use tokio::io::AsyncReadExt;
    use tokio::sync::watch;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn settings(relay: SocketAddr) -> Settings {
        Settings {
            enabled: true,
            relay: relay.to_string(),
            id: "pc-test".to_string(),
            secret: SECRET.to_string(),
            lend_hours: None,
            metered: false,
            consent_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn test_opts() -> Opts {
        Opts {
            private_ok: true,
            ping: Duration::from_millis(200),
            silent: Duration::from_millis(700),
        }
    }

    /// A site that answers every request with `hello` and closes.
    async fn site() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    let _ = s
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
                        .await;
                });
            }
        });
        addr
    }

    async fn relay_on(agents: TcpListener) -> SocketAddr {
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = proxy.local_addr().unwrap();
        let relay = Relay::new(addr.port());
        tokio::spawn(accept_agents(relay.clone(), agents));
        tokio::spawn(accept_proxy(relay, proxy, None));
        addr
    }

    async fn ask(proxy: SocketAddr, request: String) -> String {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_string(&mut out)).await;
        out
    }

    fn login(id: &str, secret: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"))
    }

    async fn wait_idle(shared: &Shared) {
        for _ in 0..100 {
            if shared.idle() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the PC never connected");
    }

    #[tokio::test]
    async fn a_container_reaches_a_site_through_the_pc_and_the_bytes_are_counted() {
        let site = site().await;
        let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agents_addr = agents.local_addr().unwrap();
        let proxy = relay_on(agents).await;
        let (_s_tx, s_rx) = watch::channel(settings(agents_addr));
        let (_h_tx, h_rx) = watch::channel(None);
        let shared = Arc::new(Shared::default());
        spawn_links(s_rx, h_rx, shared.clone(), test_opts());
        wait_idle(&shared).await;

        // HTTPS-style: CONNECT, then the bytes of the site.
        let auth = login("pc-test", SECRET);
        let mut s = TcpStream::connect(proxy).await.unwrap();
        s.write_all(format!("CONNECT {site} HTTP/1.1\r\nHost: {site}\r\nProxy-Authorization: Basic {auth}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("HTTP/1.1 200"), "{line}");
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        reader
            .get_mut()
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        reader.read_to_string(&mut out).await.unwrap();
        assert!(out.ends_with("hello"), "{out}");

        // Plain http://, rewritten for the site.
        let out = ask(proxy, format!("GET http://{site}/x HTTP/1.1\r\nHost: {site}\r\nProxy-Authorization: Basic {auth}\r\n\r\n")).await;
        assert!(out.ends_with("hello"), "{out}");

        let (received, sent) = shared.bytes();
        assert!(received >= 2 * 60 && sent > 40, "{received} {sent}");

        // No login, a wrong one, a PC that is not there.
        let out = ask(proxy, format!("CONNECT {site} HTTP/1.1\r\n\r\n")).await;
        assert!(out.starts_with("HTTP/1.1 407"), "{out}");
        let wrong = login("pc-test", "ffffffffffffffffffffffffffffffff");
        let out = ask(
            proxy,
            format!("CONNECT {site} HTTP/1.1\r\nProxy-Authorization: Basic {wrong}\r\n\r\n"),
        )
        .await;
        assert!(out.starts_with("HTTP/1.1 407"), "{out}");
    }

    #[tokio::test]
    async fn the_pc_refuses_its_own_network() {
        let site = site().await;
        let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agents_addr = agents.local_addr().unwrap();
        let proxy = relay_on(agents).await;
        let (_s_tx, s_rx) = watch::channel(settings(agents_addr));
        let (_h_tx, h_rx) = watch::channel(None);
        let shared = Arc::new(Shared::default());
        spawn_links(
            s_rx,
            h_rx,
            shared.clone(),
            Opts {
                private_ok: false,
                ..test_opts()
            },
        );
        wait_idle(&shared).await;
        let auth = login("pc-test", SECRET);
        let out = ask(
            proxy,
            format!("CONNECT {site} HTTP/1.1\r\nProxy-Authorization: Basic {auth}\r\n\r\n"),
        )
        .await;
        assert!(out.starts_with("HTTP/1.1 502"), "{out}");
        assert!(out.contains("own network"), "{out}");
    }

    #[tokio::test]
    async fn a_waiting_pc_closes_its_connections_and_comes_back() {
        let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agents_addr = agents.local_addr().unwrap();
        let _proxy = relay_on(agents).await;
        let (_s_tx, s_rx) = watch::channel(settings(agents_addr));
        let (h_tx, h_rx) = watch::channel(None);
        let shared = Arc::new(Shared::default());
        spawn_links(s_rx, h_rx, shared.clone(), test_opts());
        wait_idle(&shared).await;
        h_tx.send_replace(Some(crate::lending::Hold {
            code: "METERED_NETWORK".to_string(),
            detail: "metered".to_string(),
            resumes_at: None,
        }));
        for _ in 0..50 {
            if shared.idle() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(shared.idle(), 0);
        h_tx.send_replace(None);
        wait_idle(&shared).await;
    }

    /// A relay that greets and then says nothing: what a PC sees after it
    /// slept, or when its network changed under an open connection.
    #[tokio::test]
    async fn a_silent_relay_is_dropped_and_dialed_again() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hellos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = hellos.clone();
        tokio::spawn(async move {
            let mut kept = Vec::new();
            loop {
                let (s, _) = listener.accept().await.unwrap();
                let mut link = BufReader::new(s);
                if read_line(&mut link).await.is_ok() {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = link.write_all(b"OK 8080\n").await;
                }
                kept.push(link);
            }
        });
        let (_s_tx, s_rx) = watch::channel(settings(addr));
        let (_h_tx, h_rx) = watch::channel(None);
        let shared = Arc::new(Shared::default());
        spawn_links(s_rx, h_rx, shared.clone(), test_opts());
        tokio::time::sleep(Duration::from_secs(4)).await;
        let n = hellos.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            n > 4,
            "only {n} connections: the dead ones were not replaced"
        );
    }

    /// The relay goes away and comes back on the same port: the PC is
    /// connected again with nothing done on it. Unix only: Windows may keep
    /// the port a while after the first relay closed its connections.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_pc_connects_again_when_the_relay_comes_back() {
        let site = site().await;
        let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agents_addr = agents.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        // The first relay on a runtime of its own, so dropping it closes
        // every socket it had.
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let agents_std = agents.into_std().unwrap();
        let proxy_std = proxy.into_std().unwrap();
        let first = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let relay = Relay::new(proxy_addr.port());
                tokio::spawn(accept_agents(
                    relay.clone(),
                    TcpListener::from_std(agents_std).unwrap(),
                ));
                tokio::spawn(accept_proxy(
                    relay,
                    TcpListener::from_std(proxy_std).unwrap(),
                    None,
                ));
                tokio::task::spawn_blocking(move || stopped.recv())
                    .await
                    .unwrap()
                    .ok();
            });
            drop(rt);
        });
        let (_s_tx, s_rx) = watch::channel(settings(agents_addr));
        let (_h_tx, h_rx) = watch::channel(None);
        let shared = Arc::new(Shared::default());
        spawn_links(s_rx, h_rx, shared.clone(), test_opts());
        wait_idle(&shared).await;
        stop.send(()).unwrap();
        first.join().unwrap();
        for _ in 0..100 {
            if shared.idle() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(shared.idle(), 0, "the PC did not see the relay go");

        let agents = TcpListener::bind(agents_addr).await.unwrap();
        let proxy = TcpListener::bind(proxy_addr).await.unwrap();
        let relay = Relay::new(proxy_addr.port());
        tokio::spawn(accept_agents(relay.clone(), agents));
        tokio::spawn(accept_proxy(relay, proxy, None));
        for _ in 0..300 {
            if shared.idle() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(shared.idle() > 0, "the PC did not connect again");
        let auth = login("pc-test", SECRET);
        let out = ask(proxy_addr, format!("GET http://{site}/ HTTP/1.1\r\nHost: {site}\r\nProxy-Authorization: Basic {auth}\r\n\r\n")).await;
        assert!(out.ends_with("hello"), "{out}");
    }

    #[tokio::test]
    async fn an_id_in_use_with_another_password_is_refused() {
        let relay = Relay::new(8080);
        let (_first, _) = relay.register("pc-a", SECRET).unwrap();
        assert!(relay
            .register("pc-a", "ffffffffffffffffffffffffffffffff")
            .is_err());
        assert!(relay.register("pc-a", SECRET).is_ok());
        assert_eq!(
            relay
                .claim("pc-a", "ffffffffffffffffffffffffffffffff")
                .await
                .err(),
            Some(ClaimError::Login)
        );
        assert_eq!(
            relay.claim("pc-b", SECRET).await.err(),
            Some(ClaimError::Login)
        );
    }

    #[test]
    fn a_plain_request_loses_the_proxy_headers_and_keeps_the_rest() {
        let lines: Vec<String> = [
            "GET http://example.com/a?b=1 HTTP/1.1",
            "Host: example.com",
            "Proxy-Authorization: Basic cGMtYTpzZWNyZXQ=",
            "Proxy-Connection: keep-alive",
            "Accept: */*",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let request = parse_head(&lines).unwrap();
        assert_eq!(request.target, "example.com:80");
        assert_eq!(
            request.login,
            Some(("pc-a".to_string(), "secret".to_string()))
        );
        let head = String::from_utf8(request.head).unwrap();
        assert!(head.starts_with("GET /a?b=1 HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\n"));
        assert!(!head.contains("Proxy-"));
        assert!(head.ends_with("Connection: close\r\n\r\n"));
        assert!(parse_head(&["CONNECT example.com HTTP/1.1".to_string()]).is_err());
        assert!(parse_head(&["GET ftp://x/ HTTP/1.1".to_string()]).is_err());
        assert_eq!(parse_open("8101=pc-a:0123456789abcdef").unwrap().0, 8101);
        assert!(parse_open("8101=pc-a").is_err());
    }
}
