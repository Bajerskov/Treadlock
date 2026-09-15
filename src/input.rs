//! Input from keyboard and gamepad.
//!
//! Gamepads are read straight from Linux evdev rather than through a library,
//! because the usual crates pull in libudev. Talking to /dev/input directly
//! keeps the dependency list short, which suits a fixed-hardware console.

use crate::vehicle::Controls;

#[derive(Default)]
pub struct Keys {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub boost: bool,
    pub handbrake: bool,
    pub fire: bool,
}

pub struct Input {
    pub keys: Keys,
    #[cfg(target_os = "linux")]
    pads: Vec<evdev::Pad>,
    pad_state: PadState,
    menu_latch: MenuLatch,
}

#[derive(Default, Clone, Copy)]
struct PadState {
    steer: f32,
    throttle: f32,
    brake: f32,
    boost: bool,
    handbrake: bool,
    fire: bool,
    respawn: bool,
    /// Raw vertical stick and d-pad, for driving the menu only.
    menu_y: f32,
    menu_accept: bool,
    menu_back: bool,
    connected: bool,
}

/// Turn a raw stick position into steering, applying the player's settings.
///
/// Deadzone first and then rescaled, so the stick still reaches full lock at
/// full deflection: subtracting a deadzone without rescaling quietly costs the
/// player the last of their steering, which feels like a broken controller.
pub fn shape_steer(raw: f32, pad: &crate::settings::Pad) -> f32 {
    let raw = raw.clamp(-1.0, 1.0);
    let magnitude = raw.abs();
    if magnitude <= pad.deadzone {
        return 0.0;
    }
    let live = (magnitude - pad.deadzone) / (1.0 - pad.deadzone).max(1e-3);
    // A curve above 1 is gentler near the centre and unchanged at the stops,
    // which is what makes a very fast car steerable on a short stick.
    let shaped = live.powf(pad.steer_curve) * pad.steer_sensitivity;
    let signed = shaped.min(1.0) * raw.signum();
    if pad.invert_steer {
        -signed
    } else {
        signed
    }
}

impl Input {
    pub fn new() -> Input {
        Input {
            keys: Keys::default(),
            #[cfg(target_os = "linux")]
            pads: evdev::discover(),
            pad_state: PadState::default(),
            menu_latch: MenuLatch::default(),
        }
    }

    pub fn gamepad_count(&self) -> usize {
        #[cfg(target_os = "linux")]
        {
            self.pads.len()
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }

    /// Drain pending gamepad events. Call once per frame.
    pub fn poll(&mut self) {
        #[cfg(target_os = "linux")]
        for pad in &mut self.pads {
            pad.poll(&mut self.pad_state);
        }
    }

    /// Combine keyboard and pad into one control set. The pad wins whenever it
    /// is actually being pushed, so both can be used without fighting.
    pub fn controls(&self, pad: &crate::settings::Pad) -> Controls {
        let key_steer =
            (self.keys.right as i32 as f32) - (self.keys.left as i32 as f32);
        let mut c = Controls {
            throttle: self.keys.up as i32 as f32,
            brake: self.keys.down as i32 as f32,
            steer: key_steer,
            boost: self.keys.boost,
            handbrake: self.keys.handbrake,
            fire: self.keys.fire,
        };

        if self.pad_state.connected {
            let p = self.pad_state;
            let steer = shape_steer(p.steer, pad);
            if steer != 0.0 {
                c.steer = steer;
            }
            c.throttle = c.throttle.max(p.throttle);
            c.brake = c.brake.max(p.brake);
            c.boost |= p.boost;
            c.handbrake |= p.handbrake;
            c.fire |= p.fire;
        }
        c
    }

    pub fn take_respawn(&mut self) -> bool {
        let r = self.pad_state.respawn;
        self.pad_state.respawn = false;
        r
    }

    /// The next menu action the pad is asking for, if any.
    ///
    /// Edge triggered, and the stick has to return near centre before it
    /// repeats: held on a menu, a raw stick would run the cursor off the end of
    /// the list before the player's thumb was off it.
    pub fn take_menu_action(&mut self) -> Option<crate::menu::Action> {
        let p = self.pad_state;
        if !p.connected {
            return None;
        }

        // Buttons first: they are already edges.
        if p.menu_accept && !self.menu_latch.accept {
            self.menu_latch.accept = true;
            return Some(crate::menu::Action::Accept);
        }
        if !p.menu_accept {
            self.menu_latch.accept = false;
        }
        if p.menu_back && !self.menu_latch.back {
            self.menu_latch.back = true;
            return Some(crate::menu::Action::Back);
        }
        if !p.menu_back {
            self.menu_latch.back = false;
        }

        const PUSH: f32 = 0.55;
        const RELEASE: f32 = 0.30;
        let vertical = p.menu_y;
        let horizontal = p.steer;

        if self.menu_latch.vertical && vertical.abs() < RELEASE {
            self.menu_latch.vertical = false;
        }
        if !self.menu_latch.vertical && vertical.abs() > PUSH {
            self.menu_latch.vertical = true;
            // Sticks report down as positive, which is also how a menu counts.
            return Some(if vertical > 0.0 {
                crate::menu::Action::Down
            } else {
                crate::menu::Action::Up
            });
        }
        if self.menu_latch.horizontal && horizontal.abs() < RELEASE {
            self.menu_latch.horizontal = false;
        }
        if !self.menu_latch.horizontal && horizontal.abs() > PUSH {
            self.menu_latch.horizontal = true;
            return Some(if horizontal > 0.0 {
                crate::menu::Action::Right
            } else {
                crate::menu::Action::Left
            });
        }
        None
    }
}

/// Which pad inputs are currently held, so a menu sees each push once.
#[derive(Default)]
struct MenuLatch {
    vertical: bool,
    horizontal: bool,
    accept: bool,
    back: bool,
}


#[cfg(target_os = "linux")]
mod evdev {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    use super::PadState;

