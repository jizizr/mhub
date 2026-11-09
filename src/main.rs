use music_api::settings::SETTINGS;
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
    let addr = parse_addr("MUSIC_API_BIND", &bind);
    let listener = bind_listener(addr).await;

    let gateway_listen = SETTINGS.gateway.listen.trim();
    let app = if gateway_listen.is_empty() {
        music_api::api::app()
    } else {
        // A misconfigured gateway must fail loudly rather than run open or
        // silently leave its clients without music.
        let token = SETTINGS
            .gateway
            .resolve_token()
            .unwrap_or_else(|err| panic!("invalid [gateway] config: {err}"));
        let gateway_addr = parse_addr("gateway.listen", gateway_listen);
        if !gateway_addr.ip().is_loopback() {
            tracing::warn!("music gateway listens on non-loopback address {gateway_addr}");
        }
        let gateway_listener = bind_listener(gateway_addr).await;
        let (app, gateway) = music_api::api::app_and_gateway(&token);
        tracing::info!("music gateway listening on http://{gateway_addr}");
        tokio::spawn(async move {
            axum::serve(gateway_listener, gateway)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .expect("gateway server failed");
        });
        app
    };

    tracing::info!("music-api listening on http://{addr}");
    // The peer address decides whether X-Real-IP is trusted for upstream quotas.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("server failed");
}

fn parse_addr(name: &str, value: &str) -> SocketAddr {
    value
        .parse()
        .unwrap_or_else(|err| panic!("invalid {name} {value}: {err}"))
}

async fn bind_listener(addr: SocketAddr) -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"))
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
