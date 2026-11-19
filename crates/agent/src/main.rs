use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use http_body_util::{Full, BodyExt};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Instant;

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn uptime_s() -> u64 {
    START.get().map(|s| s.elapsed().as_secs()).unwrap_or(0)
}

async fn handle(req: Request<hyper::body::Incoming>) -> Result<Response<Full<bytes::Bytes>>, Infallible> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    match (method, path.as_str()) {
        (Method::GET, "/api/health") => {
            let body = serde_json::json!({
                "ok": true,
                "pid": std::process::id(),
                "uptime_s": uptime_s(),
                "port": 8199u16,
            });
            Ok(json_response(StatusCode::OK, &body))
        }
        (Method::GET, "/api/current") => {
            // Read from root sample.json for power/temps
            let home = std::env::var("HOME").unwrap_or_default();
            let sample_path = format!("{}/Library/Application Support/syswatch/root/sample.json", home);
            let root_sample: serde_json::Value = std::fs::read_to_string(&sample_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::json!({}));

            let body = serde_json::json!({
                "cpu_total": null,
                "memory_used_pct": null,
                "root_sample": root_sample,
                "agent": "rust",
                "uptime_s": uptime_s(),
            });
            Ok(json_response(StatusCode::OK, &body))
        }
        _ => {
            // Serve static files from disk
            let base = "dashboard/static";
            let file_path = if path == "/" { format!("{}/index.html", base) } else { format!("{}{}", base, path) };
            match std::fs::read(&file_path) {
                Ok(data) => {
                    let ct = if file_path.ends_with(".html") { "text/html" }
                        else if file_path.ends_with(".js") { "application/javascript" }
                        else if file_path.ends_with(".css") { "text/css" }
                        else if file_path.ends_with(".json") { "application/json" }
                        else if file_path.ends_with(".png") { "image/png" }
                        else if file_path.ends_with(".svg") { "image/svg+xml" }
                        else { "application/octet-stream" };
                    let mut resp = Response::new(Full::new(bytes::Bytes::from(data)));
                    resp.headers_mut().insert("content-type", ct.parse().unwrap());
                    Ok(resp)
                }
                Err(_) => {
                    let mut resp = Response::new(Full::new(bytes::Bytes::from("Not Found")));
                    *resp.status_mut() = StatusCode::NOT_FOUND;
                    Ok(resp)
                }
            }
        }
    }
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Response<Full<bytes::Bytes>> {
    let json = serde_json::to_string(body).unwrap_or_default();
    let mut resp = Response::new(Full::new(bytes::Bytes::from(json)));
    *resp.status_mut() = status;
    resp.headers_mut().insert("content-type", "application/json".parse().unwrap());
    resp.headers_mut().insert("access-control-allow-origin", "*".parse().unwrap());
    resp
}

#[tokio::main]
async fn main() {
    START.set(Instant::now()).ok();

    let port: u16 = std::env::args()
        .skip_while(|a| a != "--port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8199);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("syswatch-agent listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service_fn(handle))
                .await
            {
                eprintln!("connection error: {}", e);
            }
        });
    }
}
