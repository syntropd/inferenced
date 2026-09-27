pub mod handlers;
pub mod types;

#[cfg(test)]
mod tests;

pub use handlers::AppState;

use crate::activation::GatewayListener;
use axum::{
    routing::{get, post},
    Router,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use std::future::Future;
use std::sync::Arc;
use tokio::net::UnixListener;
use tracing::info;

pub fn build_gateway_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handlers::health_handler))
        .route("/v1/models", get(handlers::models_handler))
        .route("/v1/chat/completions", post(handlers::chat_completions_handler))
        .with_state(state)
}

pub async fn serve_gateway<F>(
    listener: GatewayListener,
    app: Router,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    match listener {
        GatewayListener::Tcp(tcp) => {
            info!("Serving HTTP Gateway over TCP");
            axum::serve(tcp, app)
                .with_graceful_shutdown(shutdown)
                .await?;
        }
        GatewayListener::Unix(unix) => {
            info!("Serving HTTP Gateway over Unix stream socket");
            serve_unix_gateway(unix, app, shutdown).await?;
        }
    }
    Ok(())
}

async fn serve_unix_gateway<F>(
    listener: UnixListener,
    app: Router,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            res = listener.accept() => {
                let (stream, _) = res?;
                let app = app.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = TowerToHyperService::new(app);
                    let _ = Builder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await;
                });
            }
        }
    }
    Ok(())
}
