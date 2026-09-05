mod herdr;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use herdr_remote::transcript::{self, Transcript};
use herdr_remote::{live, markdown};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

type ApiResult<T> = Result<T, (StatusCode, &'static str)>;

#[derive(Clone, PartialEq, Eq)]
struct TranscriptIdentity {
    agent: String,
    session_kind: Option<String>,
    session_value: Option<String>,
    cwd: String,
    title: Option<String>,
}

impl TranscriptIdentity {
    fn pane(&self) -> transcript::PaneRef<'_> {
        transcript::PaneRef {
            agent: &self.agent,
            session_kind: self.session_kind.as_deref(),
            session_value: self.session_value.as_deref(),
            cwd: &self.cwd,
            title: self.title.as_deref(),
        }
    }
}

struct CacheEntry {
    identity: TranscriptIdentity,
    /// Newest request that confirmed this identity, including cache hits.
    ticket: u64,
    transcript: Arc<Mutex<Transcript>>,
}

#[derive(Default)]
struct TranscriptCache {
    entries: HashMap<String, CacheEntry>,
    /// Closes and resolution misses both invalidate older requests.
    last_close: HashMap<String, u64>,
}

type Cache = Arc<Mutex<TranscriptCache>>;

static TRANSCRIPT_TICKETS: AtomicU64 = AtomicU64::new(0);

fn next_transcript_ticket() -> u64 {
    TRANSCRIPT_TICKETS.fetch_add(1, Ordering::Relaxed)
}

/// The single pane this server has zoomed. A `tokio` mutex, because the
/// transition awaits herdr while holding it: two concurrent opens must not
/// trade the slot between each other's zoom calls.
#[derive(Clone, Default)]
struct Zoomed(Arc<tokio::sync::Mutex<Option<String>>>);

/// Axum carries one router state, so the two things handlers need travel
/// together rather than as two `with_state` calls that cannot both exist.
#[derive(Clone, Default)]
struct AppState {
    transcripts: Cache,
    zoomed: Zoomed,
}

#[derive(Serialize)]
struct Card {
    seq: u64,
    role: transcript::Role,
    preview: String,
    html: String,
    /// A local command's output, shown under the command it answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

#[derive(Serialize)]
struct TranscriptPage {
    messages: Vec<Card>,
    has_more: bool,
}

#[derive(Debug)]
struct TranscriptSnapshot {
    source: String,
    version: String,
    messages: Vec<transcript::Message>,
    has_more: bool,
}

#[derive(Deserialize)]
struct TranscriptWindow {
    before: Option<u64>,
    limit: Option<usize>,
}

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
            // `recent` hands back rows already broken at the pane's width, so a
            // narrow pane's output arrives pre-wrapped and unreadable on a
            // phone; the unwrapped source joins the soft wraps back up.
            Source::Scrollback => "recent_unwrapped",
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

/// The newest `limit` messages, or the `limit` messages whose `seq` is strictly
/// below `before`. `has_more` says whether anything older remains.
fn window(
    messages: &[transcript::Message],
    before: Option<u64>,
    limit: usize,
) -> (&[transcript::Message], bool) {
    let end = match before {
        Some(before) => messages.partition_point(|message| message.seq < before),
        None => messages.len(),
    };
    let start = end.saturating_sub(limit);
    (&messages[start..end], start > 0)
}

fn etag(version: &str, before: Option<u64>, limit: usize) -> String {
    let anchor = before.map_or_else(|| "tail".to_string(), |before| before.to_string());
    format!("\"{version}-{anchor}-{limit}\"")
}

fn card(message: &transcript::Message) -> Card {
    Card {
        seq: message.seq,
        role: message.role,
        preview: match message.role {
            transcript::Role::Assistant => markdown::preview(&message.text, 300),
            // What the person typed, markers and all: the parser reads `1.` as
            // a list and `#` as a heading and drops both, and the card would
            // lose characters its own writer put there.
            transcript::Role::User => markdown::plain(&message.text, 300),
        },
        // Both speakers write markdown, and the renderer escapes raw HTML and
        // gates link and image schemes, so neither half can script the page.
        // Only the person's newlines are line breaks: an agent wrapping its
        // prose means one paragraph, and a person pressing the key means two
        // lines.
        html: match message.role {
            transcript::Role::Assistant => markdown::to_html(&message.text),
            transcript::Role::User => markdown::to_html_hard_breaks(&message.text),
        },
        // A command's output is not markdown and not anyone's words: it goes
        // as text, and the phone gives it a node of its own.
        output: message.output.clone(),
    }
}

fn transcript_is_missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            || cause
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        rusqlite::Error::SqliteFailure(code, _)
                            if code.code == rusqlite::ErrorCode::CannotOpen
                    )
                })
    })
}

/// Parsing a cold transcript — forty-four megabytes at the extreme — reading
/// SQLite, and rendering markdown are all synchronous. Only a source that went
/// missing becomes `None`; other failures keep the cache and reach the client
/// as an error.
fn take_window(
    cache: Cache,
    key: String,
    identity: TranscriptIdentity,
    ticket: u64,
    home: std::path::PathBuf,
    before: Option<u64>,
    limit: usize,
) -> anyhow::Result<Option<TranscriptSnapshot>> {
    let cached = {
        let mut map = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("transcript cache lock is poisoned"))?;
        map.entries
            .get_mut(&key)
            .filter(|entry| entry.identity == identity)
            .map(|entry| {
                entry.ticket = entry.ticket.max(ticket);
                Arc::clone(&entry.transcript)
            })
    };
    let entry = match cached {
        Some(entry) => entry,
        None => {
            let resolved = transcript::resolve(&identity.pane(), &home)
                .map(Transcript::open)
                .map(Mutex::new)
                .map(Arc::new);
            let mut map = cache
                .lock()
                .map_err(|_| anyhow::anyhow!("transcript cache lock is poisoned"))?;
            if map
                .last_close
                .get(&key)
                .is_some_and(|last_close| *last_close > ticket)
            {
                return Ok(None);
            }
            match map.entries.get_mut(&key) {
                Some(cached) if cached.ticket > ticket || cached.identity == identity => {
                    cached.ticket = cached.ticket.max(ticket);
                    Arc::clone(&cached.transcript)
                }
                _ => {
                    let Some(entry) = resolved else {
                        map.entries.remove(&key);
                        record_close(&mut map, &key, ticket);
                        return Ok(None);
                    };
                    map.entries.insert(
                        key.clone(),
                        CacheEntry {
                            identity,
                            ticket,
                            transcript: Arc::clone(&entry),
                        },
                    );
                    entry
                }
            }
        }
    };
    let mut transcript = entry
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript cache entry is poisoned"))?;
    if let Err(error) = transcript.refresh() {
        if transcript_is_missing(&error) {
            drop(transcript);
            let mut map = cache
                .lock()
                .map_err(|_| anyhow::anyhow!("transcript cache lock is poisoned"))?;
            if map
                .entries
                .get(&key)
                .is_some_and(|cached| Arc::ptr_eq(&cached.transcript, &entry))
            {
                map.entries.remove(&key);
            }
            return Ok(None);
        }
        return Err(error);
    }
    let map = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("transcript cache lock is poisoned"))?;
    if map
        .last_close
        .get(&key)
        .is_some_and(|closed| *closed > ticket)
        || map
            .entries
            .get(&key)
            .is_none_or(|cached| !Arc::ptr_eq(&cached.transcript, &entry))
    {
        return Ok(None);
    }
    drop(map);
    let (messages, has_more) = window(transcript.messages(), before, limit);
    Ok(Some(TranscriptSnapshot {
        source: transcript.source_id(),
        version: transcript.version(),
        messages: messages.to_vec(),
        has_more,
    }))
}

