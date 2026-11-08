//! Every route of the API is listed in `routes.txt` with who may call it and whether it is reached
//! from outside the house. A route added to the code and not listed (or listed and gone) fails
//! here: whoever adds it decides, and the test then checks that the level holds.

use std::time::Duration;

use moli_runtime::{Hub, HubOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// The routes written in `src`: (METHOD, path), `{id}` kept as is.
fn declared(src: &str) -> Vec<(String, String)> {
    let src = src.split("#[cfg(test)]").next().unwrap();
    let mut found = Vec::new();
    for (at, _) in src.match_indices(".route(") {
        let rest = &src[at + ".route(".len()..];
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"').expect("route path is a literal");
        let (path, rest) = rest.split_once('"').unwrap();
        let mut depth = 1;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = i;
                break;
            }
        }
        let call = &rest[..end];
        let before = found.len();
        for method in ["get", "post", "put", "delete", "patch"] {
            for (i, _) in call.match_indices(&format!("{method}(")) {
                let prev = call[..i].chars().next_back();
                if !prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    found.push((method.to_uppercase(), path.to_owned()));
                }
            }
        }
        assert!(found.len() > before, "no method found for route {path}");
    }
    found
}

async fn serve(
    options: &moli_api::Options,
) -> (String, CancellationToken, tokio::task::JoinHandle<()>) {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();
    let app = moli_api::router(hub, shutdown.clone(), options);
    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let stopped = shutdown.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
            .unwrap();
    });
    (addr, shutdown, task)
}

/// The status line only: live streams never end. `extra`: header lines, each ending with CRLF.
async fn status(addr: &str, method: &str, path: &str, extra: &str) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\n{extra}Content-Length: 2\r\n\r\n{{}}"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut line = [0u8; 12];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut line))
        .await
        .unwrap_or_else(|_| panic!("{method} {path}: no answer"))
        .unwrap();
    String::from_utf8_lossy(&line[9..12]).parse().unwrap()
}

async fn stop(shutdown: CancellationToken, task: tokio::task::JoinHandle<()>) {
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("route server did not stop")
        .unwrap();
}

/// The status and the body of a refusal (a refusal is short and closes).
async fn refusal(addr: &str, method: &str, path: &str, extra: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\n{extra}Content-Length: 2\r\n\r\n{{}}"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut raw = vec![0u8; 4096];
    let mut len = 0;
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(500), stream.read(&mut raw[len..])).await
    {
        if n == 0 || len + n == raw.len() {
            len += n;
            break;
        }
        len += n;
    }
    let raw = String::from_utf8_lossy(&raw[..len]).to_string();
    let status = raw.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
    (status, body)
}

/// `{id}`, `{z}`… become a plain value.
fn concrete(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some((head, tail)) = rest.split_once('{') {
        out.push_str(head);
        out.push('x');
        rest = tail.split_once('}').unwrap().1;
    }
    out + rest
}

/// `routes.txt`: `METHOD path level remote`, then free text.
fn listed() -> Vec<(String, String, String, bool)> {
    include_str!("routes.txt")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut w = l.split_whitespace();
            let mut next = || {
                w.next()
                    .unwrap_or_else(|| panic!("routes.txt: bad line {l:?}"))
            };
            let (method, path, level, remote) = (next(), next(), next(), next());
            match (level, remote) {
                (_, "oui" | "non") => {}
                _ => panic!("routes.txt: remote is `oui` or `non`, not {remote:?}: {l:?}"),
            }
            // The sign-in is how a person comes in from outside (I3).
            assert!(level != "public" || remote == "oui", "routes.txt: {l:?}");
            // Agents stay at home unless the house decides otherwise (I4).
            assert!(path != "/mcp" || remote == "non", "routes.txt: {l:?}");
            (
                method.to_owned(),
                path.to_owned(),
                level.to_owned(),
                remote == "oui",
            )
        })
        .collect()
}

#[tokio::test]
async fn every_route_is_listed_and_holds_its_level() {
    let (lib, weather) = (
        include_str!("../src/lib.rs"),
        include_str!("../src/weather.rs"),
    );
    let mut code = declared(lib);
    code.extend(declared(weather));
    code.extend(declared(include_str!("../src/overkiz.rs")));
    // The MCP service is nested, not routed.
    assert!(
        lib.contains("\"/mcp\""),
        "the MCP service moved: update this test"
    );
    code.push(("POST".into(), "/mcp".into()));
    assert!(
        code.len() > 80,
        "only {} routes found: is the parser lost?",
        code.len()
    );

    let listed = listed();

    let new: Vec<_> = code
        .iter()
        .filter(|(m, p)| !listed.iter().any(|(lm, lp, ..)| lm == m && lp == p))
        .map(|(m, p)| format!("  new route {m} {p}: add it to tests/routes.txt with its level"))
        .collect();
    let gone: Vec<_> = listed
        .iter()
        .filter(|(m, p, ..)| !code.iter().any(|(cm, cp)| cm == m && cp == p))
        .map(|(m, p, ..)| format!("  gone route {m} {p}: remove it from tests/routes.txt"))
        .collect();
    assert!(
        new.is_empty() && gone.is_empty(),
        "\n{}",
        [new, gone].concat().join("\n")
    );

    // A request relayed through a proxy is a stranger's, even from this machine.
    let stranger = "X-Forwarded-For: 203.0.113.9\r\n";
    let mut wrong = Vec::new();
    for (method, path, level, _) in &listed {
        // Authorization must not inherit another route’s exhausted rate budget.
        let (addr, shutdown, task) = serve(&moli_api::Options::default()).await;
        let url = concrete(path);
        let outside = status(&addr, method, &url, stranger).await;
        let household = status(&addr, method, &url, "").await;
        stop(shutdown, task).await;
        let ok = match level.as_str() {
            "public" => outside != 403,
            "foyer" => outside == 403,
            "humain" => outside == 403 && household == 403,
            other => panic!("routes.txt: unknown level {other:?} for {method} {path}"),
        };
        if !ok {
            wrong.push(format!(
                "  {method} {path} is {level}: stranger got {outside}, household got {household}"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[tokio::test]
async fn what_stays_at_home_is_refused_to_what_comes_from_outside() {
    let listed = listed();
    let stranger = "X-Forwarded-For: 203.0.113.9\r\n";
    let mut wrong = Vec::new();
    // This machine declared as a reverse proxy: what it relays came in from
    // outside, where what stays at home is refused with its own answer.
    let behind_proxy = moli_api::Options {
        remote: moli_api::RemoteConfig {
            vpn: Vec::new(),
            proxies: vec!["127.0.0.1".parse().unwrap()],
        },
        ..moli_api::Options::default()
    };
    let (addr, shutdown, task) = serve(&behind_proxy).await;
    let (_, as_stranger) = refusal(&addr, "GET", "/api/devices", stranger).await;
    let mut proxied = Vec::new();
    for (i, (method, path, _, _)) in listed.iter().enumerate() {
        // One client each: no route spends another's rate budget.
        let client = format!("X-Forwarded-For: 198.51.{}.{}\r\n", i / 200, i % 200 + 1);
        proxied.push(refusal(&addr, method, &concrete(path), &client).await);
    }
    stop(shutdown, task).await;
    for ((method, path, _, remote), (status, body)) in listed.iter().zip(&proxied) {
        let at_home_only = *status == 403 && *body != as_stranger;
        if at_home_only == *remote {
            wrong.push(format!(
                "  {method} {path} is distant {}: behind a declared proxy got {status} {body}",
                if *remote { "oui" } else { "non" }
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
