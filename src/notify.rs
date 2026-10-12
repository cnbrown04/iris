//! Desktop notifications over D-Bus, without notify-send.
//!
//! Shows org.freedesktop.Notifications popups (quickshell is the server on
//! this desktop) and handles the ActionInvoked signal of the `open` action.
//! One listener thread with one match rule reads the signals of every banner
//! this process has shown; the map keeps only `id -> url`, so a never-clicked
//! banner leaves one small entry behind, nothing worse.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use zbus::match_rule::MatchRule;
use zbus::zvariant::Value;
use zbus::blocking::Connection;

const IFACE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const DEST: &str = "org.freedesktop.Notifications";
const APP: &str = "iris";

static URLS: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
static LISTENER: OnceLock<()> = OnceLock::new();

fn urls() -> &'static Mutex<HashMap<u32, String>> {
    URLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Show one banner and return its D-Bus id.
///
/// `url` is opened in the browser when the user clicks the `open` action.
/// `replace` replaces an earlier banner of the same view instead of stacking.
pub fn show(
    summary: &str,
    body: &str,
    replace: u32,
    url: &str,
    timeout_ms: i32,
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    let conn = Connection::session()
        .map_err(|e| format!("no session bus: {e}"))?;
    let actions: Vec<String> = vec!["open".to_string(), "Open".to_string()];
    let mut hints: HashMap<String, Value> = HashMap::new();
    hints.insert("urgency".to_string(), Value::U8(1));
    let reply = conn.call_method(
        Some(DEST),
        PATH,
        Some(IFACE),
        "Notify",
        &(
            APP,
            replace,
            "",
            summary,
            body,
            actions,
            hints,
            timeout_ms,
        ),
    )?;
    let id: u32 = reply.body().deserialize()?;
    urls().lock().unwrap().insert(id, url.to_string());
    ensure_listener();
    Ok(id)
}

fn ensure_listener() {
    LISTENER.get_or_init(start_listener);
}

fn start_listener() {
    let _ = std::thread::Builder::new()
        .name("iris-notify".to_string())
        .spawn(move || {
            let Ok(conn) = Connection::session() else {
                return;
            };
            let Ok(after_iface) = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(IFACE)
            else {
                return;
            };
            let Ok(after_member) = after_iface.member("ActionInvoked") else {
                return;
            };
            let rule = after_member.build();
            let Ok(stream) =
                zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64))
            else {
                return;
            };
            for msg in stream {
                let msg = match msg {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if msg.message_type() != zbus::message::Type::Signal {
                    continue;
                }
                let Ok((id, action_key)) = msg.body().deserialize::<(u32, String)>() else {
                    continue;
                };
                let url = urls().lock().unwrap().remove(&id);
                if let (Some(url), true) = (url, action_key == "open") {
                    let _ = Command::new("xdg-open")
                        .arg(url)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
        });
}
