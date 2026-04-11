use actix_web::{web, App, HttpServer, Responder, HttpResponse};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse};
use futures_util::future::Either;

mod app_config;
mod auth;
mod network_scanner;
mod relay;
use network_scanner::NetworkScanner;

async fn get_devices(scanner: web::Data<NetworkScanner>) -> impl Responder {
    HttpResponse::Ok().json(scanner.get_devices())
}

async fn get_latest(scanner: web::Data<NetworkScanner>) -> impl Responder {
    match scanner.get_latest() {
        Some(device) => HttpResponse::Ok().json(device),
        None => HttpResponse::NoContent().finish(),
    }
}

async fn health() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Health</title></head>
<body><h1>&#x2705; OK</h1><p>Server is up and running.</p></body>
</html>"#)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    eprintln!("[BOOT] working dir  : {:?}", std::env::current_dir().unwrap_or_default());

    eprintln!("[BOOT] loading config.yaml …");
    let settings = app_config::Settings::load()
        .expect("Failed to load config.yaml");
    eprintln!("[BOOT] config loaded OK");
    eprintln!("[BOOT] server.host  : {}", settings.server.host);
    eprintln!("[BOOT] server.port  : {}", settings.server.port);

    let scanner = NetworkScanner::new(&settings.scanner.network_base);
    scanner.start(settings.scanner.delay_seconds);

    let scanner_data = web::Data::new(scanner);
    let relay_data = web::Data::new(relay::RelayState::new(
        settings.relay.targets,
        scanner_data.get_ref().clone(),
    ));
    let auth_data = web::Data::new(auth::AuthState::new(settings.users, settings.server.token_ttl_seconds));

    let bind_addr = format!("{}:{}", settings.server.host, settings.server.port);
    println!("Listening on http://{}", bind_addr);
    println!("Relay targets: {:?}", relay_data.targets);
    println!(
        "Authentication: {}",
        if auth_data.is_disabled() { "disabled (no users configured)" } else { "enabled – POST /login to get a token" }
    );
    println!("TLS: disabled (HTTPS handled by Cloudflare Tunnel)");

    HttpServer::new(move || {
        let auth_mw = auth_data.clone();

        App::new()
            .app_data(scanner_data.clone())
            .app_data(relay_data.clone())
            .app_data(auth_data.clone())
            // ── Bearer-token middleware (skips /login) ───────────────────────
            .wrap_fn(move |req: ServiceRequest, srv| {
                let authorized = req.path() == "/login"
                    || req.path() == "/health"
                    || auth_mw.check_request(&req);

                if authorized {
                    let fut = srv.call(req);
                    Either::Left(async move { fut.await })
                } else {
                    let (http_req, _) = req.into_parts();
                    let response = HttpResponse::Unauthorized()
                        .insert_header(("WWW-Authenticate", r#"Bearer realm="rust-server2""#))
                        .content_type("text/plain")
                        .body("Unauthorized: POST /login to obtain a token");
                    Either::Right(async move {
                        Ok(ServiceResponse::new(http_req, response))
                    })
                }
            })
            // ── Routes ───────────────────────────────────────────────────────
            .route("/login", web::post().to(auth::login_handler))
            .route("/logout", web::post().to(auth::logout_handler))
            .route("/health", web::get().to(health))
            .route("/devices", web::get().to(get_devices))
            .route("/devices/latest", web::get().to(get_latest))
            .route("/relay/{target}", web::route().to(relay::relay_root_handler))
            .route("/relay/{target}/{path:.*}", web::route().to(relay::relay_handler))
    })
    .bind(bind_addr)?
    .run()
    .await
}
