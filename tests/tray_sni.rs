//! The tray as seen from D-Bus: a fake `org.kde.StatusNotifierWatcher` on a
//! private session bus accepts our registration, then we read the exported
//! `org.kde.StatusNotifierItem` properties and its `com.canonical.dbusmenu`
//! menu exactly as a panel would, click things, and check that the right
//! `Remote` requests come out.
//!
//! Needs a private session bus: `dbus-run-session -- cargo test --test
//! tray_sni` (CI does). Without one the test skips, unless
//! `FERRO_REQUIRE_BUS=1` (CI sets it) turns the skip into a failure. It also
//! skips when a real tray host already owns the watcher name, so it can
//! never interfere with a desktop session.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrogram::remote::{MuteRequest, Remote};
use ferrogram::tray::{self, TrayModel};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, Proxy, object_server::SignalEmitter};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
const WAIT: Duration = Duration::from_secs(10);

type Icons = Vec<(i32, i32, Vec<u8>)>;
type ToolTip = (String, Icons, String, String);
type Props = HashMap<String, OwnedValue>;
type Layout = (u32, (i32, Props, Vec<OwnedValue>));

struct FakeWatcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl FakeWatcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        if let Ok(mut items) = self.items.lock() {
            items.push(service.clone());
        }
        let _ = Self::status_notifier_item_registered(&emitter, &service).await;
    }

    async fn register_status_notifier_host(&self, _service: String) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().map(|i| i.clone()).unwrap_or_default()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
}

/// Connect to the session bus and claim the watcher name, or say why not.
async fn start_watcher(items: Arc<Mutex<Vec<String>>>) -> Result<Connection, String> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        return Err("no session bus (run under dbus-run-session)".into());
    }
    let conn = Connection::session().await.map_err(|e| e.to_string())?;
    conn.object_server()
        .at("/StatusNotifierWatcher", FakeWatcher { items })
        .await
        .map_err(|e| e.to_string())?;
    conn.request_name_with_flags(WATCHER, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
        .map_err(|e| format!("a real tray host owns {WATCHER}: {e}"))?;
    Ok(conn)
}

/// A proxy that reads properties live (a panel would follow the change
/// signals; here we want to see the current value right after `set_model`).
async fn uncached<'a>(
    conn: &Connection,
    dest: &'a str,
    path: &'a str,
    iface: &'a str,
) -> Proxy<'a> {
    zbus::proxy::Builder::new(conn)
        .destination(dest)
        .expect("destination")
        .path(path)
        .expect("path")
        .interface(iface)
        .expect("interface")
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .expect("proxy")
}

async fn next(rx: &mut UnboundedReceiver<Remote>) -> Remote {
    match timeout(WAIT, rx.recv()).await {
        Ok(Some(r)) => r,
        other => panic!("no request arrived: {other:?}"),
    }
}

