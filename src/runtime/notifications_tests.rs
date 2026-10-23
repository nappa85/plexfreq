use super::*;
use crate::mutex_lock;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use zbus::zvariant::OwnedValue;

static TEST_BUS: Mutex<()> = Mutex::new(());

// Platform fixtures temporarily rewrite the process bus environment. Never
// consult it for either side of this fixture, and reap our daemon on unwind.
struct TestBus {
    process: std::process::Child,
    address: String,
    _config: tempfile::TempDir,
    _output: BufReader<std::process::ChildStdout>,
}

impl TestBus {
    fn new() -> Self {
        Self::configured(false)
    }

    fn activatable() -> Self {
        Self::configured(true)
    }

    fn configured(activate: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let services = directory.path().join("services");
        std::fs::create_dir(&services).unwrap();
        let executable = std::env::current_exe().unwrap();
        if activate {
            // The daemon's address pipe is not a logging channel. Redirect
            // the child harness's stdout so shutdown cannot break its summary
            // write after the parent has closed that pipe. Keep stderr visible.
            let script = directory.path().join("activate.sh");
            let executable = executable.to_str().unwrap().replace('\'', "'\\''");
            std::fs::write(
                &script,
                format!("#!/bin/sh\nexec '{executable}' --ignored --exact runtime::notifications::tests::activation_service_child --nocapture >/dev/null\n"),
            ).unwrap();
            std::fs::write(
                services.join(format!("{SERVICE}.service")),
                format!(
                    "[D-BUS Service]\nName={SERVICE}\nExec=/bin/sh \"{}\"\n",
                    script.display()
                ),
            )
            .unwrap();
        }
        std::fs::write(
            directory.path().join("bus.conf"),
            format!(r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
  <servicedir>{}</servicedir>
  <policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy>
</busconfig>"#, services.display()),
        ).unwrap();
        Self::launch(directory)
    }

    fn launch(config: tempfile::TempDir) -> Self {
        assert_eq!(std::env::var("PLEXFREQ_TEST_ISOLATED").as_deref(), Ok("1"));
        let mut command = std::process::Command::new("dbus-daemon");
        command
            .arg("--config-file")
            .arg(config.path().join("bus.conf"));
        let mut process = command
            .args(["--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = BufReader::new(process.stdout.take().unwrap());
        let mut bus = Self {
            process,
            address: String::new(),
            _config: config,
            _output: output,
        };
        bus._output.read_line(&mut bus.address).unwrap();
        bus.address = bus.address.trim().to_owned();
        bus
    }

    fn notifications(&self) -> Notifications {
        let address = self.address.clone();
        Notifications::start(move || {
            let address = address.clone();
            async move {
                zbus::connection::Builder::address(address.as_str())?
                    .build()
                    .await
            }
        })
    }
}

// Invoked by dbus-daemon from the temporary .service file, not by the parent
// test runner. Uses only the address supplied by D-Bus activation itself.
#[test]
#[ignore = "D-Bus activation subprocess entry point"]
fn activation_service_child() {
    let address = std::env::var("DBUS_STARTER_ADDRESS").unwrap();
    rt::block_on(async {
        let connection = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .serve_at(
                PATH,
                Fake {
                    calls: Arc::new(Mutex::new(Vec::new())),
                    delay: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    setup_delay: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    setup_started: watch::channel(0).0,
                    markup: true,
                    live: Mutex::new(Default::default()),
                    apply_gate: Mutex::new(None),
                },
            )
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .build()
            .await
            .unwrap();
        let mut messages = MessageStream::from(&connection);
        // Parent owns the daemon; bus EOF ends the child even if an assertion
        // unwinds the parent. A finite backstop also bounds fixture lifetime.
        let _ = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(Ok(_)) = messages.next().await {}
        })
        .await;
    });
}

#[test]
fn one_event_activates_service_and_delivers_once() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::activatable();
    rt::block_on(async {
        let connection = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let dbus = zbus::fdo::DBusProxy::new(&connection).await.unwrap();
        assert!(!dbus
            .name_has_owner(SERVICE.try_into().unwrap())
            .await
            .unwrap());
        assert!(dbus
            .list_activatable_names()
            .await
            .unwrap()
            .iter()
            .any(|name| name.as_str() == SERVICE));
        let notifications = bus.notifications();
        notifications.send(false, "Activate and deliver");
        completed(&notifications, 1).await;
        // GetNameOwner itself cannot activate; this must now succeed because
        // the admitted event started the real activatable service.
        let owner = dbus
            .get_name_owner(SERVICE.try_into().unwrap())
            .await
            .unwrap();
        let proxy = zbus::Proxy::new(&connection, owner, PATH, SERVICE)
            .await
            .unwrap();
        let count: u32 = proxy.call("DeliveredCount", &()).await.unwrap();
        assert_eq!(count, 1);
        notifications.send(false, "Activate and deliver");
        let count: u32 = proxy.call("DeliveredCount", &()).await.unwrap();
        assert_eq!(count, 1);
        drop(notifications);
    });
}

