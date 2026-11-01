//! Desktop notifications over D-Bus, without notify-send.
//!
//! Shows org.freedesktop.Notifications banners (Hokum is the server on this
//! desktop) and reports what happens to them: a button or the banner itself
//! was clicked (ActionInvoked; the banner body is the `default` action), or
//! the banner closed (NotificationClosed).
//!
//! Everything goes through one session connection. Some servers send the
//! signals only to the connection that showed the banner, so listening on a
//! second connection would miss them.
//!
//! URLs open through the desktop portal (org.freedesktop.portal.OpenURI),
//! which runs inside the graphical session. The daemon itself runs from the
//! bare systemd user environment (no WAYLAND_DISPLAY), where a plain
//! `xdg-open` can't start a browser; it's only the fallback.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use zbus::blocking::Connection;
use zbus::match_rule::MatchRule;
use zbus::zvariant::Value;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

const IFACE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const DEST: &str = "org.freedesktop.Notifications";
const APP: &str = "iris";

/// The action key of a click on the banner itself (not on a button).
pub const DEFAULT: &str = "default";

static CONN: OnceLock<Connection> = OnceLock::new();
/// XDG activation tokens the server handed out with a click, by banner id.
static TOKENS: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();

fn conn() -> Result<&'static Connection, Error> {
    if let Some(c) = CONN.get() {
        return Ok(c);
    }
    let c = Connection::session().map_err(|e| format!("no session bus: {e}"))?;
    Ok(CONN.get_or_init(|| c))
}

fn tokens() -> &'static Mutex<HashMap<u32, String>> {
    TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One banner. `actions` are (key, label) pairs; a `DEFAULT` key makes the
/// whole banner clickable and is never drawn as a button.
pub struct Banner<'a> {
    pub summary: &'a str,
    pub body: &'a str,
    /// The id of an earlier banner to replace, or 0.
    pub replace: u32,
    pub actions: &'a [(&'a str, &'a str)],
    /// Milliseconds on screen; 0 keeps it until it's clicked or closed.
    pub timeout_ms: i32,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
}

impl<'a> Banner<'a> {
    /// A plain informational banner.
    pub fn info(summary: &'a str, body: &'a str, timeout_ms: i32) -> Self {
        Self {
            summary,
            body,
            replace: 0,
            actions: &[],
            timeout_ms,
            urgency: 1,
        }
    }
}

/// Show one banner and return its D-Bus id.
pub fn show(b: &Banner) -> Result<u32, Error> {
    let actions: Vec<&str> = b.actions.iter().flat_map(|(k, l)| [*k, *l]).collect();
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("urgency", Value::U8(b.urgency));
    let reply = conn()?.call_method(
        Some(DEST),
        PATH,
        Some(IFACE),
        "Notify",
        &(APP, b.replace, "", b.summary, b.body, actions, hints, b.timeout_ms),
    )?;
    Ok(reply.body().deserialize()?)
}

/// Close a banner (no error when it's already gone).
pub fn close(id: u32) {
    if let Ok(c) = conn() {
        let _ = c.call_method(Some(DEST), PATH, Some(IFACE), "CloseNotification", &(id,));
    }
}

/// Something that happened to one of our banners.
#[derive(Debug, Clone)]
pub enum Event {
    /// A button (or `DEFAULT`: the banner itself) was clicked.
    Action { id: u32, key: String },
    /// The banner went away. Reason 1 expired, 2 dismissed by the user,
    /// 3 closed by a CloseNotification call, 4 other.
    Closed { id: u32, reason: u32 },
}

/// Read the server's signals on a background thread and hand every event to
/// `handler` (on that thread, one at a time; spawn for slow work).
pub fn listen(handler: impl Fn(Event) + Send + 'static) -> Result<(), Error> {
    let c = conn()?.clone();
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(IFACE)?
        .path(PATH)?
        .build();
    let stream = zbus::blocking::MessageIterator::for_match_rule(rule, &c, Some(64))?;
    std::thread::Builder::new()
        .name("iris-notify".to_string())
        .spawn(move || {
            for msg in stream {
                let Ok(msg) = msg else { continue };
                let header = msg.header();
                let Some(member) = header.member() else { continue };
                match member.as_str() {
                    "ActionInvoked" => {
                        if let Ok((id, key)) = msg.body().deserialize::<(u32, String)>() {
                            handler(Event::Action { id, key });
                        }
                    }
                    "ActivationToken" => {
                        if let Ok((id, token)) = msg.body().deserialize::<(u32, String)>() {
                            tokens().lock().unwrap().insert(id, token);
                        }
                    }
                    "NotificationClosed" => {
                        if let Ok((id, reason)) = msg.body().deserialize::<(u32, u32)>() {
                            tokens().lock().unwrap().remove(&id);
                            handler(Event::Closed { id, reason });
                        }
                    }
                    _ => {}
                }
            }
        })?;
    Ok(())
}

/// Open a URL in the default browser. `banner` is the banner that was
/// clicked, if any: its activation token lets the browser raise its window.
pub fn open_url(url: &str, banner: Option<u32>) {
    let token = banner.and_then(|id| tokens().lock().unwrap().remove(&id));
    if let Err(e) = open_portal(url, token.as_deref()) {
        eprintln!("portal open failed ({e}), trying xdg-open");
        let _ = Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn open_portal(url: &str, token: Option<&str>) -> Result<(), Error> {
    let mut opts: HashMap<&str, Value> = HashMap::new();
    if let Some(t) = token {
        opts.insert("activation_token", Value::from(t));
    }
    conn()?.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.OpenURI"),
        "OpenURI",
        &("", url, opts),
    )?;
    Ok(())
}
