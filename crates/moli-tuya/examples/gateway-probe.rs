//! Read only: asks a Tuya gateway (protocol 3.3) the state of one of its
//! sub-devices, the way the driver does, and shows its answers. Never sends
//! an order. The gateway's local key comes on stdin, from a pipe:
//!
//!   python3 -I tools/tuya-iot/tuya-iot-export.py --key=<gateway id end> \
//!     | cargo run -q -p moli-tuya --example gateway-probe -- <ip> <gateway id> <cid>
//!
//! The driver holds a connection to the gateway: some accept one at a time,
//! and drop it for the probe's while it runs (about 20 s).

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use moli_tuya::protocol::{Codec, Version, cmd};
use serde_json::json;

const PORT: u16 = 6668;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every frame heard for `wait`, shown as its JSON.
fn listen(stream: &mut TcpStream, codec: &Codec, buf: &mut Vec<u8>, wait: Duration) {
    let end = Instant::now() + wait;
    let mut chunk = [0u8; 2048];
    while Instant::now() < end {
        match stream.read(&mut chunk) {
            Ok(0) => {
                println!("   (the gateway closed the connection)");
                return;
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => {
                println!("   (read error: {e})");
                return;
            }
        }
        while let Ok(Some(frame)) = codec.decode(buf) {
            if frame.cmd == cmd::HEART_BEAT {
                continue;
            }
            let shown = match codec.open_json(&frame.payload) {
                Ok(Some(json)) => json.to_string(),
                Ok(None) => "(empty)".into(),
                Err(e) => format!("(unreadable: {e})"),
            };
            println!(
                "   <- command {} code {:?}: {shown}",
                frame.cmd, frame.retcode
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [ip, gateway, cid] = args.as_slice() else {
        eprintln!("usage: <gateway key on stdin> | gateway-probe <ip> <gateway id> <cid>");
        std::process::exit(2);
    };
    let mut key = String::new();
    if std::io::stdin().read_to_string(&mut key).is_err() {
        eprintln!("no key on stdin");
        std::process::exit(2);
    }
    let Ok(key) = <[u8; 16]>::try_from(key.trim().as_bytes()) else {
        eprintln!("a local key has 16 characters");
        std::process::exit(2);
    };
    let Ok(address) = format!("{ip}:{PORT}").parse() else {
        eprintln!("not an IP address: {ip}");
        std::process::exit(2);
    };
    let mut stream = match TcpStream::connect_timeout(&address, Duration::from_secs(5)) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("gateway unreachable on {PORT}: {e}");
            std::process::exit(1);
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut codec = Codec::new(Version::V33, key);
    let mut buf = Vec::new();
    let questions = [
        (
            "1. the gateway's own state (a gateway keeps it to itself)",
            json!({"gwId": gateway, "devId": gateway, "uid": gateway, "t": now().to_string()}),
        ),
        (
            "2. the sub-device's state (as the driver asks it)",
            json!({"cid": cid, "t": now().to_string()}),
        ),
    ];
    for (label, question) in questions {
        println!("{label}");
        let frame = codec.encode(cmd::DP_QUERY, question.to_string().as_bytes());
        if let Err(e) = stream.write_all(&frame) {
            eprintln!("cannot send: {e}");
            std::process::exit(1);
        }
        listen(&mut stream, &codec, &mut buf, Duration::from_secs(4));
    }
    println!("3. what the gateway says on its own (10 s)");
    listen(&mut stream, &codec, &mut buf, Duration::from_secs(10));
    println!("done: no order was sent.");
}
