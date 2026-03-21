use std::sync::Arc;

use tower_lsp::{LspService, Server};
use tracing_subscriber::prelude::*;

mod backend;
mod cache;
mod config;
mod providers;
mod version_utils;

#[tokio::main]
async fn main() {
    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(tracing::Level::ERROR.into());

    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .json(),
        )
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let reload_handle = Arc::new(reload_handle);
    let (service, socket) =
        LspService::new(move |client| backend::Backend::new(client, Arc::clone(&reload_handle)));

    Server::new(stdin, stdout, socket).serve(service).await;
}
