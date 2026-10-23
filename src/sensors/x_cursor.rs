// X11-only global cursor poll: feeds the pet's eye-follow with root
// coordinates. Wayland has no global cursor poll — there the eyes only
// follow while the pointer hovers the pet (see host/wl.rs Cursor events).

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

pub struct CursorSensor {
    conn: RustConnection,
    root: x11rb::protocol::xproto::Window,
}

impl CursorSensor {
    pub fn new() -> Result<Self, String> {
        let (conn, idx) = RustConnection::connect(None).map_err(|e| format!("x11 connect: {e}"))?;
        let root = conn.setup().roots[idx].root;
        Ok(Self { conn, root })
    }

    /// global cursor position; None while the pointer is gone
    pub fn poll(&mut self) -> Option<(i32, i32)> {
        let rep = self
            .conn
            .query_pointer(self.root)
            .ok()?
            .reply()
            .ok()?;
        if !rep.same_screen {
            return None;
        }
        Some((rep.root_x as i32, rep.root_y as i32))
    }
}
