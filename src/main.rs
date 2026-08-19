mod herdr;

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::any, routing::get, routing::post};
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

type ApiResult<T> = Result<T, (StatusCode, &'static str)>;

/// Detail goes to the operator's terminal, not to the client: the context
/// chain carries the herdr socket path.
fn failed(what: &'static str) -> impl FnOnce(anyhow::Error) -> (StatusCode, &'static str) {
    move |error| {
        eprintln!("{what}: {error:#}");
        (StatusCode::INTERNAL_SERVER_ERROR, what)
    }
}

async fn session() -> ApiResult<Json<herdr::Session>> {
    herdr::session()
        .await
        .map(Json)
        .map_err(failed("could not read the herdr session"))
}

#[derive(Deserialize)]
struct Prompt {
    text: String,
}

async fn prompt(Path(pane_id): Path<String>, Json(body): Json<Prompt>) -> ApiResult<StatusCode> {
    herdr::prompt(&pane_id, &body.text)
        .await
        .map_err(failed("could not send to the pane"))?
        .ok_or((StatusCode::NOT_FOUND, "no such pane"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Enough scrollback to make sense of what a pane is doing, little enough that
/// a phone on mobile data can poll it every few seconds.
const OUTPUT_LINES: u32 = 300;
/// A ceiling on what one request can pull back, so a deep buffer cannot be
/// turned into an unbounded download.
const MAX_OUTPUT_LINES: u32 = 20_000;

#[derive(Deserialize)]
struct Window {
    lines: Option<u32>,
    #[serde(default)]
    source: Source,
}

/// Named for what the caller wants, not for herdr's vocabulary: a pane whose
/// screen is redrawn in place has no useful history to follow.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Source {
    #[default]
    Scrollback,
    Screen,
}

impl Source {
    fn as_herdr(self) -> &'static str {
        match self {
            Source::Scrollback => "recent",
            Source::Screen => "visible",
        }
    }
}

/// Plain text, not JSON: the body is the pane's output and nothing else, so it
/// renders verbatim. Whether more scrollback exists rides in `x-truncated`,
/// which is what the phone's "earlier output" control keys off. A pane that has
/// closed answers 500 here; the session list is what notices it went.
async fn output(
    Path(pane_id): Path<String>,
    Query(window): Query<Window>,
) -> ApiResult<([(&'static str, String); 1], String)> {
    let lines = window
        .lines
        .unwrap_or(OUTPUT_LINES)
        .clamp(1, MAX_OUTPUT_LINES);
    let output = herdr::read(&pane_id, lines, window.source.as_herdr())
        .await
        .map_err(failed("could not read the pane"))?;
    Ok(([("x-truncated", output.truncated.to_string())], output.text))
}

// --- Host allowlist ---------------------------------------------------------
//
// Cloudflare Access guards the tunnel, not this socket. A browser tricked by
// DNS rebinding resolves an attacker hostname to 127.0.0.1 and is then treated
// as same-origin, so CORS does not apply — but it still sends that hostname as
// `Host`. Pinning the accepted values closes that path.

#[derive(Clone)]
struct Allowed(Arc<Vec<String>>);

/// Loopback is always accepted so `make run` keeps working; a rebinding page
/// sends the attacker's own hostname as `Host` and cannot forge 127.0.0.1, so
/// allowing it costs nothing. The address we bind is accepted for the same
/// reason: reaching it already required a packet arriving on that interface.
/// `ALLOWED_HOSTS` adds the tunnel's public hostname on top, comma-separated.
fn allowed_hosts(bind: &str, port: &str) -> Vec<String> {
    let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let own = format!("{}:{port}", bind.trim().to_ascii_lowercase());
    if !hosts.contains(&own) {
        hosts.push(own);
    }
    if let Ok(list) = std::env::var("ALLOWED_HOSTS") {
        hosts.extend(
            list.split(',')
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty()),
        );
    }
    hosts
}

fn host_allowed(host: &str, allowed: &[String]) -> bool {
    let host = host.trim().to_ascii_lowercase();
    !host.is_empty() && allowed.contains(&host)
}

async fn guard_host(State(allowed): State<Allowed>, request: Request, next: Next) -> Response {
    // HTTP/2 carries the name in :authority rather than in a Host header.
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().authority().map(|a| a.as_str().to_owned()))
        .unwrap_or_default();

    if host_allowed(&host, &allowed.0) {
        next.run(request).await
    } else {
        eprintln!("rejected Host {host:?}; allowed: {:?}", allowed.0);
        (StatusCode::FORBIDDEN, "host not allowed").into_response()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8787".into());
    // Default loopback. A Zero Trust private-network route needs an address the
    // WARP client can be routed to, so bind an alias on `lo` (see README) rather
    // than a LAN interface, which would also publish the server to the LAN.
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".into());
    if bind == "0.0.0.0" || bind == "::" {
        eprintln!(
            "WARNING: BIND_ADDR={bind} listens on every interface. Nothing in front \
             of this socket authenticates; anyone who can route to this host can \
             drive your panes."
        );
    }
    let allowed = Allowed(Arc::new(allowed_hosts(&bind, &port)));
    println!("accepting Host: {:?}", allowed.0);

    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/session", get(session))
        .route("/api/panes/{pane_id}/prompt", post(prompt))
        .route("/api/panes/{pane_id}/output", get(output))
        // Without this, an unknown /api path would fall through to the UI and
        // answer a fetch with 200 and a page of HTML.
        .route("/api/{*rest}", any(|| async { StatusCode::NOT_FOUND }))
        // The UI routes on the path (/t/<tab>/p/<pane>), so a deep link or a
        // reload asks for a file that does not exist; hand back the app.
        .fallback_service(ServeDir::new("web/dist").fallback(ServeFile::new("web/dist/index.html")))
        .layer(middleware::from_fn_with_state(allowed, guard_host));

    // The tunnel is the sole intended ingress; the bind address decides who else
    // can even open a socket.
    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_allows_only_loopback() {
        // SAFETY: single-threaded test process, no other thread reads the env.
        unsafe { std::env::remove_var("ALLOWED_HOSTS") };
        let allowed = allowed_hosts("127.0.0.1", "8787");
        assert!(host_allowed("127.0.0.1:8787", &allowed));
        assert!(host_allowed("localhost:8787", &allowed));
        // The rebinding case: an attacker name pointed at 127.0.0.1.
        assert!(!host_allowed("evil.example.com", &allowed));
        // A different port is a different origin.
        assert!(!host_allowed("127.0.0.1:9999", &allowed));
        // No Host header at all.
        assert!(!host_allowed("", &allowed));
    }

    #[test]
    fn the_bound_address_is_accepted_without_extra_config() {
        // SAFETY: single-threaded test process, no other thread reads the env.
        unsafe { std::env::remove_var("ALLOWED_HOSTS") };
        // The private-network case: an alias on `lo`, reachable only via the tunnel.
        let allowed = allowed_hosts("10.99.99.1", "8787");
        assert!(host_allowed("10.99.99.1:8787", &allowed));
        // Local work still reaches it, and the guard still holds.
        assert!(host_allowed("127.0.0.1:8787", &allowed));
        assert!(!host_allowed("evil.example.com", &allowed));
    }

    #[test]
    fn configured_hosts_join_loopback_and_match_case_insensitively() {
        let allowed = ["127.0.0.1:8787", "herdr.example.com"].map(String::from);
        assert!(host_allowed("Herdr.Example.COM", &allowed));
        assert!(host_allowed(" herdr.example.com ", &allowed));
        // A suffix is not a match, and local work still reaches the server.
        assert!(!host_allowed("herdr.example.com.evil.net", &allowed));
        assert!(host_allowed("127.0.0.1:8787", &allowed));
    }
}