    // Linux input event codes.
    const EV_KEY: u16 = 0x01;
    const EV_ABS: u16 = 0x03;
    const ABS_X: u16 = 0x00;
    const ABS_Y: u16 = 0x01;
    /// The d-pad reports as a hat axis rather than as buttons.
    const ABS_HAT0X: u16 = 0x10;
    const ABS_HAT0Y: u16 = 0x11;
    const ABS_Z: u16 = 0x02;
    const ABS_RZ: u16 = 0x05;
    const BTN_SOUTH: u16 = 0x130;
    const BTN_EAST: u16 = 0x131;
    /// The X button on an Xbox pad: fire.
    const BTN_WEST: u16 = 0x134;
    const BTN_START: u16 = 0x13b;

    /// `struct input_event` on 64-bit Linux: two 64-bit timeval fields, then
    /// type, code and value.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct InputEvent {
        tv_sec: i64,
        tv_usec: i64,
        kind: u16,
        code: u16,
        value: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct AbsInfo {
        value: i32,
        minimum: i32,
        maximum: i32,
        fuzz: i32,
        flat: i32,
        resolution: i32,
    }

    /// Reconstruct the _IOC macro from the kernel headers.
    fn ioc(dir: u64, ty: u64, nr: u64, size: u64) -> u64 {
        (dir << 30) | (size << 16) | (ty << 8) | nr
    }

    pub struct Pad {
        file: File,
        // Per-axis range, so sticks and triggers normalise correctly whatever
        // the device reports.
        steer: AbsInfo,
        left_trigger: AbsInfo,
        right_trigger: AbsInfo,
    }

