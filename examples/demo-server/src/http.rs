//! HTTP API: liveness + status endpoint with connection state and build info.

use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
use serde::Serialize;
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::watch;
use tracing_actix_web::TracingLogger;

use crate::{BUILD_DATE, BUILD_VERSION, CARGO_VERSION, GIT_REV};

#[derive(Clone)]
pub struct ServerStatus {
    pub mqtt_connected: watch::Receiver<bool>,
    pub db_connected: watch::Receiver<bool>,
}

#[derive(Serialize)]
struct Status {
    mqtt_connected: bool,
    db_connected: bool,
    version: String,
    cargo_version: String,
    git_rev: String,
    build_date: String,
}

#[get("/server_online")]
async fn server_online() -> impl Responder {
    HttpResponse::Ok()
}

#[get("/status")]
async fn status(state: web::Data<ServerStatus>) -> impl Responder {
    HttpResponse::Ok().json(Status {
        mqtt_connected: *state.mqtt_connected.borrow(),
        db_connected: *state.db_connected.borrow(),
        version: BUILD_VERSION.unwrap_or("dev").to_string(),
        cargo_version: CARGO_VERSION.to_string(),
        git_rev: GIT_REV.unwrap_or("dev").to_string(),
        build_date: BUILD_DATE.unwrap_or("dev").to_string(),
    })
}

pub fn start_task(state: ServerStatus, on_panic: PanicCallback) -> Handle {
    let task_function = server_task;
    Handle::new(
        move |shutdown_rx| task_function(shutdown_rx, state.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn server_task(mut shutdown_rx: watch::Receiver<()>, state: ServerStatus) {
    let bind_addr = crate::get_http_bind_addr();
    let state = web::Data::new(state);

    let server = match HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .wrap(TracingLogger::default())
            .service(web::scope("/api/v1").service(server_online).service(status))
    })
    .bind(&bind_addr)
    {
        Ok(server) => server.run(),
        Err(err) => {
            tracing::error!("Failed to bind HTTP server to {bind_addr}: {err}");
            return;
        }
    };
    tracing::info!("HTTP server listening on {bind_addr}");

    let server_handle = server.handle();
    tokio::select! {
        _ = shutdown_rx.changed() => {
            server_handle.stop(true).await;
        }
        result = server => {
            if let Err(err) = result {
                tracing::error!("HTTP server stopped: {err}");
            }
        }
    }
}
