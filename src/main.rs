use axum::{Router, routing::get};
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
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