#[test]
fn dismissed_replacement_adopts_new_live_id() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::new();
    rt::block_on(async {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let service = service(&bus, &calls, &delay, true).await;
        let notifications = bus.notifications();
        notifications.send(false, "First error");
        completed(&notifications, 1).await;
        let interface = service
            .object_server()
            .interface::<_, Fake>(PATH)
            .await
            .unwrap();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *mutex_lock(&interface.get().await.apply_gate) = Some(gate.clone());
        notifications.send(false, "Replacement in flight");
        wait_calls(&calls, 2).await;
        assert_eq!(mutex_lock(&calls)[1].id, 1);
        // The service receives replaces_id=A, but A disappears before Notify
        // applies. Its live-ID model must allocate B, not return dead A.
        dismiss(&service, 1).await;
        gate.add_permits(1);
        completed(&notifications, 2).await;
        *mutex_lock(&interface.get().await.apply_gate) = None;
        assert!(mutex_lock(&interface.get().await.live).contains(&2));
        notifications.send(false, "Replace the returned new ID");
        completed(&notifications, 3).await;
        assert_eq!(mutex_lock(&calls)[2].id, 2, "valid new ID was discarded");
        assert_eq!(
            mutex_lock(&interface.get().await.live).len(),
            1,
            "duplicate retained notification"
        );
        drop(notifications);
        service.release_name(SERVICE).await.unwrap();
    });
}

impl Drop for TestBus {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn completed(
    notifications: &Notifications,
    count: usize,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let mut progress = notifications.completed.clone();
    async move {
        tokio::time::timeout(Duration::from_secs(4), progress.wait_for(|n| *n >= count))
            .await
            .expect("notification attempt did not finish")
            .expect("notification worker stopped");
    }
}

#[derive(Debug)]
struct Payload {
    app: String,
    id: u32,
    icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
    timeout: i32,
}
struct Fake {
    calls: Arc<Mutex<Vec<Payload>>>,
    delay: Arc<std::sync::atomic::AtomicU64>,
    setup_delay: Arc<std::sync::atomic::AtomicU64>,
    setup_started: watch::Sender<usize>,
    markup: bool,
    live: Mutex<std::collections::HashSet<u32>>,
    apply_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Fake {
    fn delivered_count(&self) -> u32 {
        mutex_lock(&self.calls).len() as u32
    }
    async fn get_capabilities(&self) -> Vec<&str> {
        let delay = self.setup_delay.load(std::sync::atomic::Ordering::SeqCst);
        self.setup_started.send_modify(|count| *count += 1);
        tokio::time::sleep(Duration::from_millis(delay)).await;
        if self.markup {
            vec!["body", "body-markup"]
        } else {
            vec!["body"]
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app: String,
        id: u32,
        icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        timeout: i32,
    ) -> u32 {
        // Snapshot before publishing receipt: the test may change the next
        // call's delay as soon as it observes this call in the payload list.
        let delay = self.delay.load(std::sync::atomic::Ordering::SeqCst);
        let gate = mutex_lock(&self.apply_gate).clone();
        let next_id = {
            let mut calls = mutex_lock(&self.calls);
            calls.push(Payload {
                app,
                id,
                icon,
                summary,
                body,
                actions,
                hints,
                timeout,
            });
            calls.len() as u32
        };
        if let Some(gate) = gate {
            gate.acquire().await.unwrap().forget();
        }
        // Match lipstick: a replacement of an ID that no longer exists
        // creates a new notification and returns its new ID.
        let result = {
            let mut live = mutex_lock(&self.live);
            let result = if live.contains(&id) { id } else { next_id };
            live.insert(result);
            result
        };
        tokio::time::sleep(Duration::from_millis(delay)).await;
        result
    }
}

async fn service(
    bus: &TestBus,
    calls: &Arc<Mutex<Vec<Payload>>>,
    delay: &Arc<std::sync::atomic::AtomicU64>,
    markup: bool,
) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            PATH,
            Fake {
                calls: calls.clone(),
                delay: delay.clone(),
                setup_delay: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                setup_started: watch::channel(0).0,
                markup,
                live: Mutex::new(Default::default()),
                apply_gate: Mutex::new(None),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn dismiss(service: &Connection, id: u32) {
    let interface = service
        .object_server()
        .interface::<_, Fake>(PATH)
        .await
        .unwrap();
    mutex_lock(&interface.get().await.live).remove(&id);
    service
        .emit_signal(
            None::<&str>,
            PATH,
            SERVICE,
            "NotificationClosed",
            &(id, 2u32),
        )
        .await
        .unwrap();
}

async fn wait_calls(calls: &Mutex<Vec<Payload>>, count: usize) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if mutex_lock(calls).len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {count} Notify calls, received {}",
            mutex_lock(calls).len()
        )
    });
}