/// A pane whose agent this server cannot read answers 404: the phone reads that
/// as "use the raw output view", not as a fault. An agent it can read but that
/// has written no transcript yet answers an empty page instead — a session that
/// has not started is empty, not absent.
async fn transcript_route(
    State(state): State<AppState>,
    Path(pane_id): Path<String>,
    Query(query): Query<TranscriptWindow>,
    headers: header::HeaderMap,
) -> ApiResult<Response> {
    let ticket = next_transcript_ticket();
    let context = herdr::pane_context(&pane_id)
        .await
        .map_err(failed("could not read the herdr session"))?
        .ok_or((StatusCode::NOT_FOUND, "no such pane"))?;
    let session = context.session.as_ref();
    let identity = TranscriptIdentity {
        agent: context.agent.unwrap_or_default(),
        session_kind: session.map(|session| session.kind.clone()),
        session_value: session.map(|session| session.value.clone()),
        cwd: context.cwd,
        title: context.title,
    };

    let agent = identity.agent.clone();
    let limit = query.limit.unwrap_or(30).clamp(1, 200);
    let before = query.before;
    let cache = state.transcripts.clone();
    let key = pane_id.clone();
    let home = transcript::home();
    let taken = tokio::task::spawn_blocking(move || {
        take_window(cache, key, identity, ticket, home, before, limit)
    })
    .await
    .map_err(|error| {
        eprintln!("transcript task failed: {error}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not read the transcript",
        )
    })?
    .map_err(failed("could not read the transcript"))?;
    let Some(snapshot) = taken else {
        if transcript::parsable(&agent) {
            return Ok(Json(TranscriptPage {
                messages: Vec::new(),
                has_more: false,
            })
            .into_response());
        }
        return Err((StatusCode::NOT_FOUND, "no transcript for this pane"));
    };

    // Both the source and window matter: two sessions can have equal byte
    // offsets, and a tail validator must not validate an earlier page.
    let etag = etag(
        &format!("{}:{}", snapshot.source, snapshot.version),
        before,
        limit,
    );
    let response_headers = [
        (header::ETAG, etag.clone()),
        (
            header::HeaderName::from_static("x-transcript-id"),
            snapshot.source,
        ),
    ];
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok((StatusCode::NOT_MODIFIED, response_headers).into_response());
    }

    let page = TranscriptPage {
        messages: snapshot.messages.iter().map(card).collect(),
        has_more: snapshot.has_more,
    };
    Ok((response_headers, Json(page)).into_response())
}

#[derive(Serialize)]
struct LiveView {
    screen: String,
    composer: String,
}

/// The whole visible screen, not a cropped region: a picker is drawn wherever
/// the agent likes, the pane's status stays `idle` while one is open, and a
/// zoomed screen measured 1,393 bytes. `MAX_OUTPUT_LINES` is the same ceiling
/// the raw-output route uses, and no measured screen approaches it.
async fn live_route(Path(pane_id): Path<String>) -> ApiResult<Json<LiveView>> {
    let context = herdr::pane_context(&pane_id)
        .await
        .map_err(failed("could not read the herdr session"))?
        .ok_or((StatusCode::NOT_FOUND, "no such pane"))?;
    let output = herdr::read(&pane_id, MAX_OUTPUT_LINES, "visible")
        .await
        .map_err(failed("could not read the pane"))?;
    let composer = context
        .agent
        .as_deref()
        .and_then(|agent| live::composer(agent, &output.text))
        .unwrap_or_default();
    Ok(Json(LiveView {
        screen: output.text,
        composer,
    }))
}

/// The pane that must be released before `pane_id` can take the slot.
fn superseded(slot: &Option<String>, pane_id: &str) -> Option<String> {
    slot.clone().filter(|held| held != pane_id)
}

/// What herdr already has zoomed decides the whole of `open`: a pane that is
/// zoomed needs nothing done to it, and one this server did not zoom itself is
/// the operator's own layout, which it neither repeats nor takes away.
async fn open(State(state): State<AppState>, Path(pane_id): Path<String>) -> ApiResult<StatusCode> {
    // A snapshot this server could not read decides nothing, and it falls back
    // to acting blind, which is all it could do before it thought to ask.
    let zoomed = herdr::zoomed().await.ok();
    let already = |pane: &str| {
        zoomed
            .as_ref()
            .is_some_and(|held| held.iter().any(|zoomed| zoomed == pane))
    };
    let unzoomed = |pane: &str| zoomed.is_some() && !already(pane);
    let mut slot = state.zoomed.0.lock().await;
    if let Some(previous) = superseded(&slot, &pane_id) {
        // herdr keeps one zoom per tab, so the pane this server zoomed may have
        // lost the zoom to a neighbour already; asking for it down then would
        // take that neighbour's zoom with it. A failure is worth reporting and
        // not worth refusing the pane the phone actually asked for.
        if !unzoomed(&previous)
            && let Err(error) = herdr::zoom(&previous, false).await
        {
            eprintln!("could not unzoom {previous}: {error:#}");
        }
        *slot = None;
    }
    if already(&pane_id) {
        return Ok(StatusCode::NO_CONTENT);
    }
    herdr::zoom(&pane_id, true)
        .await
        .map_err(failed("could not zoom the pane"))?;
    *slot = Some(pane_id);
    Ok(StatusCode::NO_CONTENT)
}

/// Remember the newest close or resolution miss. Observations can take tickets in
/// one order and reach the cache lock in the other; overwriting would let the
/// older one lower the tombstone, and a request holding a ticket between them
/// would then pass the check and resurrect the entry it was meant to drop.
fn record_close(cache: &mut TranscriptCache, pane_id: &str, ticket: u64) {
    let last = cache
        .last_close
        .entry(pane_id.to_string())
        .or_insert(ticket);
    *last = (*last).max(ticket);
}

