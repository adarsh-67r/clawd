// niri focus source — in-process IPC client.
// Talks the newline-JSON protocol of niri-ipc directly over $NIRI_SOCKET;
// no `niri` subprocess, no helper. Tracks the windows map, keeps the cache
// fed with the focused window's app_id/title.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value as Json;

use super::{Cache, FocusEvent, FocusSource, FocusWindow, SharedCache};

pub struct Niri {
    socket: PathBuf,
}

impl Niri {
    pub fn from_env(env: &HashMap<String, String>) -> Niri {
        Niri {
            socket: env
                .get("NIRI_SOCKET")
                .map(PathBuf::from)
                .unwrap_or_default(),
        }
    }

    /// Connect, request the event stream, feed events into the cache.
    /// Returns when the connection dies (caller reconnects).
    pub fn stream(&self, cache: &SharedCache) -> std::io::Result<()> {
        let mut sock = UnixStream::connect(&self.socket)?;
        // Unit variants serialize as a bare JSON string.
        sock.write_all(b"\"EventStream\"\n")?;
        let reader = BufReader::new(sock);

        // id → (app_id, title); full state comes with the first WindowsChanged.
        let mut windows: HashMap<u64, FocusWindow> = HashMap::new();
        let mut focused_id: Option<u64> = None;

        for line in reader.lines() {
            let line = line?;
            // one Reply line ({"Ok":..}/{"Err":..}) precedes the event stream
            if line.starts_with("{\"Ok\"") || line.starts_with("{\"Err\"") {
                continue;
            }
            let Ok(json) = serde_json::from_str::<Json>(&line) else { continue };
            apply(&mut windows, &mut focused_id, &json, cache);
        }
        Ok(())
    }
}

impl FocusSource for Niri {
    fn watch(self: Box<Self>, cache: &SharedCache) {
        let _ = self.stream(cache);
    }
}

