use axum::{Router, routing::get};
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .fallback_service(ServeDir::new("web/dist"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787").await?;
    println!("listening on http://127.0.0.1:8787");
    axum::serve(listener, app).await?;
    Ok(())
}
