// SPDX-License-Identifier: GPL-3.0-only
/*
 * qtfblight - QTFB to libblight compatibility layer
 * Copyright (C) 2026 Noa Himesaka
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use crate::blight::{BlightContentType, BlightImageFormat};
use std::ffi::CString;
use std::fs;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DeviceType {
    RM1,
    RM2,
    RMPP,
    RMPPM,
    RMPure,
}

#[derive(Debug, Clone)]
pub struct DeviceProfile {
    pub device_type: DeviceType,
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub format: BlightImageFormat,
    pub color_type: BlightContentType,
    pub touch_device: u16,
    pub pen_device: u16,
    pub button_device: u16,
}

impl DeviceProfile {
    pub fn new(device_type: DeviceType) -> Self {
        match device_type {
            DeviceType::RM1 => DeviceProfile {
                device_type,
                name: "reMarkable 1",
                width: 1404,
                height: 1872,
                format: BlightImageFormat::FormatRGB16,
                color_type: BlightContentType::Monochrome,
                touch_device: 1,
                pen_device: 0,
                button_device: 2,
            },
            DeviceType::RM2 => DeviceProfile {
                device_type,
                name: "reMarkable 2",
                width: 1404,
                height: 1872,
                format: BlightImageFormat::FormatRGBA8888,
                color_type: BlightContentType::Monochrome,
                touch_device: 2,
                pen_device: 1,
                button_device: 0,
            },
            DeviceType::RMPP => DeviceProfile {
                device_type,
                name: "reMarkable Paper Pro",
                width: 1620,
                height: 2160,
                format: BlightImageFormat::FormatRGBA8888,
                color_type: BlightContentType::Color,
                touch_device: 3,
                pen_device: 2,
                button_device: 0,
            },
            DeviceType::RMPPM => DeviceProfile {
                device_type,
                name: "reMarkable Paper Pro Move",
                width: 954,
                height: 1696,
                format: BlightImageFormat::FormatRGBA8888,
                color_type: BlightContentType::Color,
                touch_device: 3,
                pen_device: 2,
                button_device: 0,
            },
            DeviceType::RMPure => DeviceProfile {
                device_type,
                name: "reMarkable Paper Pure",
                width: 1404,
                height: 1872,
                format: BlightImageFormat::FormatRGBA8888,
                color_type: BlightContentType::Monochrome,
                touch_device: 3,
                pen_device: 2,
                button_device: 0,
            },
        }
    }

    pub fn transform_touch(&self, raw_x: i32, raw_y: i32) -> (i32, i32) {
        let fb_w = self.width as i32;
        let fb_h = self.height as i32;
        let (x, y) = match self.device_type {
            DeviceType::RM1 => {
                let x = ((767 - raw_x) * fb_w) / 767;
                let y = ((1023 - raw_y) * fb_h) / 1023;
                (x, y)
            }
            DeviceType::RM2 => {
                let x = (raw_x * fb_w) / 1403;
                let y = ((1871 - raw_y) * fb_h) / 1871;
                (x, y)
            }
            DeviceType::RMPP => {
                let x = (raw_x * fb_w) / 2064;
                let y = (raw_y * fb_h) / 2832;
                (x, y)
            }
            DeviceType::RMPPM => {
                let x = (raw_x * fb_w) / 1248;
                let y = (raw_y * fb_h) / 2208;
                (x, y)
            }
            DeviceType::RMPure => {
                let x = (raw_x * fb_w) / 1776;
                let y = (raw_y * fb_h) / 2400;
                (x, y)
            }
        };
        (x.max(0).min(fb_w - 1), y.max(0).min(fb_h - 1))
    }

    pub fn transform_pen(&self, raw_x: i32, raw_y: i32, raw_pressure: i32) -> (i32, i32, i32) {
        let fb_w = self.width as i32;
        let fb_h = self.height as i32;
        let (x, y, d) = match self.device_type {
            DeviceType::RM1 | DeviceType::RM2 => {
                let x = (raw_y * fb_w) / 15725;
                let y = ((20967 - raw_x) * fb_h) / 20967;
                let d = (raw_pressure * 100) / 4096;
                (x, y, d)
            }
            DeviceType::RMPP => {
                let x = (raw_x * fb_w) / 11180;
                let y = (raw_y * fb_h) / 15340;
                let d = (raw_pressure * 100) / 255;
                (x, y, d)
            }
            DeviceType::RMPPM => {
                let x = (raw_x * fb_w) / 6760;
                let y = (raw_y * fb_h) / 11960;
                let d = (raw_pressure * 100) / 255;
                (x, y, d)
            }
            DeviceType::RMPure => {
                let x = (raw_x * fb_w) / 9620;
                let y = (raw_y * fb_h) / 13000;
                let d = (raw_pressure * 100) / 255;
                (x, y, d)
            }
        };
        (
            x.max(0).min(fb_w - 1),
            y.max(0).min(fb_h - 1),
            d.clamp(0, 100),
        )
    }
}

pub fn detect_device_type() -> DeviceType {
    let files_to_check = [
        "/sys/devices/soc0/machine",
        "/sys/firmware/devicetree/base/model",
        "/proc/device-tree/model",
    ];

    for file_path in &files_to_check {
        if let Ok(content) = fs::read_to_string(file_path) {
            let upper = content.to_uppercase();
            if upper.contains("FERRARI") {
                return DeviceType::RMPP;
            } else if upper.contains("CHIAPPA") {
                return DeviceType::RMPPM;
            } else if upper.contains("TATSU") {
                return DeviceType::RMPure;
            } else if upper.contains("2.0") {
                return DeviceType::RM2;
            }
        }
    }

    // Default to RM1 as a safe fallback
    DeviceType::RM1
}

const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const KEY_POWER: u16 = 116;
const BTN_STYLUS: u16 = 0x14b;
const ABS_MT_SLOT: u16 = 0x2f;

/// EVIOCGVERSION: _IOR('E', 0x01, int)
const EVIOCGVERSION: libc::c_ulong = (2 << 30) | (4 << 16) | ((b'E' as libc::c_ulong) << 8) | 0x01;

/// EVIOCGBIT(ev, len): _IOC(_IOC_READ, 'E', 0x20 + ev, len)
fn eviocgbit(ev_type: u16, len: usize) -> libc::c_ulong {
    (2 << 30)
        | ((len as libc::c_ulong) << 16)
        | ((b'E' as libc::c_ulong) << 8)
        | (0x20 + ev_type as libc::c_ulong)
}

fn test_bit(bit: u16, bits: &[u8]) -> bool {
    let byte = (bit / 8) as usize;
    byte < bits.len() && (bits[byte] >> (bit % 8)) & 1 != 0
}

fn event_bits(fd: libc::c_int, ev_type: u16) -> [u8; 96] {
    // 96 bytes covers KEY_MAX (0x2ff), the largest code space queried here.
    let mut bits = [0u8; 96];
    unsafe { libc::ioctl(fd, eviocgbit(ev_type, bits.len()), bits.as_mut_ptr()) };
    bits
}

fn parse_event_number(name: &str) -> Option<u16> {
    name.strip_prefix("event")?.parse().ok()
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct ProbedInputDevices {
    pub touch: Option<u16>,
    pub pen: Option<u16>,
    pub button: Option<u16>,
}

/// Classifies `/dev/input/event*` nodes with the same capability checks (and
/// the same first-match-wins order) as Oxide's `DeviceSettings`, which is what
/// decides the device numbers the blight display server publishes input
/// under. Static per-model numbers break whenever a unit enumerates its input
/// devices differently, so agreement with the display server has to come from
/// probing, not from a table.
pub fn probe_input_devices() -> ProbedInputDevices {
    let mut probed = ProbedInputDevices::default();

    let Ok(dir) = fs::read_dir("/dev/input") else {
        return probed;
    };
    let mut numbers: Vec<u16> = dir
        .flatten()
        .filter_map(|entry| parse_event_number(&entry.file_name().to_string_lossy()))
        .collect();
    numbers.sort_unstable();

    for number in numbers {
        if probed.touch.is_some() && probed.pen.is_some() && probed.button.is_some() {
            break;
        }
        let Ok(path) = CString::new(format!("/dev/input/event{}", number)) else {
            continue;
        };
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            continue;
        }

        let mut version: libc::c_int = 0;
        if unsafe { libc::ioctl(fd, EVIOCGVERSION, &mut version) } != 0 {
            unsafe { libc::close(fd) };
            continue;
        }

        let type_bits = event_bits(fd, 0);
        let key_bits = event_bits(fd, EV_KEY);
        let abs_bits = event_bits(fd, EV_ABS);
        unsafe { libc::close(fd) };

        if test_bit(EV_KEY, &type_bits) {
            if test_bit(BTN_STYLUS, &key_bits) && test_bit(EV_ABS, &type_bits) {
                probed.pen.get_or_insert(number);
                continue;
            }
            if test_bit(KEY_POWER, &key_bits) {
                probed.button.get_or_insert(number);
                continue;
            }
        }
        if test_bit(ABS_MT_SLOT, &abs_bits) {
            probed.touch.get_or_insert(number);
        }
    }

    probed
}

impl DeviceProfile {
    /// Replaces the static per-model input device numbers with the ones found
    /// on the running system, keeping the static number for any class the
    /// probe could not identify.
    pub fn apply_probed_input_devices(&mut self, probed: ProbedInputDevices) {
        if let Some(touch) = probed.touch {
            self.touch_device = touch;
        }
        if let Some(pen) = probed.pen {
            self.pen_device = pen;
        }
        if let Some(button) = probed.button {
            self.button_device = button;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rm1_transforms() {
        let profile = DeviceProfile::new(DeviceType::RM1);
        let (tx, ty) = profile.transform_touch(0, 0);
        assert_eq!(tx, 1403);
        assert_eq!(ty, 1871);

        let (px, py, pd) = profile.transform_pen(0, 0, 0);
        assert_eq!(px, 0);
        assert_eq!(py, 1871);
        assert_eq!(pd, 0);
    }

    #[test]
    fn test_rm2_transforms() {
        let profile = DeviceProfile::new(DeviceType::RM2);
        let (tx, ty) = profile.transform_touch(0, 1871);
        assert_eq!(tx, 0);
        assert_eq!(ty, 0);

        let (px, py, pd) = profile.transform_pen(20967, 15725, 4096);
        assert_eq!(px, 1403);
        assert_eq!(py, 0);
        assert_eq!(pd, 100);
    }

    #[test]
    fn test_rmpp_transforms() {
        let profile = DeviceProfile::new(DeviceType::RMPP);
        let (tx, ty) = profile.transform_touch(2064, 2832);
        assert_eq!(tx, 1619);
        assert_eq!(ty, 2159);

        let (px, py, pd) = profile.transform_pen(11180, 15340, 255);
        assert_eq!(px, 1619);
        assert_eq!(py, 2159);
        assert_eq!(pd, 100);
    }

    #[test]
    fn test_rmppm_transforms() {
        let profile = DeviceProfile::new(DeviceType::RMPPM);
        let (tx, ty) = profile.transform_touch(1248, 2208);
        assert_eq!(tx, 953);
        assert_eq!(ty, 1695);

        let (px, py, pd) = profile.transform_pen(6760, 11960, 255);
        assert_eq!(px, 953);
        assert_eq!(py, 1695);
        assert_eq!(pd, 100);
    }

    #[test]
    fn event_numbers_parse_from_dev_input_names() {
        assert_eq!(parse_event_number("event0"), Some(0));
        assert_eq!(parse_event_number("event12"), Some(12));
        assert_eq!(parse_event_number("mice"), None);
        assert_eq!(parse_event_number("eventx"), None);
    }

    #[test]
    fn probed_devices_override_only_what_was_found() {
        let mut profile = DeviceProfile::new(DeviceType::RM1);
        profile.apply_probed_input_devices(ProbedInputDevices {
            touch: Some(5),
            pen: None,
            button: Some(7),
        });
        assert_eq!(profile.touch_device, 5);
        assert_eq!(profile.pen_device, 0);
        assert_eq!(profile.button_device, 7);
    }

    #[test]
    fn eviocgbit_matches_kernel_encoding() {
        // _IOC(_IOC_READ, 'E', 0x20 + EV_KEY, 96)
        assert_eq!(eviocgbit(EV_KEY, 96) & 0xffff_ffff, 0x8060_4521);
        // EVIOCGVERSION = _IOR('E', 0x01, int)
        assert_eq!(EVIOCGVERSION & 0xffff_ffff, 0x8004_4501);
    }

    #[test]
    fn test_rmpure_transforms() {
        let profile = DeviceProfile::new(DeviceType::RMPure);
        let (tx, ty) = profile.transform_touch(1776, 2400);
        assert_eq!(tx, 1403);
        assert_eq!(ty, 1871);

        let (px, py, pd) = profile.transform_pen(9620, 13000, 255);
        assert_eq!(px, 1403);
        assert_eq!(py, 1871);
        assert_eq!(pd, 100);
    }
}
