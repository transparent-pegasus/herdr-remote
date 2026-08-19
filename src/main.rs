mod herdr;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::{Json, Router, routing::get, routing::post};
use serde::Deserialize;
use tower_http::services::ServeDir;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn failed(error: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
}

async fn session() -> ApiResult<Json<herdr::Session>> {
    herdr::session().await.map(Json).map_err(failed)
}

#[derive(Deserialize)]
struct Prompt {
    text: String,
}

async fn prompt(Path(pane_id): Path<String>, Json(body): Json<Prompt>) -> ApiResult<StatusCode> {
    let has_agent = herdr::pane_has_agent(&pane_id)
        .await
        .map_err(failed)?
        .ok_or((StatusCode::NOT_FOUND, format!("no pane {pane_id}")))?;
    herdr::prompt(&pane_id, &body.text, has_agent)
        .await
        .map_err(failed)?;
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
