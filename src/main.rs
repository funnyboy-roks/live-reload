use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    body::Body,
    extract::{ws::WebSocket, Path, State, WebSocketUpgrade},
    http::HeaderValue,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Parser;
use hyper::{header, StatusCode};
use tokio::{fs, signal::unix::SignalKind};
use tokio_util::io::ReaderStream;
use tower_http::services::ServeDir;
use tracing::{debug, error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod cli;

pub struct AppState {
    pub tx: async_channel::Sender<()>,
    pub rx: async_channel::Receiver<()>,
    pub path: PathBuf,
}

async fn handle_socket(mut ws: WebSocket, state: Arc<AppState>) {
    let rx = state.rx.clone();

    // empty the rx so we don't get unnecessary reloads
    while !rx.is_empty() {
        rx.recv().await.expect("clear rx buffer")
    }

    // fn like this so I can actually have rustfmt
    async fn on_recv(ws: &mut WebSocket, v: Result<(), async_channel::RecvError>) {
        match v {
            Ok(()) => {
                match ws
                    .send(axum::extract::ws::Message::Binary(Vec::new()))
                    .await
                {
                    Ok(()) => {
                        debug!("Sent refresh message to page");
                    }
                    Err(e) => {
                        error!(?e, "error after sending reload message");
                        return;
                    }
                }
            }
            Err(e) => {
                error!(?e, "error when receiving message from signal channel");
                return;
            }
        }
    }

    loop {
        // trigger on either receiving a message from `rx` or the socket
        tokio::select! {
            // if we receive a message from the rx, then we should refresh the page
            v = rx.recv() => on_recv(&mut ws, v).await,
            // if we receive a message from the websocket, it is close, so let's stop listening
            _ = ws.recv() => {
                debug!("socket close");
                return;
            }
        }
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(|ws| handle_socket(ws, state))
}

async fn handle_signal(tx: async_channel::Sender<()>) -> anyhow::Result<()> {
    let mut sig = tokio::signal::unix::signal(SignalKind::hangup())
        .context("Creating listener for SIGHUP")?;

    while sig.recv().await.is_some() {
        tx.send(()).await?;
        info!("Received SIGHUP signal");
    }

    error!("Done listening for signals");
    Ok(())
}

fn validate_path(path: &std::path::Path) -> bool {
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) => {
                // Not supposed to be reachable
                return false;
            }
            std::path::Component::RootDir => {
                // Not supposed to be reachable
                return false;
            }
            std::path::Component::CurDir => {} // these don't matter much
            std::path::Component::ParentDir => {
                // We want to get mad about these since they can cause exploits
                return false;
            }
            std::path::Component::Normal(_) => {}
        };
    }
    return true;
}

fn not_found() -> impl IntoResponse {
    return (StatusCode::NOT_FOUND, "404: Page not found.");
}

async fn serve_file(path: Option<Path<PathBuf>>, State(state): State<Arc<AppState>>) -> Response {
    let Path(path) = path.unwrap_or_else(|| Path(PathBuf::new()));

    if !validate_path(&path) {
        return not_found().into_response();
    }

    let mut full_path: PathBuf = state.path.components().chain(path.components()).collect();

    if full_path.is_dir() {
        full_path.push("index.html");
    }

    if !full_path.exists() {
        return not_found().into_response();
    }

    let mt = mime_guess::from_path(&full_path).first();

    let mut res = match mt {
        Some(m) if m.essence_str() == "text/html" => {
            let s = match fs::read_to_string(&full_path).await {
                Ok(s) => s,
                Err(e) => {
                    error!(
                        ?e,
                        full_path = %full_path.display(),
                        "Error when reading file at path"
                    );
                    return not_found().into_response();
                }
            };

            let prev_len = s.len();

            let mut s = s.replace(
                "</body>",
                concat!(
                    "<script>",
                    include_str!("../extra/js.js"),
                    "</script></body>"
                ),
            );

            if prev_len == s.len() {
                s.push_str(concat!(
                    "<script>",
                    include_str!("../extra/js.js"),
                    "</script>"
                ));
            }

            let mut res = Response::new(Body::from(s));
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(m.essence_str()).unwrap(),
            );

            res
        }
        _ => {
            let file = match fs::File::open(&full_path).await {
                Ok(f) => f,
                Err(e) => {
                    error!(
                        ?e,
                        full_path = %full_path.display(),
                        "Error when reading file at path"
                    );
                    return not_found().into_response();
                }
            };

            Response::new(Body::from_stream(ReaderStream::new(file)))
        }
    };

    res.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );

    res
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tracing = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer());

    if cfg!(debug_assertions) {
        tracing.with(tracing_subscriber::filter::LevelFilter::DEBUG)
    } else {
        tracing.with(tracing_subscriber::filter::LevelFilter::INFO)
    }
    .init();

    let cli = cli::Cli::parse();
    debug!(?cli, "parsed cli");

    let (tx, rx) = async_channel::unbounded();

    // Only spawn the handle_signal future if we don't want a static server
    if !cli.static_only {
        tokio::spawn(handle_signal(tx.clone()));
    }

    let app = Router::new();

    let app = if !cli.static_only {
        app.route("/ws", get(ws_handler))
            .route("/", get(serve_file))
            .route("/*path", get(serve_file))
    } else {
        app.nest_service("/", ServeDir::new(&cli.directory))
    }
    .layer(tower_http::trace::TraceLayer::new_for_http())
    .with_state(Arc::new(AppState {
        tx,
        rx,
        path: cli.directory,
    }));

    let addr = SocketAddr::new(cli.addr, cli.port);
    println!("Listening at http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .context("Opening TCP listener")?;

    axum::serve(listener, app).await.context("Running server")?;

    Ok(())
}
