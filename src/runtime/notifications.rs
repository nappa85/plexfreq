//! Best-effort retained notifications: two coalesced slots, no delivery retries.
use crate::rt::{self, Task};
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use zbus::{zvariant::Value, Connection, MessageStream};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const DEADLINE: Duration = Duration::from_secs(2);
const THROTTLE: Duration = Duration::from_secs(300);
const TEXT_LIMIT: usize = 1024;
const BODY_LIMIT: usize = 4096;

type Admission = Option<(String, Instant)>;

pub(super) struct Notifications {
    slots: [watch::Sender<Admission>; 2],
    task: Task,
    #[cfg(test)]
    completed: watch::Receiver<usize>,
}

impl Notifications {
    pub(super) fn new() -> Self {
        Self::start(Connection::session)
    }

    fn start<C, F>(connect: C) -> Self
    where
        C: Fn() -> F + Send + Sync + 'static,
        F: std::future::Future<Output = zbus::Result<Connection>> + Send + 'static,
    {
        let (a, ar) = watch::channel(None);
        let (b, br) = watch::channel(None);
        #[cfg(test)]
        let (progress, completed) = watch::channel(0);
        Self {
            slots: [a, b],
            task: rt::spawn(worker(
                [ar, br],
                connect,
                #[cfg(test)]
                progress,
            )),
            #[cfg(test)]
            completed,
        }
    }

    /// Only bounded memory operations; never waits for D-Bus or the worker.
    pub(super) fn send(&self, radio: bool, text: &str) {
        let Some(text) = safe_text(text) else {
            return;
        };
        admit(&self.slots[usize::from(radio)], text, Instant::now());
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        self.task.abort();
        self.task.join(Some(Duration::from_millis(200)));
    }
}

fn admit(slot: &watch::Sender<Admission>, text: String, now: Instant) {
    slot.send_if_modified(|last| {
        if last
            .as_ref()
            .is_some_and(|(old, when)| old == &text && now.duration_since(*when) < THROTTLE)
        {
            return false;
        }
        *last = Some((text, now));
        true
    });
}

