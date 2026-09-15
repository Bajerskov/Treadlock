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
    connected: bool,
}

impl Input {
    pub fn new() -> Input {
        Input {
            keys: Keys::default(),
            #[cfg(target_os = "linux")]
            pads: evdev::discover(),
            pad_state: PadState::default(),
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
    pub fn controls(&self) -> Controls {
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
            if p.steer.abs() > 0.01 {
                c.steer = p.steer;
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
                        // Sticks rest near centre; a deadzone stops drift from
                        // steering the car on its own.
                        let v = normalise(&self.steer, event.value) * 2.0 - 1.0;
                        state.steer = if v.abs() < 0.12 { 0.0 } else { v };
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
                            state.connected = true;
                        }
                        BTN_EAST => {
                            state.handbrake = pressed;
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