/// `label -> (id, toggle-state)` of the root menu's children. Ids are not
/// stable (the menu is renumbered whenever it changes), so read the layout.
async fn menu_labels(menu: &Proxy<'_>) -> HashMap<String, (i32, Option<i32>)> {
    let layout = menu
        .call_method("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
        .await
        .expect("GetLayout");
    let (_revision, root): Layout = layout.body().deserialize().expect("layout body");
    let ids: Vec<i32> = root
        .2
        .iter()
        .map(|child| {
            let st = zbus::zvariant::Structure::try_from(child.clone()).expect("child struct");
            match st.fields().first() {
                Some(Value::I32(id)) => *id,
                other => panic!("unexpected child id {other:?}"),
            }
        })
        .collect();
    let reply: Vec<(i32, Props)> = menu
        .call("GetGroupProperties", &(ids, Vec::<String>::new()))
        .await
        .expect("GetGroupProperties");
    reply
        .into_iter()
        .filter_map(|(id, props)| {
            let label = String::try_from(props.get("label")?.clone()).ok()?;
            let toggled = props
                .get("toggle-state")
                .and_then(|v| i32::try_from(v.clone()).ok());
            Some((label, (id, toggled)))
        })
        .collect()
}

async fn click(menu: &Proxy<'_>, id: i32) {
    let _: () = menu
        .call("Event", &(id, "clicked", Value::from(0i32), 0u32))
        .await
        .expect("Event");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tray_exports_a_status_notifier_item_and_menu() {
    // First, with nobody on the bus: registering fails, which is what makes
    // the window's close button quit instead of hiding into the void.
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        let probe = Connection::session().await.expect("session bus");
        let owned = zbus::fdo::DBusProxy::new(&probe)
            .await
            .expect("dbus proxy")
            .name_has_owner(WATCHER.try_into().expect("name"))
            .await
            .unwrap_or(true);
        if !owned {
            let sink: tray::Sink = Arc::new(|_| {});
            let none = tray::spawn(TrayModel::default(), sink, 2, Duration::from_millis(50)).await;
            assert!(none.is_err(), "no tray host: spawn must fail");
        }
    }

    let items = Arc::new(Mutex::new(Vec::new()));
    let watcher = match start_watcher(items.clone()).await {
        Ok(c) => c,
        Err(why) => {
            assert!(
                std::env::var_os("FERRO_REQUIRE_BUS").is_none(),
                "bus required but unusable: {why}"
            );
            eprintln!("skipping tray_sni: {why}");
            return;
        }
    };

    let (tx, mut rx) = unbounded_channel();
    let sink: tray::Sink = Arc::new(move |r| {
        let _ = tx.send(r);
    });
    let handle = tray::spawn(TrayModel::default(), sink, 1, Duration::ZERO)
        .await
        .expect("registers with the fake watcher");

    // The panel side: registered under a bus name, with the item path.
    let service = timeout(WAIT, async {
        loop {
            if let Some(s) = items.lock().ok().and_then(|i| i.first().cloned()) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("item registered");
    assert!(service.contains("StatusNotifierItem"), "{service}");

    let client = Connection::session().await.expect("client connection");
    let item = uncached(&client, &service, "/StatusNotifierItem", ITEM_IFACE).await;

    // The item's bus name is requested right after it registers: wait for it.
    timeout(WAIT, async {
        while item.get_property::<String>("Id").await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("item answers on the bus");

    // Idle state.
    assert_eq!(
        item.get_property::<String>("Id").await.unwrap(),
        "dev.ferro.ferrogram"
    );
    assert_eq!(
        item.get_property::<String>("Category").await.unwrap(),
        "Communications"
    );
    assert_eq!(
        item.get_property::<String>("Title").await.unwrap(),
        "Ferrogram"
    );
    assert_eq!(
        item.get_property::<String>("Status").await.unwrap(),
        "Active"
    );
    assert!(
        !item.get_property::<bool>("ItemIsMenu").await.unwrap(),
        "left click must Activate"
    );
    let pixmaps: Icons = item.get_property("IconPixmap").await.unwrap();
    let sizes: Vec<i32> = pixmaps.iter().map(|p| p.0).collect();
    assert_eq!(sizes, vec![22, 32, 48]);
    for (w, h, data) in &pixmaps {
        assert_eq!(w, h);
        assert_eq!(data.len(), (w * h * 4) as usize, "ARGB32");
    }
    let attention: Icons = item.get_property("AttentionIconPixmap").await.unwrap();
    assert_eq!(attention.len(), 3);
    assert_ne!(
        attention[0].2, pixmaps[0].2,
        "attention icon carries the badge"
    );
    let tip: ToolTip = item.get_property("ToolTip").await.unwrap();
    assert_eq!(tip.2, "Ferrogram");

    // Unread messages in unmuted chats: attention + count in the tooltip.
    handle
        .set_model(TrayModel {
            unread: 3,
            window_open: true,
            muted: Some("until re-enabled".into()),
            start_minimized: true,
        })
        .await;
    assert_eq!(
        item.get_property::<String>("Status").await.unwrap(),
        "NeedsAttention"
    );
    let tip: ToolTip = item.get_property("ToolTip").await.unwrap();
    assert_eq!(tip.2, "Ferrogram — 3 unread");
    assert_eq!(tip.3, "Notifications muted until re-enabled");
    handle.set_model(TrayModel::default()).await;
    assert_eq!(
        item.get_property::<String>("Status").await.unwrap(),
        "Active"
    );

    // Left click toggles the window.
    let _: () = item.call("Activate", &(10i32, 20i32)).await.unwrap();
    assert_eq!(next(&mut rx).await, Remote::Toggle);

    // The menu, read the way a panel does, and clicked.
    let menu = uncached(&client, &service, "/MenuBar", MENU_IFACE).await;
    let labels = menu_labels(&menu).await;
    for want in [
        "Show Ferrogram",
        "Mute notifications for 1 hour",
        "Mute notifications until re-enabled",
        "Start minimized to tray",
        "Quit",
    ] {
        assert!(labels.contains_key(want), "menu lacks {want:?}: {labels:?}");
    }
    assert_eq!(labels["Start minimized to tray"].1, Some(0), "unchecked");
    click(&menu, labels["Show Ferrogram"].0).await;
    assert_eq!(next(&mut rx).await, Remote::Toggle);
    click(&menu, labels["Mute notifications for 1 hour"].0).await;
    assert_eq!(next(&mut rx).await, Remote::Mute(MuteRequest::OneHour));
    click(&menu, labels["Mute notifications until re-enabled"].0).await;
    assert_eq!(next(&mut rx).await, Remote::Mute(MuteRequest::UntilEnabled));
    click(&menu, labels["Start minimized to tray"].0).await;
    assert_eq!(next(&mut rx).await, Remote::ToggleStartMinimized);
    click(&menu, labels["Quit"].0).await;
    assert_eq!(next(&mut rx).await, Remote::Quit);

    // The model drives the menu: window open + muted + start minimized.
    handle
        .set_model(TrayModel {
            window_open: true,
            muted: Some("until 15:30".into()),
            start_minimized: true,
            ..TrayModel::default()
        })
        .await;
    let labels = menu_labels(&menu).await;
    assert!(labels.contains_key("Hide Ferrogram"), "{labels:?}");
    assert!(labels.contains_key("Unmute notifications"), "{labels:?}");
    assert_eq!(labels["Start minimized to tray"].1, Some(1), "checked");

    // The panel disappears and comes back: the app must hear about both, so
    // a hidden window can reappear instead of leaving a ghost process.
    watcher.release_name(WATCHER).await.expect("release");
    assert_eq!(next(&mut rx).await, Remote::TrayLost);
    watcher
        .request_name(WATCHER)
        .await
        .expect("watcher returns");
    assert_eq!(next(&mut rx).await, Remote::TrayBack);

    handle.shutdown().await;
}

/// Manual helper (not part of the suite): act as the panel's watcher for
/// `FERRO_FAKE_WATCHER_SECS` seconds so the real app can be run and driven
/// with `busctl --user` under `dbus-run-session`:
/// `cargo test --test tray_sni -- --ignored serve_fake_watcher --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual helper"]
async fn serve_fake_watcher() {
    let items = Arc::new(Mutex::new(Vec::new()));
    let _conn = start_watcher(items.clone()).await.expect("watcher");
    let secs: u64 = std::env::var("FERRO_FAKE_WATCHER_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    eprintln!("fake StatusNotifierWatcher up for {secs}s");
    tokio::time::sleep(Duration::from_secs(secs)).await;
}
