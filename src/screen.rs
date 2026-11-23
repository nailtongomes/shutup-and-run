//! The screen of a running job, on the local page, view only: a reverse
//! proxy from `/local/screen/JOB_ID/...` to noVNC on port 6080 of that job's
//! container, HTTP and its WebSocket. Off unless `screen_view` is on.
//!
//! The destination is never chosen by whoever calls: the job id picks a job
//! of this machine's history that is running, and Docker says where its
//! container is (`Runtime::screen_address`). One port, one container, while
//! it runs. The routes sit behind the page's own gate (`status::local_only`):
//! a loopback peer naming a loopback host. A WebSocket also needs the page's
//! own `Origin`, since a browser lets any site open one to 127.0.0.1.

use crate::launcher::Launcher;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long a connection to the container may take.
const CONNECT: Duration = Duration::from_secs(3);
/// The page's link is shown when the container answers within this.
const PROBE: Duration = Duration::from_millis(300);
/// noVNC's files are a few hundred KiB; nothing bigger is passed on.
const MAX_FILE: usize = 8 * 1024 * 1024;
/// The upstream's answer to a WebSocket upgrade, headers only.
const MAX_HEAD: usize = 16 * 1024;

pub(crate) fn routes() -> Router<Arc<Launcher>> {
    Router::new()
        .route("/local/screen/{id}", get(open))
        .route("/local/screen/{id}/{*rest}", get(proxy))
}

/// Where the page's link goes: noVNC's own page, connected, view only,
/// scaled to the window. The WebSocket path is this proxy's. noVNC 1.5 and
/// later resolve `path` against the page (`/local/screen/ID/`), older ones
/// against the root; the dot segments land on the same place in both.
pub(crate) fn viewer(id: &str) -> String {
    format!(
        "/local/screen/{id}/vnc.html?path=../../../local/screen/{id}/websockify&autoconnect=1&view_only=1&resize=scale"
    )
}

async fn open(State(launcher): State<Arc<Launcher>>, Path(id): Path<String>) -> Response {
    match launcher.screen_address(&id).await {
        Some(_) => Redirect::to(&viewer(&id)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn proxy(
    State(launcher): State<Arc<Launcher>>,
    Path((id, _rest)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    let Some(address) = launcher.screen_address(&id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // The raw path, still percent-encoded: what follows the job's prefix
    // goes to the container as it came, and cannot hold a line break.
    let uri = request.uri();
    let path = uri
        .path()
        .strip_prefix(&format!("/local/screen/{id}"))
        .unwrap_or("/");
    let target = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    };
    if is_websocket(request.headers()) {
        if !crate::status::same_origin(request.headers()) {
            return StatusCode::FORBIDDEN.into_response();
        }
        return websocket(address, &target, request).await;
    }
    file(address, &target).await
}

fn is_websocket(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

/// One file of noVNC, fetched from the container.
async fn file(address: SocketAddr, target: &str) -> Response {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT)
        .timeout(Duration::from_secs(30))
        .build()
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let Ok(mut answer) = client.get(format!("http://{address}{target}")).send().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let mut body = Vec::new();
    loop {
        match answer.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= MAX_FILE => {
                body.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return StatusCode::BAD_GATEWAY.into_response(),
        }
    }
    let status = StatusCode::from_u16(answer.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (status, body).into_response();
    if let Some(kind) = answer.headers().get(reqwest::header::CONTENT_TYPE) {
        if let Ok(kind) = header::HeaderValue::from_bytes(kind.as_bytes()) {
            response.headers_mut().insert(header::CONTENT_TYPE, kind);
        }
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// The upgrade is asked of the container first. When it agrees, the
/// browser gets the same 101 and the two streams are joined, byte for byte,
/// until either side closes: the container stopping closes it.
async fn websocket(address: SocketAddr, target: &str, request: Request<Body>) -> Response {
    let Ok(Ok(mut upstream)) = tokio::time::timeout(CONNECT, TcpStream::connect(address)).await
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let mut head = format!(
        "GET {target} HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
    );
    for name in [
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
    ] {
        if let Some(value) = request.headers().get(name).and_then(|v| v.to_str().ok()) {
            head += &format!("{name}: {value}\r\n");
        }
    }
    head += "\r\n";
    if upstream.write_all(head.as_bytes()).await.is_err() {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let Ok(Some((answer, early))) =
        tokio::time::timeout(Duration::from_secs(10), read_head(&mut upstream)).await
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let mut lines = answer.split("\r\n");
    let switching = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .is_some_and(|code| code == "101");
    if !switching {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let mut response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::UPGRADE, "websocket")
        .header(header::CONNECTION, "Upgrade");
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name == "sec-websocket-accept" || name == "sec-websocket-protocol" {
                response = response.header(name, value.trim());
            }
        }
    }
    let Ok(response) = response.body(Body::empty()) else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    tokio::spawn(async move {
        let Ok(upgraded) = hyper::upgrade::on(request).await else {
            return;
        };
        let mut browser = hyper_util::rt::TokioIo::new(upgraded);
        // What the container sent right after its 101 (a VNC server speaks
        // first) belongs to the browser.
        if !early.is_empty() && browser.write_all(&early).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut browser, &mut upstream).await;
    });
    response
}

/// The answer's head, and the bytes read past it.
async fn read_head(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let early = buffer.split_off(end + 4);
            return Some((String::from_utf8_lossy(&buffer).into_owned(), early));
        }
        if buffer.len() > MAX_HEAD {
            return None;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// The jobs among `ids` whose screen the page can show now: `screen_view`
/// is on, the job runs, and its container answers on the port.
pub(crate) async fn available(launcher: &Launcher, ids: Vec<String>) -> HashSet<String> {
    if !launcher.settings().screen_view {
        return HashSet::new();
    }
    let probes = ids.into_iter().map(|id| async move {
        let address = launcher.screen_address(&id).await?;
        let answered = tokio::time::timeout(PROBE, TcpStream::connect(address)).await;
        matches!(answered, Ok(Ok(_))).then_some(id)
    });
    futures_util::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}
