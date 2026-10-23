// Fallback focus source: no focus info (runtime we don't adapt to);
// just parks the thread so main's spawn loop stays sane.

use crate::sensors::focus::SharedCache;
use super::FocusSource;

pub struct Fallback;

impl FocusSource for Fallback {
    fn watch(self: Box<Self>, _cache: &SharedCache) {
        loop {
            std::thread::park();
        }
    }
}
