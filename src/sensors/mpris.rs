// music via MPRIS on the user bus.
// "Playing wins": any player reporting PlaybackStatus=Playing is the one the
// pet listens to; a paused/stopped player only supplies the title memory.

use std::collections::BTreeMap;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

pub struct Music {
    pub playing: bool,
    pub title: Option<String>,
}

pub fn parse(playback_status: Option<&str>, title: Option<String>) -> Music {
    Music {
        playing: playback_status == Some("Playing"),
        title,
    }
}

/// Reads `xesam:title` out of an MPRIS metadata dict.
pub fn title_from_metadata(metadata: &BTreeMap<String, Value>) -> Option<String> {
    match metadata.get("xesam:title")? {
        Value::Str(s) => Some(s.as_str().to_string()).filter(|t| !t.trim().is_empty()),
        _ => None,
    }
}

/// Music state across all MPRIS players on the user bus.
pub fn read() -> Music {
    let Ok(conn) = Connection::session() else {
        return Music { playing: false, title: None };
    };
    let Ok(names) = zbus::blocking::fdo::DBusProxy::new(&conn)
        .and_then(|p| p.list_names().map_err(zbus::Error::from))
    else {
        return Music { playing: false, title: None };
    };

    let mut fallback: Option<Music> = None;
    for name in names.iter().map(|n| n.as_str()).filter(|n| {
        n.starts_with("org.mpris.MediaPlayer2.") && !n.ends_with(".playerctld")
    }) {
        let Ok(proxy) = zbus::blocking::Proxy::new(&conn, name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player") else {
            continue;
        };
        let Ok(status): Result<String, _> = proxy.get_property("PlaybackStatus") else {
            continue;
        };
        let title = proxy
            .get_property::<std::collections::HashMap<String, Value>>("Metadata")
            .ok()
            .and_then(|m| {
                let tree: BTreeMap<String, Value> = m.into_iter().collect();
                title_from_metadata(&tree)
            });
        let music = parse(Some(&status), title);
        if music.playing {
            return music;
        }
        fallback = fallback.or(Some(music));
    }
    fallback.unwrap_or(Music { playing: false, title: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_with_title() {
        assert!(parse(Some("Playing"), Some("X".into())).playing);
        assert_eq!(parse(Some("Playing"), Some("X".into())).title, Some("X".into()));
    }

    #[test]
    fn paused_is_not_playing_but_keeps_title() {
        let m = parse(Some("Paused"), Some("X".into()));
        assert!(!m.playing);
        assert_eq!(m.title, Some("X".into()));
    }

    #[test]
    fn nothing_playing() {
        let m = parse(None, None);
        assert!(!m.playing);
        assert_eq!(m.title, None);
    }

    #[test]
    fn title_reads_xesam_key_only() {
        let mut map = BTreeMap::new();
        assert_eq!(title_from_metadata(&map), None);
        map.insert("mpris:length".into(), Value::U64(9));
        assert_eq!(title_from_metadata(&map), None);
        map.insert("xesam:title".into(), Value::Str("Track".into()));
        assert_eq!(title_from_metadata(&map), Some("Track".into()));
    }

    #[test]
    fn empty_title_is_none() {
        let mut map = BTreeMap::new();
        map.insert("xesam:title".into(), Value::Str("  ".into()));
        assert_eq!(title_from_metadata(&map), None);
    }
}