#[test]
fn safety_and_monotonic_admission() {
    assert!(safe_text("The token is invalid").is_some());
    for bad in [
        "https://example.invalid/a",
        "X-Plex-Token=synthetic",
        "synthetic_secret_123456",
        "Bearer abc",
        "abcDEF1234567890123456789",
        "mail@example.invalid",
    ] {
        assert!(safe_text(bad).is_none());
    }
    assert!(safe_text(&"é".repeat(513)).is_none());
    assert_eq!(safe_text(&"é".repeat(512)), Some("é".repeat(512)));
    assert_eq!(
        body("<b>Errore</b> & riprova", true),
        "&lt;b&gt;Errore&lt;/b&gt; &amp; riprova"
    );
    assert_eq!(body("<b>Errore</b> & riprova", false), "Errore & riprova");
    assert!(body(&"&".repeat(1024), true).len() <= BODY_LIMIT);
    let (tx, mut rx) = watch::channel(None);
    let now = Instant::now();
    admit(&tx, "Error".into(), now);
    rx.borrow_and_update();
    admit(&tx, "Error".into(), now + Duration::from_secs(299));
    assert!(!rx.has_changed().unwrap());
    admit(&tx, "Error".into(), now + THROTTLE);
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();
    for n in 0..100 {
        admit(&tx, format!("Error {n}"), now + THROTTLE);
    }
    assert_eq!(rx.borrow_and_update().as_ref().unwrap().0, "Error 99");
}

