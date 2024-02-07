use bytes::Bytes;
use clap::Parser;
use http_body_util::{combinators::BoxBody, BodyExt};
use hyper::client::conn::http1::Builder;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::upgrade::OnUpgrade;
use hyper::{HeaderMap, Request, Response, StatusCode};
use once_cell::sync::Lazy;
use regex::Regex;
use std::{
    net::{Ipv4Addr, SocketAddr},
    str::FromStr as _,
};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::signal::{
    ctrl_c,
    unix::{signal, SignalKind},
};
use tracing::{debug, error, info};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod tokio_io;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = String::from("0.0.0.0"))]
    proxy_host: String,

    #[arg(long, default_value_t = 8100)]
    proxy_port: u16,

    #[arg(long, default_value_t = String::from("localhost"))]
    backend_host: String,

    #[arg(long, default_value_t = 80)]
    backend_port: u16,

    #[arg(long, default_value_t = String::from("localhost"))]
    domain_suffix: String,
}

fn extract_domain(s: &str) -> Option<String> {
    static XP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"^(?<domain>([a-zA-Z0-9][a-zA-Z0-9-]*[a-zA-Z0-9]*\.)+)([0-9]{1,3}\.){4}nip\.io(:[0-9]+)?$",
        )
        .unwrap()
    });

    XP.captures(s).map(|r| String::from(&r["domain"]))
}

async fn proxy(
    mut req: Request<hyper::body::Incoming>,
    args: Args,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    let host = extract_domain(req.headers()["host"].to_str().unwrap()).unwrap();
    let host = format!("{}{}", host, args.domain_suffix);

    info!("connecting to {}", host);
    info!("headers: {:?}", req.headers());

    req.headers_mut().remove("host");
    req.headers_mut()
        .insert("host", host.parse().expect("host.parse() failed"));

    let stream = TcpStream::connect((args.backend_host, args.backend_port))
        .await
        .unwrap();

    let io = tokio_io::TokioIo::new(stream);

    let request_upgrade_type = get_upgrade_type(req.headers());
    let request_upgraded = req.extensions_mut().remove::<OnUpgrade>();

    let (mut sender, conn) = Builder::new()
        .preserve_header_case(true)
        .title_case_headers(true)
        .handshake(io)
        .await?;
    tokio::task::spawn(async move {
        if let Err(err) = conn.with_upgrades().await {
            println!("Connection failed: {:?}", err);
        }
    });

    let mut resp = sender.send_request(req).await?;

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let response_upgrade_type = get_upgrade_type(resp.headers());

        if request_upgrade_type == response_upgrade_type {
            if let Some(request_upgraded) = request_upgraded {
                let response_upgraded = resp
                    .extensions_mut()
                    .remove::<OnUpgrade>()
                    .expect("response does not have an upgrade extension")
                    .await?;

                debug!("Responding to a connection upgrade response");

                tokio::spawn(async move {
                    let request_upgraded =
                        request_upgraded.await.expect("failed to upgrade request");

                    let mut a = tokio_io::TokioIo::new(response_upgraded);
                    let mut b = tokio_io::TokioIo::new(request_upgraded);

                    copy_bidirectional(&mut a, &mut b)
                        .await
                        .expect("coping between upgraded connections failed");
                });

                // Ok(resp)
            } else {
                error!("request does not have an upgrade extension")
            }
        } else {
            error!(
                "backend tried to switch to protocol {:?} when {:?} was requested",
                response_upgrade_type, request_upgrade_type
            )
        }
    }

    Ok(resp.map(|b| b.boxed()))
}

fn get_upgrade_type(headers: &HeaderMap) -> Option<String> {
    #[allow(clippy::blocks_in_if_conditions)]
    if headers
        .get("connection")
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(',')
                .any(|e| e.trim().to_lowercase() == "upgrade")
        })
        .unwrap_or(false)
    {
        if let Some(upgrade_value) = headers.get("upgrade") {
            debug!(
                "Found upgrade header with value: {}",
                upgrade_value.to_str().unwrap().to_owned()
            );

            return Some(upgrade_value.to_str().unwrap().to_owned());
        }
    }

    None
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let addr = SocketAddr::from((
        Ipv4Addr::from_str(&args.proxy_host).expect("invalid ip v4 addr"),
        args.proxy_port,
    ));

    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{}", addr);

    let mut sig_int = signal(SignalKind::interrupt()).unwrap();
    let mut sig_term = signal(SignalKind::terminate()).unwrap();
    tokio::select! {
        _ = async {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(sock) => sock,
                    Err(e) => {
                        error!("Error when accepting {:?}", e);
                        break;
                    }
                };

                let args = args.clone();
                let io = tokio_io::TokioIo::new(stream);

                tokio::task::spawn(async move {
                    let service = service_fn( move |req| {
                        let args = args.clone();
                        proxy(req, args)
                });

                    if let Err(err) = http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(io, service).with_upgrades()
                        .await
                    {
                        println!("Failed to serve connection: {:?}", err);
                    }
                });
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        } => {},
        _ = sig_int.recv() => debug!("SIGINT received"),
        _ = sig_term.recv() => debug!("SIGTERM received"),
        _ = ctrl_c() => debug!("'Ctrl C' received"),
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_regex() {
        assert!(extract_domain("foo.192.168.1.1.nip.io") == Some("foo.".to_string()));
        assert!(extract_domain("foo.bar.192.168.1.1.nip.io") == Some("foo.bar.".to_string()));
        assert!(extract_domain("foo.192.168.1.1.nip.io:8888") == Some("foo.".to_string()));
        assert!(extract_domain("foo.bar.192.168.1.1.nip.io:8888") == Some("foo.bar.".to_string()));
    }
}