    /// Open every event device that advertises a gamepad button.
    pub fn discover() -> Vec<Pad> {
        let mut pads = Vec::new();
        let Ok(entries) = std::fs::read_dir("/dev/input") else {
            return pads;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with("event") {
                continue;
            }
            let Ok(file) = File::open(entry.path()) else { continue };
            if !has_gamepad_button(&file) {
                continue;
            }
            // Non-blocking, so a quiet pad never stalls the frame.
            unsafe {
                let fd = file.as_raw_fd();
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            let steer = abs_info(&file, ABS_X).unwrap_or(AbsInfo { minimum: -32768, maximum: 32767, ..Default::default() });
            let left_trigger = abs_info(&file, ABS_Z).unwrap_or(AbsInfo { minimum: 0, maximum: 255, ..Default::default() });
            let right_trigger = abs_info(&file, ABS_RZ).unwrap_or(AbsInfo { minimum: 0, maximum: 255, ..Default::default() });
            pads.push(Pad { file, steer, left_trigger, right_trigger });
        }
        pads
    }

    fn has_gamepad_button(file: &File) -> bool {
        // EVIOCGBIT(EV_KEY, len) returns a bitmap of supported key codes.
        const BITS: usize = 0x300 / 8;
        let mut bitmap = [0u8; BITS];
        let request = ioc(2, b'E' as u64, 0x20 + EV_KEY as u64, BITS as u64);
        let ok = unsafe {
            libc::ioctl(file.as_raw_fd(), request as libc::c_ulong, bitmap.as_mut_ptr()) >= 0
        };
        if !ok {
            return false;
        }
        let bit = BTN_SOUTH as usize;
        bitmap[bit / 8] & (1 << (bit % 8)) != 0
    }

    fn abs_info(file: &File, axis: u16) -> Option<AbsInfo> {
        let mut info = AbsInfo::default();
        let request = ioc(2, b'E' as u64, 0x40 + axis as u64, std::mem::size_of::<AbsInfo>() as u64);
        let ok = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                request as libc::c_ulong,
                &mut info as *mut AbsInfo,
            ) >= 0
        };
        // A zero range means the axis is not really present.
        if ok && info.maximum != info.minimum {
            Some(info)
        } else {
            None
        }
    }

    /// Map a raw axis reading to 0..1 across its reported range.
    fn normalise(info: &AbsInfo, value: i32) -> f32 {
        let span = (info.maximum - info.minimum) as f32;
        if span.abs() < 1.0 {
            return 0.0;
        }
        ((value - info.minimum) as f32 / span).clamp(0.0, 1.0)
    }

    impl Pad {
        pub fn poll(&mut self, state: &mut PadState) {
            use std::io::Read;
            let mut buffer = [0u8; std::mem::size_of::<InputEvent>() * 64];
            loop {
                let read = match (&self.file).read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => n,
                    // WouldBlock simply means no events are pending.
                    Err(_) => break,
                };
                let count = read / std::mem::size_of::<InputEvent>();
                for i in 0..count {
                    let offset = i * std::mem::size_of::<InputEvent>();
                    let event: InputEvent = unsafe {
                        std::ptr::read_unaligned(buffer.as_ptr().add(offset) as *const InputEvent)
                    };
                    self.apply(event, state);
                }
                if count < 64 {
                    break;
                }
            }
        }