// One serial scenario owns the well-known name on the wrapper's private bus.
#[test]
fn fake_bus_delivery_lifecycle() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::new();
    assert_eq!(
        std::env::var("PLEXFREQ_TEST_ISOLATED").as_deref(),
        Ok("1"),
        "run with tools/isolated-test.sh"
    );
    rt::block_on(async {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let notifications = bus.notifications();
        notifications.send(false, "Missing service");
        completed(&notifications, 1).await;
        let first = service(&bus, &calls, &delay, true).await;
        notifications.send(false, "Errore <rete> & riprova");
        wait_calls(&calls, 1).await;
        completed(&notifications, 2).await;
        {
            let calls = calls.lock().unwrap();
            let p = &calls[0];
            assert_eq!(
                (&*p.app, &*p.icon, &*p.summary),
                ("PlexFreq", "harbour-plexfreq", "PlexFreq")
            );
            assert_eq!(p.body, "Errore &lt;rete&gt; &amp; riprova");
            assert_eq!(p.timeout, 0);
            assert!(p.actions.is_empty());
            assert!(!bool::try_from(p.hints.get("transient").unwrap()).unwrap());
            assert!(bool::try_from(p.hints.get("suppress-sound").unwrap()).unwrap());
            assert_eq!(u8::try_from(p.hints.get("urgency").unwrap()).unwrap(), 1);
            assert_eq!(
                <&str>::try_from(p.hints.get("x-nemo-owner").unwrap()).unwrap(),
                "harbour-plexfreq"
            );
        }
        notifications.send(false, "Errore <rete> & riprova");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(calls.lock().unwrap().len(), 1);
        notifications.send(false, "https://example.invalid/synthetic");
        notifications.send(true, "X-Plex-Token=synthetic");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(calls.lock().unwrap().len(), 1, "unsafe payload reached bus");
        notifications.send(false, "Different error");
        wait_calls(&calls, 2).await;
        assert_eq!(calls.lock().unwrap()[1].id, 1);
        completed(&notifications, 3).await;
        dismiss(&first, 1).await;
        notifications.send(false, "Different error");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(calls.lock().unwrap().len(), 2);
        notifications.send(false, "After dismissal");
        wait_calls(&calls, 3).await;
        assert_eq!(calls.lock().unwrap()[2].id, 0);
        delay.store(2400, std::sync::atomic::Ordering::SeqCst);
        notifications.send(true, "Radio refresh failed");
        wait_calls(&calls, 4).await;
        for n in 0..100 {
            notifications.send(true, &format!("Radio error {n}"));
        }
        completed(&notifications, 6).await;
        assert_eq!(
            calls.lock().unwrap().len(),
            4,
            "ambiguous create must suppress later creates"
        );
        // A replacement remains possible in the other, known-ID category.
        delay.store(0, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "Known replacement");
        wait_calls(&calls, 5).await;
        assert_eq!(calls.lock().unwrap()[4].id, 3);
        first.release_name(SERVICE).await.unwrap();
        let replacement = service(&bus, &calls, &delay, false).await;
        notifications.send(true, "New owner <b>radio</b>");
        wait_calls(&calls, 6).await;
        assert_eq!(calls.lock().unwrap()[5].id, 0);
        assert_eq!(calls.lock().unwrap()[5].body, "New owner radio");
        delay.store(2400, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "Slow shutdown call");
        wait_calls(&calls, 7).await;
        let start = Instant::now();
        drop(notifications);
        assert!(start.elapsed() < Duration::from_millis(500));
        replacement.release_name(SERVICE).await.unwrap();
        drop(replacement);
    });
}

#[test]
fn setup_timeout_is_definite_non_delivery_and_cancellable() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::new();
    assert_eq!(std::env::var("PLEXFREQ_TEST_ISOLATED").as_deref(), Ok("1"));
    rt::block_on(async {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let setup_delay = Arc::new(std::sync::atomic::AtomicU64::new(2400));
        let (setup_started, mut started) = watch::channel(0);
        let service = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(
                PATH,
                Fake {
                    calls: calls.clone(),
                    delay: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    setup_delay: setup_delay.clone(),
                    setup_started,
                    markup: true,
                    live: Mutex::new(Default::default()),
                    apply_gate: Mutex::new(None),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let notifications = bus.notifications();
        notifications.send(false, "Setup will time out");
        completed(&notifications, 1).await;
        assert!(calls.lock().unwrap().is_empty());
        setup_delay.store(0, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "Setup recovered");
        wait_calls(&calls, 1).await;
        assert_eq!(calls.lock().unwrap()[0].id, 0);
        setup_delay.store(2400, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "Cancel setup");
        tokio::time::timeout(
            Duration::from_secs(4),
            started.wait_for(|count| *count == 3),
        )
        .await
        .expect("cancellable setup did not start")
        .unwrap();
        let start = Instant::now();
        drop(notifications);
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(calls.lock().unwrap().len(), 1);
        service.release_name(SERVICE).await.unwrap();
    });
}

#[test]
fn owner_change_during_pending_reply() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::new();
    assert_eq!(std::env::var("PLEXFREQ_TEST_ISOLATED").as_deref(), Ok("1"));
    rt::block_on(async {
        // Both a reply within our deadline and an ambiguous timeout followed
        // by a late reply must be unable to contaminate the new owner's IDs.
        for delay_ms in [600, 2400] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let delay = Arc::new(std::sync::atomic::AtomicU64::new(delay_ms));
            let old = service(&bus, &calls, &delay, true).await;
            let notifications = bus.notifications();
            notifications.send(true, "Old owner pending");
            wait_calls(&calls, 1).await;
            old.release_name(SERVICE).await.unwrap();
            let immediate = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let new = service(&bus, &calls, &immediate, true).await;
            notifications.send(true, "New owner pending");
            wait_calls(&calls, 2).await;
            assert_eq!(
                calls.lock().unwrap()[1].id,
                0,
                "old reply reused across owners"
            );
            tokio::time::sleep(Duration::from_millis(650)).await;
            // An old-owner close (even with a colliding new-owner ID) must
            // also be ignored. The old service remains alive to send its reply.
            dismiss(&old, 2).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            notifications.send(true, "New owner replacement");
            wait_calls(&calls, 3).await;
            assert_eq!(calls.lock().unwrap()[2].id, 2);
            drop(notifications);
            new.release_name(SERVICE).await.unwrap();
        }
    });
}