/// Applies one niri event to the window map + cache. Returns true when the
/// cached focused window may have changed.
pub fn apply(
    windows: &mut HashMap<u64, FocusWindow>,
    focused_id: &mut Option<u64>,
    json: &Json,
    cache: &SharedCache,
) -> bool {
    let sync_cache = |windows: &HashMap<u64, FocusWindow>, focused_id: Option<u64>| {
        cache.lock().unwrap().window = windows
            .get(&focused_id.unwrap_or(u64::MAX))
            .cloned()
            .unwrap_or_default();
    };

    let Some(name) = json.as_object().and_then(|o| {
        o.keys().next().cloned()
    }) else {
        return false;
    };
    let payload = json.get(&name);

    match (name.as_str(), payload) {
        ("WindowFocusChanged", Some(Json::Object(o))) => {
            let new_id = match o.get("id") {
                Some(Json::Number(n)) => Some(n.as_u64().unwrap_or(0)),
                _ => None,
            };
            if new_id == *focused_id {
                return false;
            }
            *focused_id = new_id;
            sync_cache(windows, *focused_id);
            true
        }
        ("WindowsChanged", Some(payload)) => {
            // older niri sends a bare list; the wire shape sampled live at
            // 26.07 wraps it: {"windows": [...]}
            let list = match payload {
                Json::Array(list) => Some(list),
                Json::Object(o) => o.get("windows").and_then(Json::as_array),
                _ => None,
            };
            if let Some(list) = list {
                windows.clear();
                for w in list.iter() {
                    if let Ok(win) = serde_json::from_value::<NiriWindow>(w.clone()) {
                        if win.is_focused {
                            *focused_id = Some(win.id);
                        }
                        windows.insert(win.id, win.into());
                    }
                }
                sync_cache(windows, *focused_id);
                return true;
            }
            false
        }
        ("WindowOpenedOrChanged", Some(Json::Object(o))) => {
            if let Some(w) = o.get("window") {
                if let Ok(win) = serde_json::from_value::<NiriWindow>(w.clone()) {
                    let focus = FocusWindow {
                        app: win.app_id.clone().unwrap_or_default(),
                        title: win.title.clone().unwrap_or_default(),
                    };
                    let is_for_focused = (focused_id.is_none() && win.is_focused)
                        || Some(win.id) == *focused_id;
                    if win.is_focused {
                        *focused_id = Some(win.id);
                    }
                    windows.insert(win.id, focus);
                    if is_for_focused {
                        sync_cache(windows, *focused_id);
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

impl From<NiriWindow> for FocusWindow {
    fn from(w: NiriWindow) -> FocusWindow {
        FocusWindow {
            app: w.app_id.unwrap_or_default(),
            title: w.title.unwrap_or_default(),
        }
    }
}

#[derive(Deserialize)]
struct NiriWindow {
    id: u64,
    #[serde(rename = "app_id")]
    app_id: Option<String>,
    title: Option<String>,
    #[serde(rename = "is_focused", default)]
    is_focused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn cache() -> SharedCache {
        Arc::new(Mutex::new(Cache::default()))
    }

    #[test]
    fn parses_full_state_then_focus_change() {
        let mut windows = HashMap::new();
        let mut focused = None;
        let cache = cache();

        // shape sampled live from `niri msg event-stream` at 26.04
        let state = serde_json::json!({"WindowsChanged": [
            {"id":3,"title":"OC | Greeting","app_id":"kitty","pid":37,"workspace_id":1,
             "is_focused":false,"is_floating":false,"is_urgent":false},
            {"id":11,"title":"Claude Code","app_id":"kitty","pid":53,"workspace_id":1,
             "is_focused":true,"is_floating":false,"is_urgent":false}
        ]});
        assert!(apply(&mut windows, &mut focused, &state, &cache));
        assert_eq!(windows.len(), 2);
        assert_eq!(focused, Some(11));
        assert_eq!(cache.lock().unwrap().window.title, "Claude Code");
        assert_eq!(cache.lock().unwrap().window.app, "kitty");

    }

    #[test]
    fn parses_wrapped_windows_changed_object_state() {
        // newer niri wraps the list in an object: {"windows": [...]}
        let mut windows = HashMap::new();
        let mut focused = None;
        let cache = cache();
        let state = serde_json::json!({"WindowsChanged": {"windows": [
            {"id":21,"title":"OC | Greeting","app_id":"kitty","pid":88187,
             "workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false}
        ]}});
        assert!(apply(&mut windows, &mut focused, &state, &cache));
        assert_eq!(cache.lock().unwrap().window.title, "OC | Greeting");
    }

    #[test]
    fn switches_focus_between_windows() {
        let mut windows = HashMap::new();
        let mut focused = None;
        let cache = cache();
        let state = serde_json::json!({"WindowsChanged": [
            {"id":3,"title":"OC | Greeting","app_id":"kitty","pid":37,"workspace_id":1,
             "is_focused":false,"is_floating":false,"is_urgent":false},
            {"id":11,"title":"Claude Code","app_id":"kitty","pid":53,"workspace_id":1,
             "is_focused":true,"is_floating":false,"is_urgent":false}
        ]});
        apply(&mut windows, &mut focused, &state, &cache);

        // switch focus to nothing
        let lost = serde_json::json!({"WindowFocusChanged": {"id": null}});
        assert!(apply(&mut windows, &mut focused, &lost, &cache));
        assert_eq!(focused, None);
        assert_eq!(cache.lock().unwrap().window.title, "");

        // switch to another known window
        let switches = serde_json::json!({"WindowFocusChanged": {"id": 3}});
        assert!(apply(&mut windows, &mut focused, &switches, &cache));
        let w = cache.lock().unwrap().window.clone();
        assert_eq!(w.app, "kitty");
        assert_eq!(w.title, "OC | Greeting");

        // late title change on the focused window reaches the cache ("Claude
        // Code" appears in a terminal title on focus)
        let rename = serde_json::json!({"WindowOpenedOrChanged": {"window":
            {"id":3,"title":"Claude Code","app_id":"kitty","pid":37,"workspace_id":1,
             "is_focused":true,"is_floating":false,"is_urgent":false}}});
        assert!(apply(&mut windows, &mut focused, &rename, &cache));
        assert_eq!(cache.lock().unwrap().window.title, "Claude Code");

        // unknown events are ignored
        let unknown = serde_json::json!({"CastsChanged": []});
        assert!(!apply(&mut windows, &mut focused, &unknown, &cache));
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// connects to the real compositor for ~2s and prints the cache.
    /// Run: cargo test -- --ignored live
    #[test]
    #[ignore]
    fn live_stream_updates_cache() {
        let env: HashMap<String, String> = std::env::vars().collect();
        let niri = Niri::from_env(&env);
        let cache = Arc::new(Mutex::new(Cache::default()));
        let c = cache.clone();
        let t = std::thread::spawn(move || { let _ = niri.stream(&c); });
        std::thread::sleep(std::time::Duration::from_secs(2));
        let w = cache.lock().unwrap().window.clone();
        println!("LIVE CACHE: app={:?} title={:?}", w.app, w.title);
        assert!(!w.title.is_empty(), "no focused window reached the cache");
        // the stream thread never exits; let it die with the test process
    }
}
