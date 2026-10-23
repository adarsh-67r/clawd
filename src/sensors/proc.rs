// /proc-based Claude detection + config loading (Linux port).
// pet detection scans /proc/*/comm for the claude CLI process; the loader
// resolves clawd.config.json XDG-first, then exe-adjacent (upstream behavior).

use serde_json::Value;
use std::path::PathBuf;

/// Counts processes whose /proc/<pid>/comm matches `comm`.
/// Tolerates pids appearing/disappearing mid-scan (skips unreadable entries).
pub fn count_paths(dir: &str, comm: &str) -> usize {
    let mut count = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path().join("comm")) {
            if text.trim() == comm {
                count += 1;
            }
        }
    }
    count
}

/// The pet's config view: the raw JSON that polls are read from, plus the
/// Linux-port pet command.
pub struct Config {
    pub raw: Value,
    pub claude_command: String,
}

impl Config {
    pub fn parse(raw: Value) -> Config {
        let claude_command = raw
            .get("claudeCommand")
            .and_then(|v| v.as_str())
            .unwrap_or("xdg-terminal-exec claude")
            .to_string();
        Config { raw, claude_command }
    }

    /// XDG-first, then exe-adjacent, upstream fallbacks last; embedded defaults as the final net.
    pub fn load() -> Config {
        let xdg = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_default()
            });
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));

        let candidates = [
            Some(xdg.join("clawd").join("clawd.config.json")),
            exe_dir.as_ref().map(|d| d.join("clawd.config.json")),
            exe_dir.as_ref().map(|d| d.join("..").join("clawd.config.json")),
            Some(PathBuf::from("clawd.config.json")),
        ];

        for candidate in candidates.iter().flatten() {
            if let Ok(content) = std::fs::read_to_string(candidate) {
                if let Ok(raw) = serde_json::from_str(&content) {
                    return Config::parse(raw);
                }
            }
        }

        Config::parse(serde_json::json!({
            "poll": { "systemState": 2500, "cursor": 33, "keypress": 80, "tokens": 60000 },
            "window": { "width": 220, "height": 260, "margin": 24 }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_one_claude_among_decoys() {
        let dir = std::env::temp_dir().join("clawd-proc-test");
        let _ = std::fs::remove_dir_all(&dir);
        for (pid, comm) in [
            ("100", "claude"),
            ("101", "kitty"),
            ("102", "claude"), // second claude — decoy density check below
        ] {
            std::fs::create_dir_all(dir.join(pid)).unwrap();
            std::fs::write(dir.join(pid).join("comm"), format!("{comm}\n")).unwrap();
        }
        // non-pid entries must be ignored
        std::fs::write(dir.join("acpi"), "\n").unwrap();
        assert_eq!(count_paths(dir.to_str().unwrap(), "claude"), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_dir_is_zero() {
        assert_eq!(count_paths("/nonexistent-clawd-test", "claude"), 0);
    }

    #[test]
    fn config_defaults_when_keys_absent() {
        let cfg = Config::parse(serde_json::json!({
            "poll": { "systemState": 2500 }
        }));
        assert_eq!(cfg.claude_command, "xdg-terminal-exec claude");
        assert_eq!(cfg.raw["poll"]["systemState"], 2500);
    }

    #[test]
    fn config_overrides_apply_and_extra_keys_forward_compat() {
        let raw = serde_json::json!({
            "claudeCommand": "claude-dev",
            "someFutureKey": { "x": 1 }
        });
        let cfg = Config::parse(raw);
        assert_eq!(cfg.claude_command, "claude-dev");
    }
}
