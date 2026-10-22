use std::{env, net::SocketAddr};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "music_api=info,tower_http=info".into()),
        )
        .init();

    let bind = env::var("MUSIC_API_BIND").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
    let addr: SocketAddr = bind
        .parse()
        .unwrap_or_else(|err| panic!("invalid MUSIC_API_BIND {bind}: {err}"));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));

    tracing::info!("music-api listening on http://{addr}");
    axum::serve(listener, music_api::api::app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("signal handler failed")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("termination handler failed")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = interrupt => {}, _ = terminate => {} }
    tracing::info!("shutdown requested; draining active connections");
}