#[test]
fn dismissal_before_creation_reply_does_not_resurrect_id() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::new();
    assert_eq!(std::env::var("PLEXFREQ_TEST_ISOLATED").as_deref(), Ok("1"));
    rt::block_on(async {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delay = Arc::new(std::sync::atomic::AtomicU64::new(300));
        let service = service(&bus, &calls, &delay, true).await;
        let notifications = bus.notifications();
        notifications.send(false, "Dismiss while pending");
        wait_calls(&calls, 1).await;
        dismiss(&service, 1).await;
        completed(&notifications, 1).await;
        notifications.send(false, "Dismiss while pending");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(calls.lock().unwrap().len(), 1, "dismissal reset throttle");
        notifications.send(false, "After early dismissal");
        wait_calls(&calls, 2).await;
        assert_eq!(calls.lock().unwrap()[1].id, 0, "dismissed ID resurrected");
        drop(notifications);
        service.release_name(SERVICE).await.unwrap();
    });
}

#[test]
fn fixed_slots_and_latest_ingress_over_real_bus() {
    let _serial = mutex_lock(&TEST_BUS);
    let bus = TestBus::new();
    assert_eq!(std::env::var("PLEXFREQ_TEST_ISOLATED").as_deref(), Ok("1"));
    rt::block_on(async {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let service = service(&bus, &calls, &delay, true).await;
        let notifications = bus.notifications();
        // Deliver >32 distinct texts, alternating slots. Every call after the
        // first two must replace the original category ID, never allocate.
        for n in 0..40 {
            notifications.send(n % 2 != 0, &format!("Error {n}"));
            wait_calls(&calls, n + 1).await;
        }
        {
            let recorded = calls.lock().unwrap();
            for (n, p) in recorded.iter().enumerate() {
                assert_eq!(p.id, if n < 2 { 0 } else { (n % 2 + 1) as u32 });
            }
        }
        delay.store(300, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "Delayed replacement");
        wait_calls(&calls, 41).await;
        for n in 0..100 {
            notifications.send(false, &format!("Coalesced {n}"));
        }
        notifications.send(true, "Other category latest");
        wait_calls(&calls, 43).await;
        completed(&notifications, 43).await;
        {
            let recorded = calls.lock().unwrap();
            assert_eq!(recorded.len(), 43);
            assert!(recorded[41..]
                .iter()
                .any(|p| p.body == "Coalesced 99" && p.id == 1));
            assert!(recorded[41..]
                .iter()
                .any(|p| p.body == "Other category latest" && p.id == 2));
        }
        // A timed-out replacement retains its known ID and can be replaced
        // again by a new event, unlike an ambiguous creation.
        delay.store(2400, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "Ambiguous replacement");
        wait_calls(&calls, 44).await;
        completed(&notifications, 44).await;
        delay.store(0, std::sync::atomic::Ordering::SeqCst);
        notifications.send(false, "After ambiguous replacement");
        wait_calls(&calls, 45).await;
        assert_eq!(calls.lock().unwrap()[44].id, 1);
        drop(notifications);
        service.release_name(SERVICE).await.unwrap();
    });
}