fn safe_text(text: &str) -> Option<String> {
    // Reject oversized input rather than inspecting only a prefix that could
    // hide a credential suffix. This also bounds work on the caller's thread.
    if text.is_empty() || text.len() > TEXT_LIMIT {
        return None;
    }
    let lower = text.to_lowercase();
    if [
        "://",
        "www.",
        "x-plex-token",
        "token:",
        "bearer ",
        "authorization:",
        "password:",
        "secret:",
        "api_key:",
        "apikey:",
        "credential:",
        "%2f",
        "&#",
        "\\",
        "@",
        "=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.chars().any(|c| c.is_control())
        || text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .any(|word| {
                word.len() >= 20
                    && (word.bytes().any(|c| c.is_ascii_digit())
                        || word.contains('_')
                        || word.contains('-'))
            })
    {
        return None;
    }
    Some(text.to_owned())
}

fn body(text: &str, markup: bool) -> String {
    let mut result = String::new();
    let mut tag = false;
    for c in text.chars() {
        if !markup {
            if c == '<' {
                tag = true;
                continue;
            }
            if c == '>' {
                tag = false;
                continue;
            }
            if tag {
                continue;
            }
        }
        let escaped = match c {
            '&' if markup => "&amp;",
            '<' if markup => "&lt;",
            '>' if markup => "&gt;",
            _ => "",
        };
        let size = if escaped.is_empty() {
            c.len_utf8()
        } else {
            escaped.len()
        };
        if result.len() + size > BODY_LIMIT {
            break;
        }
        if escaped.is_empty() {
            result.push(c);
        } else {
            result.push_str(escaped);
        }
    }
    result
}

#[derive(Default)]
struct Slot {
    id: u32,
    ambiguous: bool,
}

struct Bus {
    connection: Connection,
    signals: futures_util::stream::Select<MessageStream, MessageStream>,
    owner: String,
    generation: u64,
    slots: [Slot; 2],
    desktop: &'static str,
}

impl Bus {
    async fn new(connection: Connection) -> zbus::Result<Self> {
        // Sailjail Notifications.permission grants TALK/broadcast access to
        // this service. xdg-dbus-proxy permits AddMatch and exposes owner
        // changes for visible names. Do not subscribe to unrelated traffic.
        // The unique sender is also checked before accepting a close signal.
        let closed = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SERVICE)?
            .path(PATH)?
            .interface(SERVICE)?
            .member("NotificationClosed")?
            .build();
        let owners = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .path("/org/freedesktop/DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, SERVICE)?
            .build();
        let signals = futures_util::stream::select(
            MessageStream::for_match_rule(owners, &connection, Some(64)).await?,
            MessageStream::for_match_rule(closed, &connection, Some(64)).await?,
        );
        let sailfish = rt::unblock(|| {
            std::fs::read_to_string("/etc/os-release")
                .unwrap_or_default()
                .lines()
                .any(|line| line == "ID=sailfishos" || line == "ID=\"sailfishos\"")
        })
        .await;
        Ok(Self {
            connection,
            signals,
            owner: String::new(),
            generation: 0,
            slots: Default::default(),
            desktop: if sailfish {
                "harbour-plexfreq"
            } else {
                "plexfreq"
            },
        })
    }

    fn owner(&mut self, owner: String) {
        if self.owner != owner {
            self.owner = owner;
            self.generation = self.generation.wrapping_add(1);
            self.slots = Default::default();
        }
    }

    fn signal(&mut self, message: zbus::Message) -> Option<u32> {
        let h = message.header();
        let sender = h.sender().map(|v| v.as_str());
        let interface = h.interface().map(|v| v.as_str());
        let member = h.member().map(|v| v.as_str());
        let path = h.path().map(|v| v.as_str());
        if sender == Some("org.freedesktop.DBus")
            && interface == Some("org.freedesktop.DBus")
            && path == Some("/org/freedesktop/DBus")
            && member == Some("NameOwnerChanged")
        {
            if let Ok((name, _, new)) = message.body().deserialize::<(String, String, String)>() {
                if name == SERVICE {
                    self.owner(new);
                }
            }
        } else if sender == Some(self.owner.as_str())
            && interface == Some(SERVICE)
            && path == Some(PATH)
            && member == Some("NotificationClosed")
        {
            if let Ok((id, _)) = message.body().deserialize::<(u32, u32)>() {
                for slot in &mut self.slots {
                    if slot.id == id {
                        slot.id = 0;
                    }
                }
                return Some(id);
            }
        }
        None
    }

    async fn deliver(
        &mut self,
        slot: usize,
        text: &str,
        diagnostic: &mut Diagnostic,
        setup_deadline: tokio::time::Instant,
    ) {
        let connection = self.connection.clone();
        let generation_before_setup = self.generation;
        let setup = tokio::time::timeout_at(setup_deadline, async {
            let dbus = zbus::fdo::DBusProxy::new(&connection).await?;
            // GetNameOwner does not activate on-demand desktop services. This
            // is setup only: no Notify has been sent if activation fails.
            if !dbus.name_has_owner(SERVICE.try_into().unwrap()).await? {
                dbus.start_service_by_name(SERVICE.try_into().unwrap(), 0)
                    .await?;
            }
            let owner = dbus.get_name_owner(SERVICE.try_into().unwrap()).await?;
            let proxy = zbus::Proxy::new(&connection, owner.to_string(), PATH, SERVICE).await?;
            let caps: Vec<String> = proxy.call("GetCapabilities", &()).await?;
            Ok::<_, zbus::Error>((owner.to_string(), caps, proxy))
        })
        .await;
        let Ok(Ok((owner, caps, proxy))) = setup else {
            diagnostic.failure();
            return;
        };
        // Consume already queued changes before adopting the resolved owner.
        for _ in 0..128 {
            match self.signals.next().now_or_never() {
                Some(Some(Ok(message))) => {
                    self.signal(message);
                }
                Some(_) => {
                    diagnostic.failure();
                    return;
                }
                None => break,
            }
        }
        if tokio::time::Instant::now() >= setup_deadline {
            diagnostic.failure();
            return;
        }
        if self.generation != generation_before_setup && self.owner != owner {
            return;
        }
        self.owner(owner);
        if self.slots[slot].ambiguous && self.slots[slot].id == 0 {
            return;
        }
        let generation = self.generation;
        let id = self.slots[slot].id;
        let body = body(text, caps.iter().any(|c| c == "body-markup"));
        let hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("transient", Value::from(false)),
            ("suppress-sound", Value::from(true)),
            ("urgency", Value::from(1u8)),
            ("x-nemo-owner", Value::from("harbour-plexfreq")),
            ("desktop-entry", Value::from(self.desktop)),
        ]);
        let args = (
            "PlexFreq",
            id,
            "harbour-plexfreq",
            "PlexFreq",
            body,
            Vec::<String>::new(),
            hints,
            0i32,
        );
        let call = tokio::time::timeout(DEADLINE, proxy.call::<_, _, u32>("Notify", &args));
        tokio::pin!(call);
        // A server may dismiss a creation before replying with its ID. Keep
        // a bounded set for this call only, so the reply cannot resurrect it.
        let mut closed = Vec::new();
        let mut close_overflow = false;
        let notify_deadline = tokio::time::Instant::now() + DEADLINE;
        loop {
            // Preserve the deadline even if signals are continuously ready.
            if tokio::time::Instant::now() >= notify_deadline {
                if generation == self.generation {
                    self.slots[slot].ambiguous = true;
                    diagnostic.failure();
                }
                return;
            }
            tokio::select! {
                biased;
                signal = self.signals.next() => match signal {
                    Some(Ok(message)) => {
                        if let Some(closed_id) = self.signal(message) {
                            if closed.len() < 64 { closed.push(closed_id); }
                            else { close_overflow = true; }
                        }
                    },
                    _ => { self.slots[slot].ambiguous = true; diagnostic.failure(); return; }
                },
                result = &mut call => {
                    if generation != self.generation { return; }
                    match result {
                        Ok(Ok(returned)) if returned != 0 => {
                            // A dismissed replaces_id may cause the server to
                            // allocate a new ID. Adopt that ID unless it too
                            // was closed while the call was pending.
                            if close_overflow {
                                self.slots[slot].ambiguous = true;
                            } else if !closed.contains(&returned) {
                                self.slots[slot].id = returned;
                            }
                        },
                        _ => { self.slots[slot].ambiguous = true; diagnostic.failure(); },
                    }
                    return;
                }
            }
        }
    }
}

