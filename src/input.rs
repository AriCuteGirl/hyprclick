//! The virtual mouse Cursor mode clicks with: a uinput device on Linux,
//! SendInput on Windows. Buttons use the Linux codes (BTN_LEFT = 272...).

#[cfg(target_os = "linux")]
mod imp {
    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};

    pub struct Mouse(VirtualDevice);

    fn uinput_err(e: std::io::Error) -> String {
        format!("/dev/uinput: {e} (are you in the 'input' group, or 'uinput' on NixOS?)")
    }

    impl Mouse {
        pub fn new() -> Result<Self, String> {
            let keys = AttributeSet::from_iter([KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE]);
            // libinput only treats it as a pointer if it has relative axes.
            let axes = AttributeSet::from_iter([RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y]);
            let device = VirtualDevice::builder()
                .map_err(uinput_err)?
                .name("hyprclick virtual mouse")
                .with_keys(&keys)
                .map_err(uinput_err)?
                .with_relative_axes(&axes)
                .map_err(uinput_err)?
                .build()
                .map_err(uinput_err)?;
            Ok(Self(device))
        }

        pub fn button(&mut self, code: u16, down: bool) -> Result<(), String> {
            let ev = InputEvent::new(EventType::KEY.0, code, down as i32);
            self.0.emit(&[ev]).map_err(|e| e.to_string())
        }

        pub fn move_by(&mut self, dx: i32, dy: i32) -> Result<(), String> {
            let rel = |axis: RelativeAxisCode, v: i32| InputEvent::new(EventType::RELATIVE.0, axis.0, v);
            self.0.emit(&[rel(RelativeAxisCode::REL_X, dx), rel(RelativeAxisCode::REL_Y, dy)]).map_err(|e| e.to_string())
        }
    }
}

#[cfg(windows)]
mod imp {
    pub struct Mouse;

    impl Mouse {
        pub fn new() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn button(&mut self, code: u16, down: bool) -> Result<(), String> {
            crate::win::mouse_button(code, down)
        }

        pub fn move_by(&mut self, dx: i32, dy: i32) -> Result<(), String> {
            crate::win::mouse_move(dx, dy)
        }
    }
}

pub use imp::Mouse;