        fn apply(&self, event: InputEvent, state: &mut PadState) {
            match event.kind {
                EV_ABS => match event.code {
                    ABS_X => {
                        // Stored raw. The deadzone and the response curve are
                        // player settings and are applied in `controls`, where
                        // they can be changed without reopening the device.
                        state.steer = normalise(&self.steer, event.value) * 2.0 - 1.0;
                        state.connected = true;
                    }
                    ABS_Y => {
                        // Only the menu reads this; the car is not steered
                        // with the vertical axis.
                        state.menu_y = normalise(&self.steer, event.value) * 2.0 - 1.0;
                        state.connected = true;
                    }
                    // The d-pad is a hat, and reports -1, 0 or 1 directly.
                    ABS_HAT0X => {
                        if event.value != 0 {
                            state.steer = event.value.signum() as f32;
                        }
                        state.connected = true;
                    }
                    ABS_HAT0Y => {
                        if event.value != 0 {
                            state.menu_y = event.value.signum() as f32;
                        }
                        state.connected = true;
                    }
                    ABS_Z => {
                        state.brake = normalise(&self.left_trigger, event.value);
                        state.connected = true;
                    }
                    ABS_RZ => {
                        state.throttle = normalise(&self.right_trigger, event.value);
                        state.connected = true;
                    }
                    _ => {}
                },
                EV_KEY => {
                    let pressed = event.value != 0;
                    match event.code {
                        BTN_SOUTH => {
                            state.boost = pressed;
                            // The same button accepts in the menu, where there
                            // is nothing to boost.
                            state.menu_accept = pressed;
                            state.connected = true;
                        }
                        BTN_EAST => {
                            state.handbrake = pressed;
                            state.menu_back = pressed;
                            state.connected = true;
                        }
                        BTN_WEST => {
                            state.fire = pressed;
                            state.connected = true;
                        }
                        BTN_START => {
                            if pressed {
                                state.respawn = true;
                            }
                            state.connected = true;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Pad;

    /// A deadzone that is not rescaled quietly costs the player their last bit
    /// of lock, which feels like a broken stick rather than a setting.
    #[test]
    fn full_deflection_still_reaches_full_lock() {
        for deadzone in [0.0f32, 0.1, 0.25, 0.45] {
            let pad = Pad { deadzone, steer_curve: 1.0, steer_sensitivity: 1.0, ..Pad::default() };
            let full = shape_steer(1.0, &pad);
            assert!(
                (full - 1.0).abs() < 1e-5,
                "deadzone {deadzone} left full lock at {full}"
            );
            assert!((shape_steer(-1.0, &pad) + 1.0).abs() < 1e-5);
        }
    }

    /// Inside the deadzone is exactly nothing, and just outside it is not.
    #[test]
    fn the_deadzone_silences_drift_and_nothing_more() {
        let pad = Pad { deadzone: 0.2, ..Pad::default() };
        for drift in [0.0f32, 0.05, 0.15, 0.199] {
            assert_eq!(shape_steer(drift, &pad), 0.0, "{drift} got through the deadzone");
            assert_eq!(shape_steer(-drift, &pad), 0.0);
        }
        assert!(shape_steer(0.6, &pad) > 0.0, "the stick is dead outside the deadzone");
    }

    /// More stick is always more steering, whatever the curve. A curve that
    /// doubles back would make the car uncontrollable in a way no player could
    /// diagnose.
    #[test]
    fn steering_rises_with_the_stick() {
        for curve in [1.0f32, 1.6, 3.0] {
            let pad = Pad { deadzone: 0.1, steer_curve: curve, ..Pad::default() };
            let mut previous = -1.0;
            let mut raw = 0.0;
            while raw <= 1.0 {
                let steer = shape_steer(raw, &pad);
                assert!(steer >= previous - 1e-6, "curve {curve} doubled back at {raw}");
                assert!((0.0..=1.0).contains(&steer), "curve {curve} gave {steer} at {raw}");
                previous = steer;
                raw += 0.01;
            }
        }
    }

    /// The curve has to do what its name says: gentler near the centre, and
    /// unchanged at the stops.
    #[test]
    fn a_higher_curve_is_gentler_near_the_centre() {
        let linear = Pad { deadzone: 0.0, steer_curve: 1.0, ..Pad::default() };
        let soft = Pad { deadzone: 0.0, steer_curve: 2.5, ..Pad::default() };
        assert!(
            shape_steer(0.4, &soft) < shape_steer(0.4, &linear),
            "a higher curve was not gentler near the centre"
        );
        assert!((shape_steer(1.0, &soft) - shape_steer(1.0, &linear)).abs() < 1e-5);
    }

    /// Sensitivity scales, and never past full lock.
    #[test]
    fn sensitivity_scales_without_overrunning_the_lock() {
        let pad = Pad { deadzone: 0.0, steer_curve: 1.0, steer_sensitivity: 2.0, ..Pad::default() };
        assert!((shape_steer(0.3, &pad) - 0.6).abs() < 1e-5);
        assert!(shape_steer(0.9, &pad) <= 1.0, "high sensitivity ran past full lock");
    }

    #[test]
    fn inverting_flips_the_sign_and_nothing_else() {
        let normal = Pad { invert_steer: false, ..Pad::default() };
        let inverted = Pad { invert_steer: true, ..Pad::default() };
        for raw in [-1.0f32, -0.4, 0.4, 1.0] {
            assert!(
                (shape_steer(raw, &normal) + shape_steer(raw, &inverted)).abs() < 1e-6,
                "inverting changed more than the sign at {raw}"
            );
        }
    }

    /// Whatever the settings, the result is a number the physics can use.
    #[test]
    fn nothing_escapes_the_steering_range() {
        for deadzone in [0.0f32, 0.45] {
            for curve in [1.0f32, 3.0] {
                for sensitivity in [0.4f32, 2.0] {
                    let pad = Pad {
                        deadzone,
                        steer_curve: curve,
                        steer_sensitivity: sensitivity,
                        invert_steer: false,
                        rumble: false,
                    };
                    for raw in [-9.0f32, -1.0, -0.5, 0.0, 0.5, 1.0, 9.0, f32::MAX] {
                        let steer = shape_steer(raw, &pad);
                        assert!(
                            steer.is_finite() && (-1.0..=1.0).contains(&steer),
                            "shape_steer({raw}) = {steer}"
                        );
                    }
                }
            }
        }
    }
}
