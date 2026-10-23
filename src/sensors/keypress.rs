use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyCommand {
    Copy,
    Paste,
    Screenshot,
}

pub struct KeypressSensor {
    last_copy: std::time::Instant,
    last_paste: std::time::Instant,
    last_screenshot: std::time::Instant,
}

impl KeypressSensor {
    pub fn new() -> Self {
        let past = std::time::Instant::now() - std::time::Duration::from_secs(10);
        Self {
            last_copy: past,
            last_paste: past,
            last_screenshot: past,
        }
    }

    pub fn poll(&mut self) -> Option<KeyCommand> {
        let now = std::time::Instant::now();

        unsafe {
            let ctrl = GetAsyncKeyState(0x11); // VK_CONTROL
            if ctrl & (0x8000u16 as i16) != 0 {
                let c = GetAsyncKeyState(0x43); // 'C'
                if c & (0x8000u16 as i16) != 0
                    && now.duration_since(self.last_copy).as_millis() > 500
                {
                    self.last_copy = now;
                    return Some(KeyCommand::Copy);
                }

                let v = GetAsyncKeyState(0x56); // 'V'
                if v & (0x8000u16 as i16) != 0
                    && now.duration_since(self.last_paste).as_millis() > 500
                {
                    self.last_paste = now;
                    return Some(KeyCommand::Paste);
                }
            }

            // PrtSc
            let prtsc = GetAsyncKeyState(0x2C);
            if prtsc & (0x8000u16 as i16) != 0
                && now.duration_since(self.last_screenshot).as_millis() > 1000
            {
                self.last_screenshot = now;
                return Some(KeyCommand::Screenshot);
            }

            // Win+Shift+S
            let lwin = GetAsyncKeyState(0x5B);
            let shift = GetAsyncKeyState(0x10);
            let s = GetAsyncKeyState(0x53);
            if lwin & (0x8000u16 as i16) != 0
                && shift & (0x8000u16 as i16) != 0
                && s & (0x8000u16 as i16) != 0
                && now.duration_since(self.last_screenshot).as_millis() > 1000
            {
                self.last_screenshot = now;
                return Some(KeyCommand::Screenshot);
            }
        }

        None
    }
}
