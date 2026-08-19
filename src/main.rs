mod herdr;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::{Json, Router, routing::get, routing::post};
use serde::Deserialize;
use tower_http::services::ServeDir;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/session", get(session))
        .route("/api/panes/{pane_id}/prompt", post(prompt))
        .fallback_service(ServeDir::new("web/dist"));
    // loopback only: the tunnel is the sole ingress
    let addr = format!(
        "127.0.0.1:{}",
        std::env::var("PORT").unwrap_or_else(|_| "8787".into())
    );
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
