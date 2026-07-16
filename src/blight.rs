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

use std::ffi::CString;
use std::os::raw::{c_double, c_int, c_uint, c_void};
use std::ptr;

pub type BlightBus = c_void;
pub type BlightThread = c_void;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BlightImageFormat {
    FormatInvalid = 0,
    FormatMono,
    FormatMonoLSB,
    FormatIndexed8,
    FormatRGB32,
    FormatARGB32,
    FormatARGB32Premultiplied,
    FormatRGB16,
    FormatARGB8565Premultiplied,
    FormatRGB666,
    FormatARGB6666Premultiplied,
    FormatRGB555,
    FormatARGB8555Premultiplied,
    FormatRGB888,
    FormatRGB444,
    FormatARGB4444Premultiplied,
    FormatRGBX8888,
    FormatRGBA8888,
    FormatRGBA8888Premultiplied,
    FormatBGR30,
    FormatA2BGR30Premultiplied,
    FormatRGB30,
    FormatA2RGB30Premultiplied,
    FormatAlpha8,
    FormatGrayscale8,
    FormatRGBX64,
    FormatRGBA64,
    FormatRGBA64Premultiplied,
    FormatGrayscale16,
    FormatBGR888,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BlightWaveformMode {
    UltraFast = 0,
    Fast = 1,
    Animate = 2,
    Content = 3,
    UI = 4,
    Full = 5,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BlightContentType {
    Monochrome = 0,
    Color = 1,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BlightUpdateMode {
    PartialUpdate = 0x00,
    FullUpdate = 0x01,
    PenUpdate = 0x02,
    AnimationUpdate = 0x04,
    UIUpdate = 0x08,
}

#[repr(C)]
pub struct BlightInputBuffer {
    pub device: u16,
    pub fd: c_int,
    pub ring_buffer: *mut c_void,
}

#[repr(C)]
pub struct BlightBuf {
    pub fd: c_int,
    pub x: c_int,
    pub y: c_int,
    pub width: c_uint,
    pub height: c_uint,
    pub stride: c_uint,
    pub format: BlightImageFormat,
    pub scale: c_double,
    pub data: *mut u8,
}

pub struct LibBlight {
    _handle: *mut c_void,
    blight_bus_connect_system: unsafe extern "C" fn(*mut *mut BlightBus) -> c_int,
    blight_bus_connect_user: unsafe extern "C" fn(*mut *mut BlightBus) -> c_int,
    blight_bus_deref: unsafe extern "C" fn(*mut BlightBus),
    blight_service_available: unsafe extern "C" fn(*mut BlightBus) -> bool,
    blight_service_open: unsafe extern "C" fn(*mut BlightBus) -> c_int,
    blight_service_input_open: unsafe extern "C" fn(*mut BlightBus, u16) -> *mut BlightInputBuffer,
    blight_input_buffer_deref: unsafe extern "C" fn(*mut BlightInputBuffer),
    blight_event_from_buffer:
        unsafe extern "C" fn(*mut BlightInputBuffer, *mut *mut libc::input_event, bool) -> c_int,
    blight_event_free: unsafe extern "C" fn(*mut libc::input_event),
    blight_create_buffer: unsafe extern "C" fn(
        c_int,
        c_int,
        c_uint,
        c_uint,
        c_uint,
        BlightImageFormat,
        c_double,
    ) -> *mut BlightBuf,
    blight_buffer_deref: unsafe extern "C" fn(*mut BlightBuf),
    blight_add_surface: unsafe extern "C" fn(*mut BlightBus, *mut BlightBuf) -> u16,
    blight_remove_surface: unsafe extern "C" fn(c_int, u16) -> c_int,
    blight_surface_repaint: unsafe extern "C" fn(
        c_int,
        u16,
        c_int,
        c_int,
        c_uint,
        c_uint,
        BlightWaveformMode,
        BlightContentType,
        BlightUpdateMode,
    ) -> c_uint,
    blight_focus: unsafe extern "C" fn(c_int) -> c_int,
    blight_raise: unsafe extern "C" fn(c_int, u16) -> c_int,
    blight_start_connection_thread: unsafe extern "C" fn(c_int) -> *mut BlightThread,
    blight_connection_thread_deref: unsafe extern "C" fn(*mut BlightThread) -> c_int,
}

impl LibBlight {
    pub fn load() -> Result<Self, String> {
        let paths = [
            "/home/root/.vellum/lib/libblight_protocol.so.3",
            "/usr/lib/libblight_protocol.so.3",
        ];
        let mut handle = ptr::null_mut();

        for path in &paths {
            let cpath = CString::new(*path).map_err(|e| e.to_string())?;
            handle = unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            if !handle.is_null() {
                break;
            }
        }

        if handle.is_null() {
            return Err("Failed to load libblight_protocol.so: dlopen returned NULL".to_string());
        }

        let load_sym = |name: &str| -> Result<*mut c_void, String> {
            let cname = CString::new(name).map_err(|e| e.to_string())?;
            let sym = unsafe { libc::dlsym(handle, cname.as_ptr()) };
            if sym.is_null() {
                Err(format!("Symbol not found: {}", name))
            } else {
                Ok(sym)
            }
        };

        unsafe {
            Ok(LibBlight {
                _handle: handle,
                blight_bus_connect_system: std::mem::transmute(load_sym(
                    "blight_bus_connect_system",
                )?),
                blight_bus_connect_user: std::mem::transmute(load_sym("blight_bus_connect_user")?),
                blight_bus_deref: std::mem::transmute(load_sym("blight_bus_deref")?),
                blight_service_available: std::mem::transmute(load_sym(
                    "blight_service_available",
                )?),
                blight_service_open: std::mem::transmute(load_sym("blight_service_open")?),
                blight_service_input_open: std::mem::transmute(load_sym(
                    "blight_service_input_open",
                )?),
                blight_input_buffer_deref: std::mem::transmute(load_sym(
                    "blight_input_buffer_deref",
                )?),
                blight_event_from_buffer: std::mem::transmute(load_sym(
                    "blight_event_from_buffer",
                )?),
                blight_event_free: std::mem::transmute(load_sym("blight_event_free")?),
                blight_create_buffer: std::mem::transmute(load_sym("blight_create_buffer")?),
                blight_buffer_deref: std::mem::transmute(load_sym("blight_buffer_deref")?),
                blight_add_surface: std::mem::transmute(load_sym("blight_add_surface")?),
                blight_remove_surface: std::mem::transmute(load_sym("blight_remove_surface")?),
                blight_surface_repaint: std::mem::transmute(load_sym("blight_surface_repaint")?),
                blight_focus: std::mem::transmute(load_sym("blight_focus")?),
                blight_raise: std::mem::transmute(load_sym("blight_raise")?),
                blight_start_connection_thread: std::mem::transmute(load_sym(
                    "blight_start_connection_thread",
                )?),
                blight_connection_thread_deref: std::mem::transmute(load_sym(
                    "blight_connection_thread_deref",
                )?),
            })
        }
    }

    pub fn connect_bus(&self) -> Result<*mut BlightBus, String> {
        let mut bus = ptr::null_mut();
        let res = unsafe { (self.blight_bus_connect_system)(&mut bus) };
        if res >= 0 && !bus.is_null() {
            return Ok(bus);
        }
        let res = unsafe { (self.blight_bus_connect_user)(&mut bus) };
        if res >= 0 && !bus.is_null() {
            return Ok(bus);
        }
        Err("Failed to connect to system or user DBus".to_string())
    }

    pub fn deref_bus(&self, bus: *mut BlightBus) {
        if !bus.is_null() {
            unsafe { (self.blight_bus_deref)(bus) };
        }
    }

    pub fn service_available(&self, bus: *mut BlightBus) -> bool {
        if bus.is_null() {
            false
        } else {
            unsafe { (self.blight_service_available)(bus) }
        }
    }

    pub fn service_open(&self, bus: *mut BlightBus) -> Result<c_int, String> {
        let fd = unsafe { (self.blight_service_open)(bus) };
        if fd < 0 {
            Err(format!("Failed to open blight service (errno: {})", -fd))
        } else {
            Ok(fd)
        }
    }

    pub fn service_input_open(
        &self,
        bus: *mut BlightBus,
        device: u16,
    ) -> Result<*mut BlightInputBuffer, String> {
        let buf = unsafe { (self.blight_service_input_open)(bus, device) };
        if buf.is_null() {
            Err(format!("Failed to open input buffer for device {}", device))
        } else {
            Ok(buf)
        }
    }

    pub fn input_buffer_deref(&self, buf: *mut BlightInputBuffer) {
        if !buf.is_null() {
            unsafe { (self.blight_input_buffer_deref)(buf) };
        }
    }

    pub fn event_from_buffer(
        &self,
        buf: *mut BlightInputBuffer,
        blocking: bool,
    ) -> Result<*mut libc::input_event, c_int> {
        let mut event = ptr::null_mut();
        let res = unsafe { (self.blight_event_from_buffer)(buf, &mut event, blocking) };
        if res < 0 {
            Err(res)
        } else if event.is_null() {
            Err(-libc::EAGAIN)
        } else {
            Ok(event)
        }
    }

    pub fn event_free(&self, event: *mut libc::input_event) {
        if !event.is_null() {
            unsafe { (self.blight_event_free)(event) };
        }
    }

    pub fn create_buffer(
        &self,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        stride: c_uint,
        format: BlightImageFormat,
        scale: c_double,
    ) -> Result<*mut BlightBuf, String> {
        let buf =
            unsafe { (self.blight_create_buffer)(x, y, width, height, stride, format, scale) };
        if buf.is_null() {
            Err("Failed to create blight buffer".to_string())
        } else {
            Ok(buf)
        }
    }

    pub fn buffer_deref(&self, buf: *mut BlightBuf) {
        if !buf.is_null() {
            unsafe { (self.blight_buffer_deref)(buf) };
        }
    }

    pub fn add_surface(&self, bus: *mut BlightBus, buf: *mut BlightBuf) -> Result<u16, String> {
        let id = unsafe { (self.blight_add_surface)(bus, buf) };
        if id == 0 {
            Err("Failed to add blight surface".to_string())
        } else {
            Ok(id)
        }
    }

    pub fn remove_surface(&self, fd: c_int, identifier: u16) -> Result<(), String> {
        let res = unsafe { (self.blight_remove_surface)(fd, identifier) };
        if res < 0 {
            Err(format!("Failed to remove surface (errno: {})", -res))
        } else {
            Ok(())
        }
    }

    pub fn surface_repaint(
        &self,
        fd: c_int,
        identifier: u16,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        waveform: BlightWaveformMode,
        contenttype: BlightContentType,
        mode: BlightUpdateMode,
    ) -> Result<u32, String> {
        let res = unsafe {
            (self.blight_surface_repaint)(
                fd,
                identifier,
                x,
                y,
                width,
                height,
                waveform,
                contenttype,
                mode,
            )
        };
        if res == 0 {
            Err("Failed to repaint surface".to_string())
        } else {
            Ok(res)
        }
    }

    pub fn focus(&self, fd: c_int) -> Result<(), String> {
        let res = unsafe { (self.blight_focus)(fd) };
        if res < 0 {
            Err(format!("Failed to set focus (errno: {})", -res))
        } else {
            Ok(())
        }
    }

    pub fn raise(&self, fd: c_int, identifier: u16) -> Result<(), String> {
        let res = unsafe { (self.blight_raise)(fd, identifier) };
        if res < 0 {
            Err(format!("Failed to raise surface (errno: {})", -res))
        } else {
            Ok(())
        }
    }

    pub fn start_connection_thread(&self, fd: c_int) -> Result<*mut BlightThread, String> {
        let thread = unsafe { (self.blight_start_connection_thread)(fd) };
        if thread.is_null() {
            Err("Failed to start blight connection thread".to_string())
        } else {
            Ok(thread)
        }
    }

    pub fn connection_thread_deref(&self, thread: *mut BlightThread) {
        if !thread.is_null() {
            unsafe { (self.blight_connection_thread_deref)(thread) };
        }
    }
}
