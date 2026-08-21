mod herdr;

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
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

/// The three-way answer for a bare-key route: who is listening decides.
fn key_gate(pane: Option<bool>) -> Result<(), (StatusCode, &'static str)> {
    match pane {
        None => Err((StatusCode::NOT_FOUND, "no such pane")),
        Some(false) => Err((StatusCode::FORBIDDEN, "not an agent pane")),
        Some(true) => Ok(()),
    }
}

/// The bare keys the phone needs: esc stops an agent's turn, enter answers the
/// question it is showing, up and down move the selection in it. All are
/// agent-only — a shell pane would treat the key as terminal input and execute
/// whatever sits on its command line, so the server refuses rather than
/// trusting the UI's gate.
async fn press(pane_id: &str, key: &str, what: &'static str) -> ApiResult<StatusCode> {
    key_gate(herdr::pane_is_agent(pane_id).await.map_err(failed(what))?)?;
    herdr::press(pane_id, key).await.map_err(failed(what))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn interrupt(Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    press(&pane_id, "esc", "could not interrupt the pane").await
}

async fn enter(Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    press(&pane_id, "enter", "could not send enter to the pane").await
}

async fn up(Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    press(&pane_id, "up", "could not send up to the pane").await
}

async fn down(Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    press(&pane_id, "down", "could not send down to the pane").await
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
/// `extra` (the ALLOWED_HOSTS env var, read by main) adds the tunnel's public
/// hostname on top, comma-separated.
fn allowed_hosts(bind: &str, port: &str, extra: Option<&str>) -> Vec<String> {
    let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let own = format!("{}:{port}", bind.trim().to_ascii_lowercase());
    if !hosts.contains(&own) {
        hosts.push(own);
    }
    if let Some(list) = extra {
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
        no_store((StatusCode::FORBIDDEN, "host not allowed").into_response()).await
    }
}

// --- Origin gate --------------------------------------------------------------
//
// The bodyless POSTs (interrupt, enter) are CORS "simple" requests: a malicious
// page anywhere can fire them without a preflight, and the browser sets Host to
// the target's own name, so the Host allowlist passes. The Origin header is the
// one thing such a page cannot forge; require it to match the same allowlist.

/// Origin is scheme://host[:port]. The scheme carries no identity here (the
/// tunnel terminates TLS), so match on host[:port] against the Host allowlist.
fn origin_allowed(origin: &str, allowed: &[String]) -> bool {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|host| host_allowed(host, allowed))
}

/// No Origin header (curl, native clients) passes: the gate is against
/// browser-ambient authority, not against someone who can already reach the
/// socket. An unreadable or unlisted Origin on a POST is refused.
async fn guard_origin(State(allowed): State<Allowed>, request: Request, next: Next) -> Response {
    let cross_site = request.method() == axum::http::Method::POST
        && match request.headers().get(header::ORIGIN) {
            None => false,
            Some(value) => !value
                .to_str()
                .is_ok_and(|origin| origin_allowed(origin, &allowed.0)),
        };
    if cross_site {
        no_store((StatusCode::FORBIDDEN, "cross-site request refused").into_response()).await
    } else {
        next.run(request).await
    }
}

/// Empty is unset: the Makefile `export`s BIND_ADDR/PORT/ALLOWED_HOSTS even
/// when .env leaves them blank, and a blank value must not change behavior.
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Refuse to listen where nothing authenticates: the bind must be one literal
/// IPv4 address, and a wildcard is fatal rather than a warning — the tunnel is
/// the only intended ingress, so there is no legitimate all-interfaces
/// deployment. IPv4 only because the whole path (host:port strings, the lo
/// alias, the nft rule) is IPv4-shaped.
fn parse_bind(bind: &str) -> anyhow::Result<std::net::Ipv4Addr> {
    let addr: std::net::Ipv4Addr = bind
        .parse()
        .map_err(|_| anyhow::anyhow!("BIND_ADDR must be a literal IPv4 address, got {bind:?}"))?;
    anyhow::ensure!(
        !addr.is_unspecified(),
        "BIND_ADDR={bind} would listen on every interface; nothing in front of \
         this socket authenticates. Bind 127.0.0.1 or the lo alias instead."
    );
    Ok(addr)
}

async fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Applied outermost so even a guard's 403 carries the headers. frame-ancestors
/// replaces X-Frame-Options; a page that cannot frame the UI cannot clickjack
/// its buttons.
async fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response
}

