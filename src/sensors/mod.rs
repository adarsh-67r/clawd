pub mod state;
#[cfg(not(windows))]
pub mod proc;
#[cfg(not(windows))]
pub mod battery;
pub mod mpris;
#[cfg(not(windows))]
pub mod clipboard;
#[cfg(not(windows))]
pub mod focus;

#[cfg(windows)]
pub mod system_state;
#[cfg(windows)]
pub mod keypress;

#[cfg(windows)]
pub use keypress::{KeyCommand, KeypressSensor};
#[cfg(windows)]
pub use system_state::poll_system_state;

#[cfg(not(windows))]
pub mod linux;

#[cfg(not(windows))]
pub use linux::{KeyCommand, KeypressSensor, poll_system_state};

#[cfg(all(not(windows), feature = "host-x11"))]
pub mod x_chords;
#[cfg(all(not(windows), feature = "host-x11"))]
pub mod x_cursor;
