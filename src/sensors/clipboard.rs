// clipboard watch on Wayland (data-control via wl-clipboard-rs).
// Linux port note: Wayland has no global key hooks, so the upstream
// "react when the user presses Ctrl+C" becomes "react when the clipboard
// content changes" — classify() decides whether it looks like the pet
// should grab (text/image) or stay put (secrets/blobs/empty).
// This module never reads clipboard CONTENT, only the offer's MIME list.

use wl_clipboard_rs::watch::{ClipboardEvent, ClipboardType, Watcher};
use wl_clipboard_rs::paste::Seat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    Text,
    Image,
    Nothing,
}

/// Image-preferring rule: an image offer trumps any text sibling.
pub fn classify(offer_mimes: &[String]) -> ClipboardKind {
    if offer_mimes.iter().any(|m| m.starts_with("image/")) {
        ClipboardKind::Image
    } else if offer_mimes.iter().any(|m| m.starts_with("text/")) {
        ClipboardKind::Text
    } else {
        ClipboardKind::Nothing
    }
}

/// Blocks forever feeding classified kinds to `on_kind` on every clipboard
/// change. Reconnects with a small backoff if the Wayland connection drops.
/// Content is never fetched or logged; only the MIME list is classified.
pub fn watch(on_kind: impl Fn(ClipboardKind)) {
    loop {
        let mut watcher = match Watcher::new(ClipboardType::Regular, Seat::Unspecified) {
            Ok(w) => w,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        };
        let mut last: Option<(ClipboardKind, std::time::Instant)> = None;
        while let Ok(event) = watcher.next_event() {
            let Some(event) = event else { continue };
            match event {
                ClipboardEvent::Changed { mime_types, .. } => {
                    let kind = classify(&mime_types);
                    // Niri fires Changed twice per copy (offer + selection);
                    // suppress same-kind repeats within a short window.
                    let now = std::time::Instant::now();
                    let is_dup = matches!(&last,
                        Some((k, t)) if *k == kind && now.duration_since(*t)
                            < std::time::Duration::from_millis(700));
                    if !is_dup {
                        on_kind(kind);
                        last = Some((kind, now));
                    }
                }
                ClipboardEvent::Cleared { .. } => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn images_win_over_text() {
        let m = s(&["text/plain", "image/jpeg", "image/png"]);
        assert_eq!(classify(&m), ClipboardKind::Image);
    }

    #[test]
    fn text_plain_charset() {
        let m = s(&["text/plain; charset=utf-8"]);
        assert_eq!(classify(&m), ClipboardKind::Text);
    }

    #[test]
    fn secret_mimes_are_nothing() {
        let m = s(&["application/x-clawd-secret"]);
        assert_eq!(classify(&m), ClipboardKind::Nothing);
    }

    #[test]
    fn empty_offer_is_nothing() {
        assert_eq!(classify(&[]), ClipboardKind::Nothing);
    }

    #[test]
    fn html_flavored_text_counts() {
        let m = s(&["text/html"]);
        assert_eq!(classify(&m), ClipboardKind::Text);
    }
}
