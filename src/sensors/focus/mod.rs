// focus sensors per compositor. niri is the verified target; Hyprland
// has a socket2 parser, everything else falls back to "no focus info".

pub mod hyprland;
pub mod niri;
pub mod fallback;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the pet needs about "the window the user is looking at".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FocusWindow {
    pub app: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FocusEvent {
    App(FocusWindow),
    Lost,
}

/// Shared focus state: the focus source writes, the 2.5s state poll reads.
#[derive(Debug, Default)]
pub struct Cache {
    pub window: FocusWindow,
}

pub type SharedCache = Arc<Mutex<Cache>>;

pub fn new_cache() -> SharedCache {
    Arc::new(Mutex::new(Cache::default()))
}

impl Cache {
    pub fn apply(&mut self, event: &FocusEvent) {
        match event {
            FocusEvent::App(w) => *self = Cache { window: w.clone() },
            FocusEvent::Lost => *self = Cache::default(),
        }
    }
}

/// A focus source streams events into the cache until its socket dies
/// (then it returns, and run_forever reconnects).
pub trait FocusSource: Send {
    fn watch(self: Box<Self>, cache: &SharedCache);
}

/// Reconnect loop: keep respawning sources; never gives up, never poisoned.
pub fn run_forever(
    mut spawn: impl FnMut() -> Box<dyn FocusSource>,
    cache: &SharedCache,
    backoff: Duration,
) {
    loop {
        spawn().watch(cache);
        std::thread::sleep(backoff);
    }
}

/// Which source to use, picked purely from env — unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub enum Which {
    Niri,
    Hyprland,
    Fallback,
}

pub fn detect_from(env: &HashMap<String, String>) -> Which {
    fn runtime_dir(env: &HashMap<String, String>) -> PathBuf {
        env.get("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_default()
    }
    if env.contains_key("NIRI_SOCKET") {
        return Which::Niri;
    }
    if env.contains_key("HYPRLAND_INSTANCE_SIGNATURE") {
        return Which::Hyprland;
    }
    if env.contains_key("SWAYSOCK") {
        // no sway adapter (untestable here); fallback still compiles the pet
        return Which::Fallback;
    }
    let _ = runtime_dir(env);
    Which::Fallback
}

pub fn detect() -> Box<dyn FocusSource> {
    let env: HashMap<String, String> = std::env::vars().collect();
    match detect_from(&env) {
        Which::Niri => Box::new(niri::Niri::from_env(&env)),
        Which::Hyprland => Box::new(hyprland::Hyprland::from_env(&env)),
        Which::Fallback => Box::new(fallback::Fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// stub source whose socket closes after 2 events keeps the watch alive
    /// afterwards (run_forever respawns; no poisoned lock).
    #[test]
    fn reconnect_keeps_watch_alive() {
        let cache = new_cache();
        let spawns = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        struct Source(Arc<AtomicBool>, usize);

        impl FocusSource for Source {
            fn watch(self: Box<Self>, cache: &SharedCache) {
                let Source(stop, generation) = *self;
                cache.lock().unwrap_or_else(|e| e.into_inner()).apply(&FocusEvent::App(FocusWindow {
                    app: format!("gen{generation}"),
                    title: "window".into(),
                }));
                if generation == 1 {
                    // "socket closes after the 2nd event"
                    stop.store(true, Ordering::SeqCst);
                }
            }
        }

        let spawns_t = spawns.clone();
        let stop_t = stop.clone();
        let cache_t = cache.clone();
        let thread = std::thread::spawn(move || {
            run_forever(
                move || {
                    let n = spawns_t.fetch_add(1, Ordering::SeqCst);
                    Box::new(Source(stop_t.clone(), n as usize))
                },
                &cache_t,
                Duration::from_millis(10),
            )
        });

        // The 2nd generation's event must land, proving respawn + no lock poisoning.
        for _ in 0..100 {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let expect = {
            let c = cache.lock().unwrap();
            c.window.app.clone()
        };
        assert_eq!(expect, "gen1");
        assert!(spawns.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn env_detection() {
        let mut env = HashMap::new();
        assert_eq!(detect_from(&env), Which::Fallback);
        env.insert("SWAYSOCK".into(), "/run/x.sock".into());
        assert_eq!(detect_from(&env), Which::Fallback);
        env.insert("HYPRLAND_INSTANCE_SIGNATURE".into(), "sig1".into());
        assert_eq!(detect_from(&env), Which::Hyprland);
        env.insert("NIRI_SOCKET".into(), "/run/niri.sock".into());
        assert_eq!(detect_from(&env), Which::Niri);
    }
}
