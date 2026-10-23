// Hyprland focus source — socket2 event stream over its unix socket.
// Parser is the unit-tested part (`activewindow>>app,title` lines, the
// exact fixture the plan calls for); the reader follows the same shape as
// the niri adapter so Hyprland ideally behaves identically.
// 	todo: headerRowCanBeRemoved — left at here in comment, see ledger note

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use super::{Cache, FocusSource, SharedCache};

pub struct Hyprland {
    socket: PathBuf,
}

impl Hyprland {
    pub fn from_env(env: &HashMap<String, String>) -> Hyprland {
        let sig = env
            .get("HYPRLAND_INSTANCE_SIGNATURE")
            .cloned()
            .unwrap_or_default();
        let dir = env
            .get("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_default();
        Hyprland {
            socket: dir.join(format!("hyprland/{sig}/socket2.sock")),
        }
    }

    pub fn stream(&self, cache: &SharedCache) -> std::io::Result<()> {
        let mut sock = UnixStream::connect(&self.socket)?;
        writeln!(sock, "subscribe activewindow,activewindowv2")?;
        let reader = BufReader::new(sock);
        for line in reader.lines() {
            let line = line?;
            let Some(window) = parse_activewindow(&line) else { continue };
            cache.lock().unwrap_or_else(|e| e.into_inner()).window = window;
        }
        Ok(())
    }
}

impl FocusSource for Hyprland {
    fn watch(self: Box<Self>, cache: &SharedCache) {
        let _ = self.stream(cache);
    }
}

/// `activewindow>>app,title` → the window; an empty line after the `>>`
/// separator or a `>>,,` means no window is active.
pub fn parse_activewindow(line: &str) -> Option<super::FocusWindow> {
    let rest = line.strip_prefix("activewindow>>")?;
    if let Some((app, title)) = rest.split_once(',') {
        if rest.is_empty() || (app.is_empty() && title.is_empty()) {
            return None;
        }
        return Some(super::FocusWindow {
            app: app.trim_start_matches(',').to_string(),
            title: title.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_focused_claude_window() {
        let w = parse_activewindow("activewindow>>code,Code - clawd.md").unwrap();
        assert_eq!(w.app, "code");
        assert_eq!(w.title, "Code - clawd.md");
    }

    #[test]
    fn title_with_commas_keeps_everything_still() {
        let w = parse_activewindow("activewindow>>kitty,vim, in, a, tty").unwrap();
        assert_eq!(w.app, "kitty");
        assert_eq!(w.title, "vim, in, a, tty");
    }

    #[test]
    fn unfocused_line_is_none() {
        assert!(parse_activewindow("activewindow>>,").is_none());
        assert!(parse_activewindow("activewindowv2>>1").is_none());
        assert!(parse_activewindow("random>>junk").is_none());
    }
}