/// A path whose first non-empty segment is `api` belongs to the API even when
/// the nested router does not claim it, as with `/api/` and `//api`.
async fn keep_api_out_of_ui(request: Request, next: Next) -> Response {
    if request
        .uri()
        .path()
        .split('/')
        .find(|segment| !segment.is_empty())
        == Some("api")
    {
        no_store(StatusCode::NOT_FOUND.into_response()).await
    } else {
        next.run(request).await
    }
}

fn app(allowed: Allowed) -> Router {
    // no-store on /api only: pane output and session state are live, but the
    // hashed UI assets should stay cacheable for a phone on mobile data.
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/session", get(session))
        .route("/panes/{pane_id}/prompt", post(prompt))
        .route("/panes/{pane_id}/interrupt", post(interrupt))
        .route("/panes/{pane_id}/enter", post(enter))
        .route("/panes/{pane_id}/up", post(up))
        .route("/panes/{pane_id}/down", post(down))
        .route("/panes/{pane_id}/output", get(output))
        // Unknown paths claimed by the nest stay API responses.
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::map_response(no_store));

    // The UI routes on the path (/w/<workspace>/t/<tab>/p/<pane>), so a deep
    // link or a reload asks for a file that does not exist; hand back the app.
    let ui = Router::new()
        .fallback_service(ServeDir::new("web/dist").fallback(ServeFile::new("web/dist/index.html")))
        .layer(middleware::from_fn(keep_api_out_of_ui));

    Router::new()
        .nest("/api", api)
        .fallback_service(ui)
        .layer(middleware::from_fn_with_state(
            allowed.clone(),
            guard_origin,
        ))
        .layer(middleware::from_fn_with_state(allowed, guard_host))
        .layer(middleware::map_response(harden))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = env_nonempty("PORT").unwrap_or_else(|| "8787".into());
    // Default loopback. A Zero Trust private-network route needs an address the
    // WARP client can be routed to, so bind an alias on `lo` (see README) rather
    // than a LAN interface, which would also publish the server to the LAN.
    let bind = env_nonempty("BIND_ADDR").unwrap_or_else(|| "127.0.0.1".into());
    parse_bind(&bind)?;
    let extra = env_nonempty("ALLOWED_HOSTS");
    let allowed = Allowed(Arc::new(allowed_hosts(&bind, &port, extra.as_deref())));
    println!("accepting Host: {:?}", allowed.0);

    let app = app(allowed);

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
    fn bare_keys_reach_agents_only() {
        assert_eq!(key_gate(Some(true)), Ok(()));
        assert_eq!(
            key_gate(Some(false)),
            Err((StatusCode::FORBIDDEN, "not an agent pane"))
        );
        assert_eq!(key_gate(None), Err((StatusCode::NOT_FOUND, "no such pane")));
    }

    #[test]
    fn unset_allows_only_loopback() {
        let allowed = allowed_hosts("127.0.0.1", "8787", None);
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
        // The private-network case: an alias on `lo`, reachable only via the tunnel.
        let allowed = allowed_hosts("10.99.99.1", "8787", None);
        assert!(host_allowed("10.99.99.1:8787", &allowed));
        // Local work still reaches it, and the guard still holds.
        assert!(host_allowed("127.0.0.1:8787", &allowed));
        assert!(!host_allowed("evil.example.com", &allowed));
    }

    #[test]
    fn configured_hosts_join_loopback_and_match_case_insensitively() {
        let allowed = allowed_hosts("127.0.0.1", "8787", Some(" Herdr.Example.COM ,, "));
        assert!(host_allowed("herdr.example.com", &allowed));
        assert!(host_allowed("HERDR.EXAMPLE.COM", &allowed));
        // A suffix is not a match, and local work still reaches the server.
        assert!(!host_allowed("herdr.example.com.evil.net", &allowed));
        assert!(host_allowed("127.0.0.1:8787", &allowed));
    }

    #[test]
    fn wildcard_and_non_literal_binds_are_fatal() {
        assert!(parse_bind("0.0.0.0").is_err());
        assert!(parse_bind("::").is_err());
        assert!(parse_bind("0:0:0:0:0:0:0:0").is_err());
        // IPv4 only: everything downstream — the host:port string, the lo
        // alias, the nft rule — is IPv4-shaped, so accepting ::1 here would
        // just move the failure somewhere less legible.
        assert!(parse_bind("::1").is_err());
        assert!(parse_bind("example.com").is_err());
        assert!(parse_bind("").is_err());
        assert!(parse_bind("10.99.99.1").is_ok());
        assert!(parse_bind("127.0.0.1").is_ok());
    }

    #[test]
    fn empty_env_is_unset() {
        // The Makefile exports BIND_ADDR/PORT/ALLOWED_HOSTS even when .env
        // leaves them blank; blank must behave exactly like absent.
        unsafe { std::env::set_var("HERDR_REMOTE_TEST_EMPTY", " ") };
        assert_eq!(env_nonempty("HERDR_REMOTE_TEST_EMPTY"), None);
        unsafe { std::env::set_var("HERDR_REMOTE_TEST_EMPTY", "x") };
        assert_eq!(env_nonempty("HERDR_REMOTE_TEST_EMPTY"), Some("x".into()));
    }

    #[test]
    fn origins_match_the_host_allowlist() {
        let allowed = allowed_hosts("10.99.99.1", "8787", Some("herdr.example.com"));
        assert!(origin_allowed("http://10.99.99.1:8787", &allowed));
        assert!(origin_allowed("http://127.0.0.1:8787", &allowed));
        assert!(origin_allowed("https://herdr.example.com", &allowed));
        assert!(!origin_allowed("https://evil.example.com", &allowed));
        // A sandboxed iframe or data: page sends the literal string "null".
        assert!(!origin_allowed("null", &allowed));
        assert!(!origin_allowed("", &allowed));
        // Scheme is required — a bare host is not an Origin.
        assert!(!origin_allowed("10.99.99.1:8787", &allowed));
    }

    #[tokio::test]
    async fn responses_are_hardened() {
        use tower::ServiceExt;
        let allowed = Allowed(Arc::new(allowed_hosts("127.0.0.1", "8787", None)));
        let request = |method: &str, path: &str, origin: Option<&str>| {
            let mut builder = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "127.0.0.1:8787");
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            builder.body(axum::body::Body::empty()).unwrap()
        };

        // API responses must never be cached, and every response names its type.
        let response = app(allowed.clone())
            .oneshot(request("GET", "/api/health", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.headers()["content-security-policy"],
            "frame-ancestors 'none'"
        );

        // A cross-site bodyless POST is refused before it reaches herdr.
        let response = app(allowed.clone())
            .oneshot(request(
                "POST",
                "/api/panes/x/enter",
                Some("https://evil.example.com"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["cache-control"], "no-store");

        // The refusal itself still carries the hardening headers.
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");

        // A same-origin POST passes the gate: it reaches the handler, which
        // fails on the absent herdr socket — anything but 403 proves passage.
        let response = app(allowed.clone())
            .oneshot(request(
                "POST",
                "/api/panes/x/enter",
                Some("http://127.0.0.1:8787"),
            ))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::FORBIDDEN);

        // An unknown or bare /api path is an API 404, never the UI's HTML.
        for path in ["/api/nope", "/api", "/api/"] {
            let response = app(allowed.clone())
                .oneshot(request("GET", path, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }

        // A rejected Host is refused whatever the path.
        let mut bad_host = request("GET", "/api/health", None);
        bad_host
            .headers_mut()
            .insert(header::HOST, "evil.example.com".parse().unwrap());
        let response = app(allowed).oneshot(bad_host).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
}
