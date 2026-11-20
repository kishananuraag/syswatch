use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn parity_md() -> String {
    std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../PARITY.md"),
    )
    .unwrap_or_default()
}

fn http_get(port: u16, path: &str) -> Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    )
    .unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(Value::Null)
}

fn key_paths(v: &Value, prefix: String, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{}.{}", prefix, k)
                };
                out.push(p.clone());
                key_paths(val, p, out);
            }
        }
        Value::Array(arr) => {
            if let Some(first) = arr.first() {
                let p = format!("{}[]", prefix);
                key_paths(first, p, out);
            }
        }
        _ => {}
    }
}

fn type_cat(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        if let Some(name) = seg.strip_suffix("[]") {
            cur = cur.get(name)?;
            cur = cur.as_array()?.first()?;
        } else {
            cur = cur.get(seg)?;
        }
    }
    Some(cur)
}

fn spawn_agent() -> (Child, u16) {
    let bin = env!("CARGO_BIN_EXE_syswatch-agent");
    let mut child = Command::new(bin)
        .arg("--port")
        .arg("8299")
        .arg("--tick-ms")
        .arg("1000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agent");
    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut port: Option<u16> = None;
    for line in reader.lines() {
        if Instant::now() > deadline {
            break;
        }
        if let Ok(l) = line {
            if let Some(idx) = l.find("SYSWATCH_AGENT_READY on port ") {
                let rest = &l[idx + "SYSWATCH_AGENT_READY on port ".len()..];
                if let Some(n) = rest.split_whitespace().next() {
                    port = n.parse().ok();
                    break;
                }
            }
        }
    }
    let port = port.unwrap_or(8299);
    // give sampler a moment
    std::thread::sleep(Duration::from_secs(7));
    (child, port)
}

#[test]
fn api_parity() {
    let (mut child, port) = spawn_agent();
    let md = parity_md();

    let endpoints: &[(&str, &str, &str)] = &[
        ("health", "py-health", "/api/health"),
        ("current", "py-current", "/api/current"),
        ("sensors", "py-sensors", "/api/sensors"),
        ("consumers", "py-consumers", "/api/consumers?module=cpu&range=1h"),
        ("apps", "py-apps", "/api/apps?range=1h"),
        ("history", "py-history", "/api/history?range=1h"),
        ("power", "py-power", "/api/power"),
        ("analytics", "py-analytics", "/api/analytics?range=1h"),
        ("battery", "py-battery", "/api/battery"),
    ];

    let mut total_keys = 0usize;
    let mut mismatches = 0usize;
    let n_endpoints = endpoints.len();

    for (name, fixture_name, url) in endpoints {
        let fx_path = fixture_dir().join(format!("{}.json", fixture_name));
        let fx: Value = serde_json::from_str(&std::fs::read_to_string(&fx_path).unwrap()).unwrap();
        let rust = http_get(port, url);

        let mut fx_keys = Vec::new();
        key_paths(&fx, String::new(), &mut fx_keys);

        let mut rust_keys = Vec::new();
        key_paths(&rust, String::new(), &mut rust_keys);

        for k in &fx_keys {
            // Empty arrays on the Rust side have no element to check nested
            // keys against; treat them as "not applicable" rather than missing.
            let mut skip = false;
            if k.contains("[]") {
                let array_path = k.split("[]").next().unwrap();
                if let Some(Value::Array(a)) = get_path(&rust, array_path) {
                    if a.is_empty() {
                        skip = true;
                    }
                }
            }
            if skip {
                continue;
            }
            total_keys += 1;
            match get_path(&rust, k) {
                Some(rv) => {
                    let ft = get_path(&fx, k).map(type_cat).unwrap();
                    let rt = type_cat(rv);
                    if ft != rt {
                        eprintln!("TYPE MISMATCH {name} {k}: fixture={ft} rust={rt}");
                        mismatches += 1;
                    }
                }
                None => {
                    eprintln!("MISSING {name} {k}");
                    mismatches += 1;
                }
            }
        }

        // extras must be listed in PARITY.md
        for k in &rust_keys {
            if !fx_keys.contains(k) {
                let ep = url.split('?').next().unwrap();
                let needle = format!("| {} | {} |", ep, k);
                if !md.contains(&needle) {
                    eprintln!("UNLISTED EXTRA {ep} {k}");
                    mismatches += 1;
                }
            }
        }
    }

    let _ = child.kill();
    println!("{} endpoints, {} keys verified, {} mismatches", n_endpoints, total_keys, mismatches);
    assert_eq!(mismatches, 0, "parity mismatches");
}