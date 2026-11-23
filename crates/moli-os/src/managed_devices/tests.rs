use super::*;
use md5::{Digest as _, Md5};
use moli_runtime::HubOptions;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A data directory and its configuration (fictional).
fn house(drivers: &str) -> (PathBuf, Config) {
    let dir = std::env::temp_dir().join(format!(
        "moli-devices-added-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("moli.toml"),
        format!(
            "[server]\ndata_dir = {}\n{drivers}",
            toml::Value::String(dir.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let config = Config::load(&dir.join("moli.toml")).unwrap();
    (dir, config)
}

fn hub(dir: &Path) -> Hub {
    Hub::new(HubOptions {
        labels_path: Some(dir.join("labels.toml")),
        secrets: Some((
            dir.join("secrets.enc"),
            moli_runtime::MasterKey::new("ab".repeat(32)),
        )),
        ..HubOptions::default()
    })
    .unwrap()
}

/// A Meross MSS315 as the protocol's descriptions give it, with the
/// package's fixtures: it answers a message its `key` signed, `ERROR` to
/// any other, and counts the orders (`SET`) it gets.
struct Plug {
    port: u16,
    orders: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Plug {
    async fn start(key: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let orders = Arc::new(AtomicUsize::new(0));
        let counted = orders.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buf = [0u8; 2048];
                let body = loop {
                    match tcp.read(&mut buf).await {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                    let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map_or(0, |l| l.trim().parse().unwrap());
                    if request.len() >= end + 4 + length {
                        break Some(request[end + 4..end + 4 + length].to_vec());
                    }
                };
                let Some(body) = body else { continue };
                let message: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let header = &message["header"];
                let signed = Md5::digest(
                    format!(
                        "{}{key}{}",
                        header["messageId"].as_str().unwrap(),
                        header["timestamp"].as_u64().unwrap()
                    )
                    .as_bytes(),
                )
                .iter()
                .fold(String::new(), |mut s, b| {
                    use std::fmt::Write as _;
                    let _ = write!(s, "{b:02x}");
                    s
                });
                if header["method"] != "GET" {
                    counted.fetch_add(1, Ordering::Relaxed);
                }
                let fixture = |name: &str| -> serde_json::Value {
                    let path = format!(
                        "{}/../../integrations/meross-mss310/fixtures/{name}.json",
                        env!("CARGO_MANIFEST_DIR")
                    );
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
                };
                let answer = if header["sign"].as_str() == Some(signed.as_str()) {
                    match header["namespace"].as_str().unwrap() {
                        "Appliance.System.All" => {
                            let mut all = fixture("all");
                            all["payload"]["all"]["system"]["hardware"]["type"] = "mss315".into();
                            all
                        }
                        "Appliance.Control.Electricity" => fixture("electricity"),
                        "Appliance.Control.ConsumptionX" => fixture("consumption"),
                        _ => serde_json::json!({"header": {"method": "GETACK"},
                            "payload": {"ability": {}}}),
                    }
                } else {
                    serde_json::json!({"header": {"method": "ERROR"},
                        "payload": {"error": {"code": 5001, "detail": "sign error"}}})
                };
                let answer = answer.to_string();
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = tcp.write_all(answer.as_bytes()).await;
            }
        });
        Self { port, orders, task }
    }
}

const IDENTITY: &str = "00000000000000000000000000000001";
const LOOPBACK: &str = "127.0.0.1";

fn probe(host: &str, key: Option<&str>) -> DeviceProbe {
    DeviceProbe {
        host: host.into(),
        key: key.map(str::to_owned),
    }
}

fn install(identity: &str, key: Option<&str>) -> DeviceInstall {
    DeviceInstall {
        host: LOOPBACK.into(),
        identity: identity.into(),
        name: "Lampe".into(),
        room: Some("Bureau".into()),
        key: key.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_plug_without_a_key_is_added_named_placed_and_kept() {
    let plug = Plug::start("").await;
    let (dir, config) = house("");
    let hub = hub(&dir);
    let cancel = CancellationToken::new();
    let setup = Manager::at_port(hub.clone(), &config, cancel.clone(), plug.port);

    let preview = setup.probe(probe(LOOPBACK, None)).await.unwrap();
    assert_eq!(preview.status, "recognized");
    assert_eq!(preview.profile.as_deref(), Some("meross-mss310"));
    assert_eq!(preview.model.as_deref(), Some("mss315"));
    assert_eq!(preview.identity.as_deref(), Some(IDENTITY));
    assert_eq!(preview.key, Some("empty"));

    let added = setup
        .install(install(IDENTITY, None), "a person".into())
        .await
        .unwrap();
    assert_eq!(added.instance, "meross-mss310-1");
    assert_eq!(added.device, format!("meross-mss310-1:{IDENTITY}"));
    assert!(added.labelled);
    let (_, label) = hub
        .describe(&DeviceId::from(added.device.as_str()))
        .unwrap();
    assert_eq!(label.name.as_deref(), Some("Lampe"));
    assert_eq!(label.room.as_deref(), Some("Bureau"));
    assert_eq!(
        plug.orders.load(Ordering::Relaxed),
        0,
        "adding orders nothing"
    );

    // Kept: read back as it was written, without a key to look for.
    let (listed, editable) = read_store(&dir.join(FILE));
    assert!(editable);
    assert_eq!(listed.len(), 1);
    assert!(listed[0].empty_secret);
    assert_eq!(listed[0].profile, "meross-mss310");

    // The same device again: already in the house.
    let again = setup.probe(probe(LOOPBACK, None)).await.unwrap();
    assert_eq!(again.existing_instance.as_deref(), Some("meross-mss310-1"));
    let refused = setup
        .install(install(IDENTITY, None), "a person".into())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "duplicate_device");

    cancel.cancel();
    setup.shutdown().await;
    plug.task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_bound_plug_takes_the_key_the_person_types_and_it_goes_to_the_vault() {
    let plug = Plug::start("fictional-account-key").await;
    let (dir, config) = house("");
    let hub = hub(&dir);
    let cancel = CancellationToken::new();
    let setup = Manager::at_port(hub.clone(), &config, cancel.clone(), plug.port);

    let locked = setup.probe(probe(LOOPBACK, None)).await.unwrap();
    assert_eq!(locked.status, "needs_key");
    assert_eq!(locked.manufacturer.as_deref(), Some("Meross"));
    let wrong = setup
        .probe(probe(LOOPBACK, Some("not-the-key")))
        .await
        .unwrap();
    assert_eq!(wrong.status, "needs_key");
    let given = setup
        .probe(probe(LOOPBACK, Some("fictional-account-key")))
        .await
        .unwrap();
    assert_eq!(given.key, Some("given"));

    let added = setup
        .install(
            install(IDENTITY, Some("fictional-account-key")),
            "a person".into(),
        )
        .await
        .unwrap();
    let instance = InstanceId::from(added.instance.as_str());
    assert_eq!(
        hub.secret(&instance, "key").as_deref(),
        Some("fictional-account-key")
    );
    let file = std::fs::read_to_string(dir.join(FILE)).unwrap();
    assert!(!file.contains("fictional-account-key"), "never in the file");

    // The next plug of that account: the key the house now holds.
    let (_, config) = house("");
    let next = Manager::at_port(hub.clone(), &config, cancel.clone(), plug.port);
    let preview = next.probe(probe(LOOPBACK, None)).await;
    // (Its registry is another file: it does not know the first one.)
    assert_eq!(preview.unwrap().status, "needs_key");
    let known = setup.probe(probe(LOOPBACK, None)).await.unwrap();
    assert_eq!(known.key, Some("known"));

    cancel.cancel();
    setup.shutdown().await;
    plug.task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn what_the_person_did_not_see_is_not_added() {
    let plug = Plug::start("").await;
    let (dir, config) = house("");
    let hub = hub(&dir);
    let cancel = CancellationToken::new();
    let setup = Manager::at_port(hub, &config, cancel.clone(), plug.port);
    let other = setup
        .install(
            install("00000000000000000000000000000002", None),
            "a person".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(other.code, "identity_changed");
    assert!(!dir.join(FILE).exists());
    let public = setup.probe(probe("8.8.8.8", None)).await.unwrap_err();
    assert_eq!(public.code, "invalid_host");
    let named = setup.probe(probe("plug.local", None)).await.unwrap_err();
    assert_eq!(named.code, "invalid_host");
    cancel.cancel();
    plug.task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_file_moli_did_not_write_is_never_rewritten() {
    let (dir, config) = house(
        "[[driver]]\nid = \"meross-mss310-1\"\nkind = \"profile\"\n[driver.options]\nprofile = \"meross-mss310\"\nhost = \"192.168.1.20\"\n",
    );
    let path = dir.join(FILE);
    assert_eq!(read_store(&path), (Vec::new(), true));
    std::fs::write(&path, "{ not json").unwrap();
    assert!(!read_store(&path).1);
    // Taken by the hand-written configuration, or not a recognized profile:
    // left out, and adding paused.
    std::fs::write(
        &path,
        r#"{"version":1,"devices":[
            {"instance":"meross-mss310-1","profile":"meross-mss310","host":"192.168.1.21"},
            {"instance":"meteo-2","profile":"open-meteo","host":"192.168.1.22"},
            {"instance":"meross-mss310-2","profile":"meross-mss310","host":"192.168.1.23"}]}"#,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let manager = Manager::new(Hub::new(HubOptions::default()).unwrap(), &config, cancel);
    let state = manager.0.state.try_lock().unwrap();
    assert!(!state.editable);
    assert_eq!(state.devices.len(), 1);
    assert_eq!(state.devices[0].instance, "meross-mss310-2");
    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_private_addresses_written_plainly() {
    assert!(private_host("192.168.1.20").is_some());
    assert!(private_host("10.0.0.7").is_some());
    assert!(private_host("192.168.001.020").is_none());
    assert!(private_host("1.1.1.1").is_none());
    assert!(private_host("192.168.1.255x").is_none());
}

/// A configured device that moved leaves its configured address free:
/// another device there is not « already in the house ».
#[tokio::test]
async fn an_address_a_followed_device_left_is_free() {
    let plug = Plug::start("").await;
    let (dir, config) = house(
        "[[driver]]\nid = \"meross-multi-9\"\nkind = \"profile\"\n[driver.options]\nprofile = \"meross-multiprise\"\nhost = \"127.0.0.1\"\n",
    );
    let hub = hub(&dir);
    let cancel = CancellationToken::new();
    let setup = Manager::at_port(hub.clone(), &config, cancel.clone(), plug.port);
    // Still there, as far as Moli knows: the address is taken.
    let taken = setup.probe(probe(LOOPBACK, None)).await.unwrap();
    assert_eq!(taken.existing_instance.as_deref(), Some("meross-multi-9"));
    // Followed elsewhere: free.
    hub.remember_address(
        &InstanceId::from("meross-multi-9"),
        moli_runtime::KnownAddress {
            configured: LOOPBACK.into(),
            host: "192.168.1.9".into(),
            mac: None,
            identity: None,
        },
    )
    .await
    .unwrap();
    let free = setup.probe(probe(LOOPBACK, None)).await.unwrap();
    assert_eq!(free.status, "recognized");
    assert_eq!(free.existing_instance, None);
    cancel.cancel();
    plug.task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