async fn close(
    State(state): State<AppState>,
    Path(pane_id): Path<String>,
) -> ApiResult<StatusCode> {
    let ticket = next_transcript_ticket();
    {
        let mut cache = state.transcripts.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not release the transcript",
            )
        })?;
        cache.entries.remove(&pane_id);
        record_close(&mut cache, &pane_id, ticket);
    }
    let mut slot = state.zoomed.0.lock().await;
    if slot.as_deref() != Some(pane_id.as_str()) {
        return Ok(StatusCode::NO_CONTENT);
    }
    // The zoom this server took can be gone already — the operator unzoomed it,
    // or zoomed a neighbour, which in herdr is the one zoom moving. Asking for
    // it back down would be taking away a zoom that is no longer ours. A
    // snapshot that cannot be read says nothing, and the blind release stands.
    if let Ok(zoomed) = herdr::zoomed().await
        && !zoomed.iter().any(|held| held == &pane_id)
    {
        *slot = None;
        return Ok(StatusCode::NO_CONTENT);
    }
    herdr::zoom(&pane_id, false)
        .await
        .map_err(failed("could not unzoom the pane"))?;
    // Only after the pane is actually back: a slot cleared on a failed unzoom
    // would leave a zoomed pane that no later `open` knows to release.
    *slot = None;
    Ok(StatusCode::NO_CONTENT)
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
        .route("/panes/{pane_id}/transcript", get(transcript_route))
        .route("/panes/{pane_id}/live", get(live_route))
        .route("/panes/{pane_id}/open", post(open))
        .route("/panes/{pane_id}/close", post(close))
        // Unknown paths claimed by the nest stay API responses.
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::map_response(no_store))
        .with_state(AppState::default());

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
    use std::io::Write as _;

    // The fake Herdr socket is process-wide; its route tests share this lock.
    static HERDR_SOCKET_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn scrollback_asks_herdr_to_unwrap() {
        assert_eq!(Source::Scrollback.as_herdr(), "recent_unwrapped");
        assert_eq!(Source::Screen.as_herdr(), "visible");
    }

    fn messages(seqs: &[u64]) -> Vec<herdr_remote::transcript::Message> {
        seqs.iter()
            .map(|seq| herdr_remote::transcript::Message {
                seq: *seq,
                role: herdr_remote::transcript::Role::User,
                text: format!("m{seq}"),
                output: None,
            })
            .collect()
    }

    fn line_source(path: std::path::PathBuf) -> transcript::Source {
        transcript::Source::Lines {
            path,
            format: transcript::Format::Claude,
        }
    }

    fn transcript_identity() -> TranscriptIdentity {
        TranscriptIdentity {
            agent: "claude".into(),
            session_kind: None,
            session_value: None,
            cwd: "/repo".into(),
            title: None,
        }
    }

    fn insert_cached(
        cache: &Cache,
        key: &str,
        identity: &TranscriptIdentity,
        source: transcript::Source,
    ) -> Arc<Mutex<Transcript>> {
        let transcript = Arc::new(Mutex::new(Transcript::open(source)));
        cache.lock().unwrap().entries.insert(
            key.into(),
            CacheEntry {
                identity: identity.clone(),
                ticket: next_transcript_ticket(),
                transcript: Arc::clone(&transcript),
            },
        );
        transcript
    }

    fn is_cached(cache: &Cache, key: &str) -> bool {
        cache.lock().unwrap().entries.contains_key(key)
    }

    fn cached_identity(cache: &Cache, key: &str) -> Option<TranscriptIdentity> {
        cache
            .lock()
            .unwrap()
            .entries
            .get(key)
            .map(|entry| entry.identity.clone())
    }

    fn scratch_home(label: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!(
            "herdr-remote-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn write_claude(home: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
        let path = home.join(".claude/projects/-repo").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({ "type": "user", "message": { "content": text } })
            ),
        )
        .unwrap();
        path
    }

    fn cursor_identity() -> TranscriptIdentity {
        TranscriptIdentity {
            agent: "cursor".into(),
            session_kind: Some("id".into()),
            session_value: Some("reported".into()),
            ..transcript_identity()
        }
    }

    fn blocking_cursor_metadata(
        home: &std::path::Path,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let conversation = home.join(".cursor/chats/project/conversation");
        std::fs::create_dir_all(&conversation).unwrap();
        std::fs::write(
            conversation.join("meta.json"),
            r#"{"cwd":"/repo","hasConversation":true,"createdAtMs":1,"updatedAtMs":2}"#,
        )
        .unwrap();
        let connection = rusqlite::Connection::open(conversation.join("store.db")).unwrap();
        connection
            .execute_batch(
                "create table blobs (id text primary key, data blob);\
                 create table meta (key text primary key, value text);",
            )
            .unwrap();
        let root_id = "00".repeat(32);
        let message_id = "11".repeat(32);
        let mut root = vec![0x0a_u8, 32];
        root.extend([0x11; 32]);
        connection
            .execute(
                "insert into blobs values (?1, ?2)",
                rusqlite::params![root_id, root],
            )
            .unwrap();
        connection
            .execute(
                "insert into blobs values (?1, ?2)",
                rusqlite::params![
                    message_id,
                    br#"{"role":"assistant","content":"Cursor conversation"}"#.as_slice()
                ],
            )
            .unwrap();
        let meta = serde_json::json!({ "latestRootBlobId": root_id }).to_string();
        let encoded = meta
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        connection
            .execute("insert into meta values ('0', ?1)", [encoded])
            .unwrap();
        drop(connection);

        let metadata = home.join(".cursor/chats/project/reported/meta.json");
        std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&metadata)
                .status()
                .unwrap()
                .success()
        );

        let (opened_tx, opened_rx) = std::sync::mpsc::channel();
        let (write_tx, write_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut pipe = std::fs::OpenOptions::new()
                .write(true)
                .open(&metadata)
                .unwrap();
            // The first resolver now owns the FIFO inode. Later resolvers
            // read the regular file at this pathname without blocking.
            let meta = r#"{"createdAtMs":1}"#;
            std::fs::remove_file(&metadata).unwrap();
            std::fs::write(&metadata, meta).unwrap();
            opened_tx.send(()).unwrap();
            write_rx.recv().unwrap();
            writeln!(pipe, "{meta}").unwrap();
        });
        (opened_rx, write_tx, writer)
    }

    fn write_codex(home: &std::path::Path, id: &str, text: &str) -> std::path::PathBuf {
        let path = home
            .join(".codex/sessions/2026/09/01")
            .join(format!("rollout-2026-09-01T00-00-00-{id}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::json!({ "type": "session_meta", "payload": { "id": id, "cwd": "/repo" } }),
                serde_json::json!({ "type": "response_item", "payload": {
                    "type": "message", "role": "user",
                    "content": [{ "type": "input_text", "text": text }]
                } })
            ),
        )
        .unwrap();
        path
    }

    fn newest_text(
        cache: &Cache,
        identity: &TranscriptIdentity,
        home: &std::path::Path,
    ) -> Option<String> {
        take_window(
            cache.clone(),
            "pane".into(),
            identity.clone(),
            next_transcript_ticket(),
            home.to_path_buf(),
            None,
            30,
        )
        .unwrap()?
        .messages
        .first()
        .map(|message| message.text.clone())
    }

    #[test]
    fn a_non_missing_refresh_error_keeps_the_cached_transcript() {
        let path = std::env::temp_dir().join(format!(
            "herdr-remote-refresh-error-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        let cache = Cache::default();
        let identity = transcript_identity();
        insert_cached(&cache, "pane", &identity, line_source(path));

        let result = take_window(
            cache.clone(),
            "pane".into(),
            identity,
            next_transcript_ticket(),
            std::path::PathBuf::new(),
            None,
            30,
        );

        assert!(result.is_err());
        assert!(is_cached(&cache, "pane"));
    }

    #[test]
    fn a_missing_transcript_drops_its_cache_entry() {
        let path = std::env::temp_dir().join(format!(
            "herdr-remote-missing-transcript-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let cache = Cache::default();
        let identity = transcript_identity();
        insert_cached(&cache, "pane", &identity, line_source(path));

        let result = take_window(
            cache.clone(),
            "pane".into(),
            identity,
            next_transcript_ticket(),
            std::path::PathBuf::new(),
            None,
            30,
        );

        assert!(matches!(result, Ok(None)));
        assert!(!is_cached(&cache, "pane"));
    }

    #[test]
    fn a_deleted_cursor_store_drops_its_cache_entry() {
        let home = scratch_home("deleted-cursor-store");
        let path = home.join("store.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "create table blobs (id text primary key, data blob);\
                 create table meta (key text primary key, value text);",
            )
            .unwrap();
        let root = "0".repeat(64);
        connection
            .execute(
                "insert into blobs values (?1, ?2)",
                rusqlite::params![root, Vec::<u8>::new()],
            )
            .unwrap();
        let meta = serde_json::json!({ "latestRootBlobId": root }).to_string();
        let encoded = meta
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        connection
            .execute("insert into meta values ('0', ?1)", [encoded])
            .unwrap();
        drop(connection);

        let cache = Cache::default();
        let identity = transcript_identity();
        insert_cached(
            &cache,
            "pane",
            &identity,
            transcript::Source::CursorDb { path: path.clone() },
        );
        assert!(
            take_window(
                cache.clone(),
                "pane".into(),
                identity.clone(),
                next_transcript_ticket(),
                home.clone(),
                None,
                30,
            )
            .unwrap()
            .is_some()
        );

        std::fs::remove_file(path).unwrap();
        let result = take_window(
            cache.clone(),
            "pane".into(),
            identity,
            next_transcript_ticket(),
            home,
            None,
            30,
        );

        assert!(matches!(result, Ok(None)));
        assert!(!is_cached(&cache, "pane"));
    }

    #[test]
    fn a_busy_transcript_does_not_lock_the_whole_cache() {
        let path = std::env::temp_dir().join(format!(
            "herdr-remote-busy-transcript-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let source = line_source(path);
        let identity = transcript_identity();
        let cache = Cache::default();
        let transcript = insert_cached(&cache, "busy", &identity, source);
        let held = transcript.lock().unwrap();
        let worker_cache = cache.clone();
        let worker = std::thread::spawn(move || {
            take_window(
                worker_cache,
                "busy".into(),
                identity,
                next_transcript_ticket(),
                std::path::PathBuf::new(),
                None,
                30,
            )
        });

        while Arc::strong_count(&transcript) < 3 {
            std::thread::yield_now();
        }
        let (sent, received) = std::sync::mpsc::channel();
        let probe_cache = cache.clone();
        let probe = std::thread::spawn(move || {
            let _map = probe_cache.lock().unwrap();
            sent.send(()).unwrap();
        });
        assert!(
            received
                .recv_timeout(std::time::Duration::from_secs(1))
                .is_ok()
        );

        drop(held);
        assert!(matches!(worker.join().unwrap(), Ok(None)));
        probe.join().unwrap();
    }

    #[test]
    fn resolution_is_reused_until_the_pane_identity_changes() {
        let home = scratch_home("resolution-cache");
        write_claude(&home, "older.jsonl", "older");
        let cache = Cache::default();
        let identity = transcript_identity();

        assert_eq!(
            newest_text(&cache, &identity, &home).as_deref(),
            Some("older")
        );

        let newer = write_claude(&home, "newer.jsonl", "newer");
        std::fs::File::open(&newer)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            newest_text(&cache, &identity, &home).as_deref(),
            Some("older")
        );

        let changed = TranscriptIdentity {
            title: Some("new title".into()),
            ..identity
        };
        assert_eq!(
            newest_text(&cache, &changed, &home).as_deref(),
            Some("newer")
        );
    }

    #[test]
    fn a_transcript_created_after_a_resolution_miss_is_found() {
        let home = scratch_home("resolution-miss");
        let cache = Cache::default();
        let identity = transcript_identity();

        assert_eq!(newest_text(&cache, &identity, &home), None);

        write_claude(&home, "session.jsonl", "ready");
        assert_eq!(
            newest_text(&cache, &identity, &home).as_deref(),
            Some("ready")
        );
    }

    #[tokio::test]
    async fn close_releases_the_panes_cached_transcript() {
        let state = AppState::default();
        let identity = transcript_identity();
        insert_cached(
            &state.transcripts,
            "pane",
            &identity,
            line_source(std::path::PathBuf::from("unused.jsonl")),
        );

        assert_eq!(
            close(State(state.clone()), Path("pane".into())).await,
            Ok(StatusCode::NO_CONTENT)
        );
        assert!(!is_cached(&state.transcripts, "pane"));
    }

    #[tokio::test]
    async fn a_close_after_request_ticket_prevents_cache_reinsertion() {
        let home = scratch_home("close-after-ticket");
        write_claude(&home, "session.jsonl", "stale");
        let state = AppState::default();
        let identity = transcript_identity();
        let request_ticket = next_transcript_ticket();

        assert_eq!(
            close(State(state.clone()), Path("pane".into())).await,
            Ok(StatusCode::NO_CONTENT)
        );
        let result = take_window(
            state.transcripts.clone(),
            "pane".into(),
            identity,
            request_ticket,
            home,
            None,
            30,
        );

        assert!(matches!(result, Ok(None)));
        assert!(!is_cached(&state.transcripts, "pane"));
    }

    /// The tombstone must survive a close whose ticket is older than one already
    /// recorded — the ordering the lock does not guarantee.
    #[test]
    fn an_out_of_order_close_cannot_lower_the_tombstone() {
        let mut cache = TranscriptCache::default();
        record_close(&mut cache, "pane", 5);
        record_close(&mut cache, "pane", 3);
        assert_eq!(cache.last_close.get("pane"), Some(&5));
    }

    #[test]
    fn an_older_request_ticket_cannot_replace_a_newer_identity() {
        let home = scratch_home("older-request-ticket");
        write_claude(&home, "session.jsonl", "ready");
        let cache = Cache::default();
        let old_identity = transcript_identity();
        let old_ticket = next_transcript_ticket();
        let new_identity = TranscriptIdentity {
            title: Some("new identity".into()),
            ..transcript_identity()
        };
        let new_ticket = next_transcript_ticket();

        assert!(
            take_window(
                cache.clone(),
                "pane".into(),
                new_identity.clone(),
                new_ticket,
                home.clone(),
                None,
                30,
            )
            .unwrap()
            .is_some()
        );
        assert!(
            take_window(
                cache.clone(),
                "pane".into(),
                old_identity,
                old_ticket,
                home,
                None,
                30,
            )
            .unwrap()
            .is_some()
        );

        assert!(cached_identity(&cache, "pane").as_ref() == Some(&new_identity));
    }

    #[test]
    fn concurrent_cold_requests_for_one_pane_both_receive_a_transcript() {
        let home = scratch_home("concurrent-cold-requests");
        let cache = Cache::default();
        let identity = cursor_identity();
        let old_ticket = next_transcript_ticket();
        let old_cache = cache.clone();
        let old_identity = identity.clone();
        let old_home = home.clone();
        let (opened_rx, write_tx, writer) = blocking_cursor_metadata(&home);
        let old = std::thread::spawn(move || {
            take_window(
                old_cache,
                "pane".into(),
                old_identity,
                old_ticket,
                old_home,
                None,
                30,
            )
        });

        opened_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let newer = take_window(
            cache,
            "pane".into(),
            identity,
            next_transcript_ticket(),
            home,
            None,
            30,
        );
        write_tx.send(()).unwrap();
        writer.join().unwrap();
        let older = old.join().unwrap();

        for result in [older, newer] {
            let snapshot = result.unwrap().expect("both resolvers must succeed");
            assert_eq!(snapshot.messages[0].text, "Cursor conversation");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_during_resolution_does_not_reinsert_the_transcript() {
        let home = scratch_home("close-during-resolution");
        let state = AppState::default();
        let worker_cache = state.transcripts.clone();
        let identity = cursor_identity();
        let (opened_rx, write_tx, writer) = blocking_cursor_metadata(&home);
        let worker = std::thread::spawn(move || {
            take_window(
                worker_cache,
                "pane".into(),
                identity,
                next_transcript_ticket(),
                home,
                None,
                30,
            )
        });

        opened_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            close(State(state.clone()), Path("pane".into())).await,
            Ok(StatusCode::NO_CONTENT)
        );
        write_tx.send(()).unwrap();
        writer.join().unwrap();

        assert!(matches!(worker.join().unwrap(), Ok(None)));
        assert!(!is_cached(&state.transcripts, "pane"));
    }

    #[test]
    fn an_older_resolution_cannot_replace_a_newer_identity() {
        let home = scratch_home("superseded-resolution");
        let cache = Cache::default();
        let old_cache = cache.clone();
        let old_identity = cursor_identity();
        let old_home = home.clone();
        let (opened_rx, write_tx, writer) = blocking_cursor_metadata(&home);
        let old = std::thread::spawn(move || {
            take_window(
                old_cache,
                "pane".into(),
                old_identity,
                next_transcript_ticket(),
                old_home,
                None,
                30,
            )
        });

        opened_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        write_claude(&home, "new.jsonl", "new");
        let new_identity = transcript_identity();
        assert_eq!(
            newest_text(&cache, &new_identity, &home).as_deref(),
            Some("new")
        );
        write_tx.send(()).unwrap();
        writer.join().unwrap();

        let snapshot = old.join().unwrap().unwrap().unwrap();
        assert_eq!(snapshot.messages[0].text, "new");
        assert!(cached_identity(&cache, "pane").as_ref() == Some(&new_identity));
    }

    #[test]
    fn a_resolution_miss_invalidates_an_older_blocked_conversation() {
        let home = scratch_home("miss-during-resolution");
        let cache = Cache::default();
        let old_cache = cache.clone();
        let old_home = home.clone();
        let old_ticket = next_transcript_ticket();
        let (opened_rx, write_tx, writer) = blocking_cursor_metadata(&home);
        let old = std::thread::spawn(move || {
            take_window(
                old_cache,
                "pane".into(),
                cursor_identity(),
                old_ticket,
                old_home,
                None,
                30,
            )
        });
        opened_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();

        let miss_ticket = next_transcript_ticket();
        let unresolved = TranscriptIdentity {
            agent: "codex".into(),
            ..transcript_identity()
        };
        let newest = take_window(
            cache.clone(),
            "pane".into(),
            unresolved,
            miss_ticket,
            home,
            None,
            30,
        );
        write_tx.send(()).unwrap();
        writer.join().unwrap();
        let older = old.join().unwrap();

        assert!(
            newest.unwrap().is_none(),
            "the no-ID observation must be empty"
        );
        assert!(
            older.unwrap().is_none(),
            "older Cursor conversation returned after a no-ID Codex miss"
        );
        assert!(!is_cached(&cache, "pane"));
        assert_eq!(
            cache.lock().unwrap().last_close.get("pane"),
            Some(&miss_ticket)
        );
    }

    fn snapshot_at(
        cache: &Cache,
        identity: &TranscriptIdentity,
        ticket: u64,
        home: &std::path::Path,
    ) -> Option<TranscriptSnapshot> {
        take_window(
            cache.clone(),
            "pane".into(),
            identity.clone(),
            ticket,
            home.to_path_buf(),
            None,
            30,
        )
        .unwrap()
    }

    fn cached_codex_b(label: &str) -> (std::path::PathBuf, Cache, TranscriptIdentity) {
        let home = scratch_home(label);
        write_codex(&home, "b", "B");
        let identity = TranscriptIdentity {
            agent: "codex".into(),
            session_kind: Some("id".into()),
            session_value: Some("b".into()),
            ..transcript_identity()
        };
        let cache = Cache::default();
        assert_eq!(
            snapshot_at(&cache, &identity, 10, &home).unwrap().messages[0].text,
            "B"
        );
        (home, cache, identity)
    }

    fn assert_completed_cache_hit_survives(late_older_hit: bool) {
        let (home, cache, identity) = cached_codex_b("completed-hit-before-miss");
        let unresolved = TranscriptIdentity {
            session_kind: None,
            session_value: None,
            ..identity.clone()
        };
        // Ticket 20 observed no ID, but reaches resolution after ticket 30.
        let miss_ticket = 20;
        assert_eq!(
            snapshot_at(&cache, &identity, 30, &home).unwrap().messages[0].text,
            "B"
        );
        if late_older_hit {
            // An older identical observation cannot lower the accepted order.
            assert_eq!(
                snapshot_at(&cache, &identity, 15, &home).unwrap().messages[0].text,
                "B"
            );
        }

        snapshot_at(&cache, &unresolved, miss_ticket, &home);

        let map = cache.lock().unwrap();
        assert!(
            map.entries.contains_key("pane"),
            "ticket 20 evicted B confirmed by ticket 30; tombstone={:?}",
            map.last_close.get("pane")
        );
        assert_eq!(map.last_close.get("pane"), None);
        drop(map);
        assert_eq!(
            snapshot_at(&cache, &identity, 25, &home)
                .expect("a stale miss must leave newer reads valid")
                .messages[0]
                .text,
            "B"
        );
    }

    #[test]
    fn a_stale_miss_preserves_a_completed_cache_hit() {
        assert_completed_cache_hit_survives(false);
    }

    #[test]
    fn a_stale_miss_preserves_a_completed_hit_after_an_older_hit() {
        assert_completed_cache_hit_survives(true);
    }

    #[test]
    fn a_stale_miss_preserves_a_cache_hit_waiting_to_refresh() {
        let (home, cache, identity) = cached_codex_b("blocked-hit-before-miss");
        let unresolved = TranscriptIdentity {
            session_kind: None,
            session_value: None,
            ..identity.clone()
        };
        let entry = Arc::clone(&cache.lock().unwrap().entries["pane"].transcript);
        let held = entry.lock().unwrap();
        let worker_cache = cache.clone();
        let worker_home = home.clone();
        let worker_identity = identity.clone();
        let newer = std::thread::spawn(move || {
            snapshot_at(&worker_cache, &worker_identity, 30, &worker_home)
        });

        // The extra Arc proves ticket 30 reused B and is waiting on its lock.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while Arc::strong_count(&entry) < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "newer read did not enter"
            );
            std::thread::yield_now();
        }
        let miss_cache = cache.clone();
        let miss_home = home.clone();
        let miss =
            std::thread::spawn(move || snapshot_at(&miss_cache, &unresolved, 20, &miss_home));
        // A valid stale read may reuse B and wait too. A discarded read may
        // finish directly. Both schedules let us release the lock without sleep.
        while !miss.is_finished() && Arc::strong_count(&entry) < 4 {
            assert!(
                std::time::Instant::now() < deadline,
                "stale read did not enter"
            );
            std::thread::yield_now();
        }
        drop(held);
        let newer = newer.join().unwrap();
        miss.join().unwrap();

        assert_eq!(
            newer
                .expect("ticket 30 must return B even when ticket 20 resolves during refresh")
                .messages[0]
                .text,
            "B"
        );
        assert!(is_cached(&cache, "pane"));
        assert_eq!(cache.lock().unwrap().last_close.get("pane"), None);
        assert_eq!(
            snapshot_at(&cache, &identity, 25, &home).unwrap().messages[0].text,
            "B"
        );
    }

    fn assert_identical_resolution_survives(resolving_ticket: u64) {
        let home = scratch_home("identical-entry-after-resolution");
        let cache = Cache::default();
        let identity = cursor_identity();
        let worker_cache = cache.clone();
        let worker_home = home.clone();
        let worker_identity = identity.clone();
        let (opened_rx, write_tx, writer) = blocking_cursor_metadata(&home);
        let resolving = std::thread::spawn(move || {
            snapshot_at(
                &worker_cache,
                &worker_identity,
                resolving_ticket,
                &worker_home,
            )
        });
        opened_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let installed = snapshot_at(&cache, &identity, 10, &home);
        let confirmed = if resolving_ticket < 30 {
            snapshot_at(&cache, &identity, 30, &home)
        } else {
            None
        };
        write_tx.send(()).unwrap();
        writer.join().unwrap();
        let reused = resolving.join().unwrap();
        for snapshot in [installed, reused] {
            assert_eq!(snapshot.unwrap().messages[0].text, "Cursor conversation");
        }
        if resolving_ticket < 30 {
            assert_eq!(confirmed.unwrap().messages[0].text, "Cursor conversation");
        }
        // Resolution reused an identical entry installed while it waited.
        // It must both advance newer observations and preserve older maxima.
        let unresolved = TranscriptIdentity {
            agent: "codex".into(),
            ..transcript_identity()
        };
        snapshot_at(&cache, &unresolved, 20, &home);

        assert!(
            is_cached(&cache, "pane"),
            "ticket 20 evicted the conversation after ticket {resolving_ticket} reused it"
        );
        assert_eq!(cache.lock().unwrap().last_close.get("pane"), None);
        assert_eq!(
            snapshot_at(&cache, &identity, 25, &home).unwrap().messages[0].text,
            "Cursor conversation"
        );
    }

    #[test]
    fn a_stale_miss_preserves_identical_entries_reused_after_resolution() {
        assert_identical_resolution_survives(30);
    }

    #[test]
    fn a_stale_miss_preserves_identical_entries_after_an_older_resolution() {
        assert_identical_resolution_survives(15);
    }

    #[test]
    fn a_codex_pane_can_move_from_a_known_session_through_no_id_to_another() {
        let home = scratch_home("codex-session-transition");
        write_codex(&home, "a", "A");
        write_codex(&home, "b", "B");
        let cache = Cache::default();
        let identity = TranscriptIdentity {
            agent: "codex".into(),
            session_kind: Some("id".into()),
            session_value: Some("a".into()),
            ..transcript_identity()
        };
        assert_eq!(newest_text(&cache, &identity, &home).as_deref(), Some("A"));
        let unresolved = TranscriptIdentity {
            session_kind: None,
            session_value: None,
            ..identity.clone()
        };
        assert_eq!(newest_text(&cache, &unresolved, &home), None);
        assert!(!is_cached(&cache, "pane"));
        let next = TranscriptIdentity {
            session_value: Some("b".into()),
            ..identity
        };
        assert_eq!(newest_text(&cache, &next, &home).as_deref(), Some("B"));
    }

    #[test]
    fn a_snapshot_carries_source_revision_and_the_requested_window() {
        let home = scratch_home("transcript-snapshot");
        let path = write_claude(&home, "session.jsonl", "first");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({ "type": "user", "message": { "content": "second" } })
        )
        .unwrap();
        drop(file);
        let expected_source =
            Transcript::open(line_source(path.canonicalize().unwrap())).source_id();
        let cache = Cache::default();
        let identity = transcript_identity();
        let newest: TranscriptSnapshot = take_window(
            cache.clone(),
            "pane".into(),
            identity.clone(),
            next_transcript_ticket(),
            home.clone(),
            None,
            1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(newest.source, expected_source);
        assert!(!newest.version.is_empty());
        assert_eq!(
            newest.messages,
            vec![transcript::Message {
                seq: 1,
                role: transcript::Role::User,
                text: "second".into(),
                output: None,
            }]
        );
        assert!(newest.has_more);

        let older: TranscriptSnapshot = take_window(
            cache,
            "pane".into(),
            identity,
            next_transcript_ticket(),
            home,
            Some(1),
            30,
        )
        .unwrap()
        .unwrap();
        assert_eq!(older.source, newest.source);
        assert_eq!(older.version, newest.version);
        assert_eq!(
            older.messages,
            vec![transcript::Message {
                seq: 0,
                role: transcript::Role::User,
                text: "first".into(),
                output: None,
            }]
        );
        assert!(!older.has_more);
    }

    fn transcript_context(identity: &TranscriptIdentity) -> &'static [u8] {
        let session = identity
            .session_kind
            .as_ref()
            .zip(identity.session_value.as_ref())
            .map(|(kind, value)| {
                serde_json::json!({
                    "source": "test", "agent": identity.agent, "kind": kind, "value": value
                })
            });
        let reply = serde_json::json!({
            "id": "herdr-remote", "result": { "snapshot": {
                "workspaces": [], "tabs": [], "panes": [{
                    "pane_id": "pane", "tab_id": "tab", "agent": identity.agent,
                    "agent_status": "idle", "cwd": identity.cwd, "title": identity.title,
                    "agent_session": session
                }]
            } }
        });
        Box::leak(format!("{reply}\n").into_bytes().into_boxed_slice())
    }

    async fn transcript_response(state: &AppState, validator: Option<&str>) -> Response {
        let mut headers = header::HeaderMap::new();
        if let Some(validator) = validator {
            headers.insert(header::IF_NONE_MATCH, validator.parse().unwrap());
        }
        transcript_route(
            State(state.clone()),
            Path("pane".into()),
            Query(TranscriptWindow {
                before: None,
                limit: Some(30),
            }),
            headers,
        )
        .await
        .unwrap_or_else(IntoResponse::into_response)
    }

    async fn transcript_body(response: Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn transcript_headers_keep_source_stable_while_the_revision_changes() {
        let _socket_lock = HERDR_SOCKET_TEST.lock().await;
        let home = scratch_home("transcript-headers");
        let path = write_claude(&home, "session.jsonl", "A");
        let identity = transcript_identity();
        let state = AppState::default();
        let cached = insert_cached(
            &state.transcripts,
            "pane",
            &identity,
            line_source(path.clone()),
        );
        let expected_source = cached.lock().unwrap().source_id();
        let (socket, _requests) = herdr::fake_herdr(vec![transcript_context(&identity); 3]);
        let _socket = herdr::use_test_socket(socket);

        let first = transcript_response(&state, None).await;
        assert_eq!(first.status(), StatusCode::OK);
        let source = first
            .headers()
            .get("x-transcript-id")
            .expect("resolved response needs a source")
            .to_str()
            .unwrap()
            .to_owned();
        let version = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        assert!(!source.is_empty());
        assert_eq!(source, expected_source);
        assert!(
            !source.contains(home.to_str().unwrap()),
            "source must be opaque"
        );
        assert_eq!(transcript_body(first).await["messages"][0]["preview"], "A");
        assert_eq!(
            transcript_response(&state, Some(&version)).await.status(),
            StatusCode::NOT_MODIFIED
        );

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({ "type": "user", "message": { "content": "B" } })
        )
        .unwrap();
        drop(file);
        let appended = transcript_response(&state, Some(&version)).await;
        assert_eq!(appended.status(), StatusCode::OK);
        assert_eq!(appended.headers()["x-transcript-id"], source);
        assert_ne!(appended.headers()[header::ETAG], version);
        let body = transcript_body(appended).await;
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["preview"], "B");
    }

    #[tokio::test]
    async fn equal_length_sources_do_not_share_an_http_etag() {
        let _socket_lock = HERDR_SOCKET_TEST.lock().await;
        let home = scratch_home("equal-length-etags");
        let a = write_claude(&home, "a.jsonl", "A");
        let b = write_claude(&home, "b.jsonl", "B");
        assert_eq!(
            std::fs::metadata(&a).unwrap().len(),
            std::fs::metadata(&b).unwrap().len()
        );
        let identity = transcript_identity();
        let state = AppState::default();
        let (socket, _requests) = herdr::fake_herdr(vec![transcript_context(&identity); 2]);
        let _socket = herdr::use_test_socket(socket);

        insert_cached(&state.transcripts, "pane", &identity, line_source(a));
        let first = transcript_response(&state, None).await;
        assert_eq!(first.status(), StatusCode::OK);
        let validator = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        let first_source = first.headers().get("x-transcript-id").cloned();
        assert_eq!(transcript_body(first).await["messages"][0]["preview"], "A");

        insert_cached(&state.transcripts, "pane", &identity, line_source(b));
        let second = transcript_response(&state, Some(&validator)).await;
        assert_eq!(
            second.status(),
            StatusCode::OK,
            "another source must return its body even at the same revision"
        );
        assert_ne!(second.headers()[header::ETAG], validator);
        let second_source = second.headers().get("x-transcript-id");
        assert!(first_source.is_some() && second_source.is_some());
        assert_ne!(second_source, first_source.as_ref());
        assert_eq!(transcript_body(second).await["messages"][0]["preview"], "B");
    }

    #[tokio::test]
    async fn unresolved_supported_agents_have_empty_pages_without_source_or_etag() {
        let _socket_lock = HERDR_SOCKET_TEST.lock().await;
        let home = scratch_home("unresolved-response");
        for agent in ["claude", "codex", "grok", "cursor"] {
            let identity = TranscriptIdentity {
                agent: agent.into(),
                session_kind: Some("path".into()),
                session_value: Some(home.join("missing.jsonl").to_str().unwrap().into()),
                ..transcript_identity()
            };
            let (socket, _requests) = herdr::fake_herdr(vec![transcript_context(&identity)]);
            let _socket = herdr::use_test_socket(socket);
            let response = transcript_response(&AppState::default(), Some("\"stale\"")).await;
            assert_eq!(response.status(), StatusCode::OK, "{agent}");
            assert!(
                !response.headers().contains_key("x-transcript-id"),
                "{agent}"
            );
            assert!(!response.headers().contains_key(header::ETAG), "{agent}");
            assert_eq!(
                transcript_body(response).await,
                serde_json::json!({ "messages": [], "has_more": false })
            );
        }
    }

    #[test]
    fn no_cursor_returns_the_newest_window() {
        let all = messages(&[0, 1, 2, 3, 4]);
        let (page, has_more) = window(&all, None, 2);
        assert_eq!(page.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![3, 4]);
        assert!(has_more);
    }

    #[test]
    fn a_cursor_is_exclusive_and_reaches_further_back() {
        let all = messages(&[0, 1, 2, 3, 4]);
        let (page, has_more) = window(&all, Some(3), 2);
        assert_eq!(page.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert!(has_more);
    }

    #[test]
    fn the_oldest_page_reports_nothing_more() {
        let all = messages(&[0, 1, 2]);
        let (page, has_more) = window(&all, Some(2), 30);
        assert_eq!(page.len(), 2);
        assert!(!has_more);
    }

    #[test]
    fn an_empty_transcript_is_an_empty_page_not_an_error() {
        let (page, has_more) = window(&[], None, 30);
        assert!(page.is_empty());
        assert!(!has_more);
    }

    #[test]
    fn etags_distinguish_tail_cursor_and_limit() {
        let tail = etag("version", None, 30);
        let earlier = etag("version", Some(42), 30);
        let wider = etag("version", None, 60);

        assert_eq!(tail, "\"version-tail-30\"");
        assert_eq!(earlier, "\"version-42-30\"");
        assert_eq!(wider, "\"version-tail-60\"");
    }

    #[test]
    fn both_speakers_cards_carry_rendered_markdown() {
        let user = card(&herdr_remote::transcript::Message {
            seq: 0,
            role: herdr_remote::transcript::Role::User,
            text: "1. <b>hi</b>".into(),
            output: Some("done".into()),
        });
        // The list the user typed renders, and the card still previews the
        // number they wrote; the tag they typed renders as text.
        assert!(user.html.contains("<ol>"));
        assert_eq!(user.preview, "1. <b>hi</b>");
        assert!(user.html.contains("&lt;b&gt;hi&lt;/b&gt;"));
        assert_eq!(user.output.as_deref(), Some("done"));
        let agent = card(&herdr_remote::transcript::Message {
            seq: 1,
            role: herdr_remote::transcript::Role::Assistant,
            text: "**hi**".into(),
            output: None,
        });
        assert!(agent.html.contains("<strong>hi</strong>"));
        assert_eq!(agent.output, None);
    }

    /// The person pressed the key twice; the card shows two lines, in the list
    /// and in the sheet alike.
    #[test]
    fn a_users_own_line_breaks_reach_the_card() {
        let user = card(&herdr_remote::transcript::Message {
            seq: 0,
            role: herdr_remote::transcript::Role::User,
            text: "one\ntwo".into(),
            output: None,
        });
        assert!(user.html.contains("<br />"));
        assert_eq!(user.preview, "one\ntwo");
    }

    #[test]
    fn the_live_view_names_its_fields_the_way_the_phone_reads_them() {
        let json = serde_json::to_value(LiveView {
            screen: "❯ draft".into(),
            composer: "draft".into(),
        })
        .unwrap();
        assert_eq!(json["screen"], "❯ draft");
        assert_eq!(json["composer"], "draft");
    }

    #[test]
    fn opening_a_second_pane_supersedes_the_first() {
        assert_eq!(superseded(&None, "w1:p1"), None);
        assert_eq!(
            superseded(&Some("w1:p1".into()), "w1:p2"),
            Some("w1:p1".into())
        );
        // Re-opening the pane already held is not a transition.
        assert_eq!(superseded(&Some("w1:p1".into()), "w1:p1"), None);
    }

    /// A snapshot whose only zoom is `pane_id`, in the shape `herdr::zoomed`
    /// reads: the zoom lives on the tab's layout, not on the pane.
    fn zoom_snapshot(pane_id: &str) -> Vec<u8> {
        format!(
            "{{\"id\":\"herdr-remote\",\"result\":{{\"snapshot\":{{\"workspaces\":[],\
             \"tabs\":[],\"panes\":[],\"layouts\":[{{\"zoomed\":true,\
             \"focused_pane_id\":\"{pane_id}\"}}]}}}}}}\n"
        )
        .into_bytes()
    }

    /// Both halves of the zoom rule in one test: the socket the fake listens on
    /// is the whole process's, so two tests holding one at a time would race.
    #[tokio::test]
    async fn open_moves_the_zoom_only_where_herdr_has_not_already() {
        let _socket_lock = HERDR_SOCKET_TEST.lock().await;
        let reply = b"{\"id\":\"herdr-remote\",\"result\":{}}\n";
        let snapshot = Box::leak(zoom_snapshot("w1:p1").into_boxed_slice());

        // The pane the phone leaves is released, and the one it asks for zoomed.
        {
            let (socket, mut requests) = herdr::fake_herdr(vec![snapshot, reply, reply]);
            let _socket = herdr::use_test_socket(socket);
            let state = AppState::default();
            *state.zoomed.0.lock().await = Some("w1:p1".into());

            assert_eq!(
                open(State(state), Path("w1:p2".into())).await,
                Ok(StatusCode::NO_CONTENT)
            );

            assert_eq!(requests.recv().await.unwrap()["method"], "session.snapshot");
            let first = requests.recv().await.unwrap();
            let second = requests.recv().await.unwrap();
            assert_eq!(first["method"], "pane.zoom");
            assert_eq!(
                first["params"],
                serde_json::json!({ "pane_id": "w1:p1", "zoomed": false })
            );
            assert_eq!(second["method"], "pane.zoom");
            assert_eq!(
                second["params"],
                serde_json::json!({ "pane_id": "w1:p2", "zoomed": true })
            );
        }

        // The operator's own zoom, or this server's from an earlier open: either
        // way the pane is already where the phone wants it, and toggling it
        // would be the layout moving under the operator for nothing.
        {
            let (socket, mut requests) = herdr::fake_herdr(vec![snapshot]);
            let _socket = herdr::use_test_socket(socket);
            let state = AppState::default();

            assert_eq!(
                open(State(state.clone()), Path("w1:p1".into())).await,
                Ok(StatusCode::NO_CONTENT)
            );

            assert_eq!(requests.recv().await.unwrap()["method"], "session.snapshot");
            assert!(requests.try_recv().is_err(), "no zoom call belongs here");
            // Nothing this server zoomed is nothing for `close` to release.
            assert_eq!(*state.zoomed.0.lock().await, None);
        }
    }

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
        let _socket_lock = HERDR_SOCKET_TEST.lock().await;
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

        // Compare against the served shell so a static fallback cannot masquerade as an API 404.
        let response = app(allowed.clone())
            .oneshot(request("GET", "/", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ui_html = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();

        // An unknown or bare /api path is an API 404, never the UI's HTML.
        for path in ["/api/nope", "/api", "/api/", "//api", "//api/health"] {
            let response = app(allowed.clone())
                .oneshot(request("GET", path, None))
                .await
                .unwrap();
            let (parts, body) = response.into_parts();
            assert_eq!(parts.status, StatusCode::NOT_FOUND, "{path}");
            let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            assert!(body != ui_html, "{path} returned the UI HTML");
            if path == "/api/" {
                assert_eq!(
                    parts
                        .headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|value| value.to_str().ok()),
                    Some("no-store"),
                    "{path}"
                );
            }
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