use futures_util::FutureExt;

#[derive(Default)]
struct Diagnostic(Option<Instant>);

impl Diagnostic {
    fn failure(&mut self) {
        let now = Instant::now();
        if self
            .0
            .is_none_or(|last| now.duration_since(last) >= THROTTLE)
        {
            self.0 = Some(now);
            crate::diagnostics::event(format_args!("notification delivery unavailable"));
        }
    }
}

async fn worker<C, F>(
    mut receivers: [watch::Receiver<Admission>; 2],
    connect: C,
    #[cfg(test)] progress: watch::Sender<usize>,
) where
    C: Fn() -> F,
    F: std::future::Future<Output = zbus::Result<Connection>>,
{
    let mut bus: Option<Bus> = None;
    let mut diagnostic = Diagnostic::default();
    loop {
        let [a, b] = &mut receivers;
        let slot = tokio::select! {
            result = a.changed() => { if result.is_err() { return; } 0 },
            result = b.changed() => { if result.is_err() { return; } 1 },
            signal = async { match bus.as_mut() {
                Some(bus) => bus.signals.next().await,
                None => std::future::pending().await,
            }} => {
                match signal {
                    Some(Ok(message)) => { bus.as_mut().unwrap().signal(message); },
                    // A lost bus connection cannot prove that retained IDs or
                    // ambiguous creations disappeared. Stop rather than risk
                    // creating duplicates on the same service after reconnect.
                    _ => { diagnostic.failure(); return; },
                }
                continue;
            }
        };
        let text = receivers[slot]
            .borrow_and_update()
            .as_ref()
            .map(|(text, _)| text.clone());
        let Some(text) = text else {
            continue;
        };
        let setup_deadline = tokio::time::Instant::now() + DEADLINE;
        if bus.is_none() {
            bus =
                tokio::time::timeout_at(setup_deadline, async { Bus::new(connect().await?).await })
                    .await
                    .ok()
                    .and_then(Result::ok);
        }
        if let Some(bus) = &mut bus {
            bus.deliver(slot, &text, &mut diagnostic, setup_deadline)
                .await;
        } else {
            diagnostic.failure();
        }
        #[cfg(test)]
        progress.send_modify(|count| *count += 1);
    }
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
