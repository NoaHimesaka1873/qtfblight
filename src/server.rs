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

use crate::blight::{
    BlightBus, BlightImageFormat, BlightUpdateMode, BlightWaveformMode, LibBlight,
};
use crate::device::DeviceProfile;
use crate::qtfb::{
    ClientMessage, FBFMT_RM2FB, FBFMT_RMPP_RGB565, FBFMT_RMPP_RGB888, FBFMT_RMPP_RGBA8888,
    FBFMT_RMPPM_RGB565, FBFMT_RMPPM_RGB888, FBFMT_RMPPM_RGBA8888, INPUT_BTN_PRESS,
    INPUT_BTN_RELEASE, INPUT_PEN_PRESS, INPUT_PEN_RELEASE, INPUT_PEN_UPDATE, INPUT_TOUCH_PRESS,
    INPUT_TOUCH_RELEASE, INPUT_TOUCH_UPDATE, InitMessageResponseContents,
    MESSAGE_CUSTOM_INITIALIZE, MESSAGE_INITIALIZE, MESSAGE_REQUEST_FULL_REFRESH,
    MESSAGE_SET_REFRESH_MODE, MESSAGE_TERMINATE, MESSAGE_UPDATE, MESSAGE_USERINPUT, ServerMessage,
    ServerMessageUnion, UPDATE_ALL, UPDATE_PARTIAL, UserInputContents,
};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const BTN_TOUCH: u16 = 0x14a;

use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

unsafe impl Send for LibBlight {}
unsafe impl Sync for LibBlight {}

pub struct ShmSegment {
    name: String,
    ptr: *mut c_void,
    size: usize,
}

impl ShmSegment {
    pub fn create(key: i32, size: usize) -> Result<Self, String> {
        let name = format!("/qtfb_{}", key);
        let c_name = CString::new(name.as_str()).unwrap();

        let fd = unsafe {
            libc::shm_open(
                c_name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o666,
            )
        };
        if fd < 0 {
            return Err(format!(
                "shm_open failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        if unsafe { libc::ftruncate(fd, size as libc::off_t) } < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            unsafe { libc::shm_unlink(c_name.as_ptr()) };
            return Err(format!("ftruncate failed: {}", err));
        }

        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };

        unsafe { libc::close(fd) };

        if ptr == libc::MAP_FAILED {
            let err = std::io::Error::last_os_error();
            unsafe { libc::shm_unlink(c_name.as_ptr()) };
            return Err(format!("mmap failed: {}", err));
        }

        Ok(ShmSegment { name, ptr, size })
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr as *mut u8
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.size);
            if let Ok(c_name) = CString::new(self.name.as_str()) {
                libc::shm_unlink(c_name.as_ptr());
            }
        }
    }
}

pub struct SeqPacketListener {
    pub fd: c_int,
}

impl SeqPacketListener {
    pub fn bind(path: &str) -> Result<Self, String> {
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
        if fd < 0 {
            return Err(format!(
                "socket() failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        let c_path = CString::new(path).unwrap();
        unsafe { libc::unlink(c_path.as_ptr()) };

        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_bytes();
        if bytes.len() >= addr.sun_path.len() {
            unsafe { libc::close(fd) };
            return Err("Socket path too long".to_string());
        }
        for (i, &b) in bytes.iter().enumerate() {
            addr.sun_path[i] = b as libc::c_char;
        }

        let addr_ptr = &addr as *const libc::sockaddr_un as *const libc::sockaddr;
        let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;

        if unsafe { libc::bind(fd, addr_ptr, addr_len) } < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("bind() failed: {}", err));
        }

        if unsafe { libc::listen(fd, 5) } < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("listen() failed: {}", err));
        }

        Ok(SeqPacketListener { fd })
    }

    pub fn accept(&self) -> Result<c_int, String> {
        let client_fd = unsafe { libc::accept(self.fd, ptr::null_mut(), ptr::null_mut()) };
        if client_fd < 0 {
            Err(format!(
                "accept() failed: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(client_fd)
        }
    }
}

impl Drop for SeqPacketListener {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

pub fn generate_random_key() -> i32 {
    let mut buf = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        buf = (now as u32).to_ne_bytes();
    }
    i32::from_ne_bytes(buf).abs() % 1000000
}

fn send_server_message(fd: c_int, msg: &ServerMessage) -> bool {
    let ptr = msg as *const ServerMessage as *const c_void;
    let len = std::mem::size_of::<ServerMessage>();
    let bytes_sent = unsafe { libc::send(fd, ptr, len, libc::MSG_NOSIGNAL) };
    bytes_sent == len as isize
}

fn broadcast_server_message(fb_key: i32, msg: &ServerMessage) {
    let fds = {
        let backends = get_backends().lock().unwrap();
        if let Some(backend) = backends.get(&fb_key) {
            backend.client_fds.clone()
        } else {
            Vec::new()
        }
    };
    for fd in fds {
        send_server_message(fd, msg);
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FramebufferFormat {
    Rgb565,
    Rgb888,
    Rgba8888,
}

impl FramebufferFormat {
    fn from_qtfb(value: u8) -> Result<Self, String> {
        match value {
            FBFMT_RM2FB | FBFMT_RMPP_RGB565 | FBFMT_RMPPM_RGB565 => Ok(Self::Rgb565),
            FBFMT_RMPP_RGB888 | FBFMT_RMPPM_RGB888 => Ok(Self::Rgb888),
            FBFMT_RMPP_RGBA8888 | FBFMT_RMPPM_RGBA8888 => Ok(Self::Rgba8888),
            _ => Err(format!("unsupported QTFB framebuffer type {}", value)),
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
            Self::Rgba8888 => 4,
        }
    }
}

fn default_dimensions(framebuffer_type: u8) -> Option<(u32, u32)> {
    match framebuffer_type {
        FBFMT_RM2FB => Some((1404, 1872)),
        FBFMT_RMPP_RGB888 | FBFMT_RMPP_RGBA8888 | FBFMT_RMPP_RGB565 => Some((1620, 2160)),
        FBFMT_RMPPM_RGB888 | FBFMT_RMPPM_RGBA8888 | FBFMT_RMPPM_RGB565 => Some((954, 1696)),
        _ => None,
    }
}

fn scale_input(value: i32, physical_extent: u32, framebuffer_extent: u32) -> i32 {
    if physical_extent == framebuffer_extent {
        value
    } else {
        (value as i64 * framebuffer_extent as i64 / physical_extent as i64) as i32
    }
}

fn clamp_region(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let x = x.max(0).min(width as i32);
    let y = y.max(0).min(height as i32);
    let w = w.max(0).min(width as i32 - x);
    let h = h.max(0).min(height as i32 - y);
    (w > 0 && h > 0).then_some((x as u32, y as u32, w as u32, h as u32))
}

fn scale_region(
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (u32, u32, u32, u32) {
    let x0 = x as u64 * target_width as u64 / source_width as u64;
    let y0 = y as u64 * target_height as u64 / source_height as u64;
    let x1 = ((x + w) as u64 * target_width as u64 + source_width as u64 - 1) / source_width as u64;
    let y1 =
        ((y + h) as u64 * target_height as u64 + source_height as u64 - 1) / source_height as u64;
    (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
}

fn copy_region(
    shm_ptr: *const u8,
    blight_ptr: *mut u8,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    region: (u32, u32, u32, u32),
    target_is_rgb16: bool,
    source_format: FramebufferFormat,
) -> (u32, u32, u32, u32) {
    let (x, y, w, h) = region;
    let target_region = scale_region(
        x,
        y,
        w,
        h,
        source_width,
        source_height,
        target_width,
        target_height,
    );
    let (target_x, target_y, target_w, target_h) = target_region;

    for target_row in target_y..target_y + target_h {
        let source_row = (target_row as u64 * source_height as u64 / target_height as u64) as u32;
        for target_col in target_x..target_x + target_w {
            let source_col = (target_col as u64 * source_width as u64 / target_width as u64) as u32;
            let source_index = (source_row as usize * source_width as usize + source_col as usize)
                * source_format.bytes_per_pixel();
            let target_index = (target_row as usize * target_width as usize + target_col as usize)
                * if target_is_rgb16 { 2 } else { 4 };
            unsafe {
                let (r8, g8, b8, a8) = match source_format {
                    FramebufferFormat::Rgb565 => {
                        let pixel = u16::from_ne_bytes([
                            *shm_ptr.add(source_index),
                            *shm_ptr.add(source_index + 1),
                        ]);
                        let r5 = ((pixel >> 11) & 0x1F) as u8;
                        let g6 = ((pixel >> 5) & 0x3F) as u8;
                        let b5 = (pixel & 0x1F) as u8;
                        (
                            (r5 << 3) | (r5 >> 2),
                            (g6 << 2) | (g6 >> 4),
                            (b5 << 3) | (b5 >> 2),
                            0xFF,
                        )
                    }
                    FramebufferFormat::Rgb888 => (
                        *shm_ptr.add(source_index),
                        *shm_ptr.add(source_index + 1),
                        *shm_ptr.add(source_index + 2),
                        0xFF,
                    ),
                    FramebufferFormat::Rgba8888 => (
                        *shm_ptr.add(source_index),
                        *shm_ptr.add(source_index + 1),
                        *shm_ptr.add(source_index + 2),
                        *shm_ptr.add(source_index + 3),
                    ),
                };
                if target_is_rgb16 {
                    let pixel =
                        ((r8 as u16 >> 3) << 11) | ((g8 as u16 >> 2) << 5) | (b8 as u16 >> 3);
                    let bytes = pixel.to_ne_bytes();
                    *blight_ptr.add(target_index) = bytes[0];
                    *blight_ptr.add(target_index + 1) = bytes[1];
                } else {
                    *blight_ptr.add(target_index) = r8;
                    *blight_ptr.add(target_index + 1) = g8;
                    *blight_ptr.add(target_index + 2) = b8;
                    *blight_ptr.add(target_index + 3) = a8;
                }
            }
        }
    }
    target_region
}

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

#[derive(Clone)]
struct ActiveBackend {
    shm_key: i32,
    shm_size: usize,
    shm_ptr: *mut c_void,
    shm_name: String,
    framebuffer_format: FramebufferFormat,
    framebuffer_width: u32,
    framebuffer_height: u32,
    blight_buf_ptr: *mut crate::blight::BlightBuf,
    surface_id: u16,
    client_fds: Vec<c_int>,
    input_running: Arc<AtomicBool>,
    ref_count: usize,
}

unsafe impl Send for ActiveBackend {}
unsafe impl Sync for ActiveBackend {}

static BACKENDS: OnceLock<Mutex<HashMap<i32, ActiveBackend>>> = OnceLock::new();

fn get_backends() -> &'static Mutex<HashMap<i32, ActiveBackend>> {
    BACKENDS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qtfb_framebuffer_formats_have_the_reference_byte_sizes() {
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RM2FB)
                .unwrap()
                .bytes_per_pixel(),
            2
        );
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RMPP_RGB888)
                .unwrap()
                .bytes_per_pixel(),
            3
        );
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RMPP_RGBA8888)
                .unwrap()
                .bytes_per_pixel(),
            4
        );
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RMPP_RGB565)
                .unwrap()
                .bytes_per_pixel(),
            2
        );
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RMPPM_RGB888)
                .unwrap()
                .bytes_per_pixel(),
            3
        );
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RMPPM_RGBA8888)
                .unwrap()
                .bytes_per_pixel(),
            4
        );
        assert_eq!(
            FramebufferFormat::from_qtfb(FBFMT_RMPPM_RGB565)
                .unwrap()
                .bytes_per_pixel(),
            2
        );
        assert!(FramebufferFormat::from_qtfb(255).is_err());
    }

    #[test]
    fn qtfb_default_dimensions_match_the_requested_format() {
        assert_eq!(default_dimensions(FBFMT_RM2FB), Some((1404, 1872)));
        assert_eq!(default_dimensions(FBFMT_RMPP_RGB888), Some((1620, 2160)));
        assert_eq!(default_dimensions(FBFMT_RMPPM_RGB888), Some((954, 1696)));
    }

    #[test]
    fn scaled_region_covers_the_corresponding_physical_area() {
        assert_eq!(
            scale_region(0, 0, 1404, 1872, 1404, 1872, 1620, 2160),
            (0, 0, 1620, 2160)
        );
        assert_eq!(
            scale_region(702, 936, 1, 1, 1404, 1872, 1620, 2160),
            (810, 1080, 2, 2)
        );
    }
}

pub fn handle_client(
    client_fd: c_int,
    libblight: Arc<LibBlight>,
    bus: *mut BlightBus,
    blight_fd: c_int,
    profile: DeviceProfile,
    running: Arc<AtomicBool>,
    key: i32,
) {
    println!("[server] Client connected");

    // 1. Read init message
    let mut msg: ClientMessage = unsafe { std::mem::zeroed() };
    let bytes_received = unsafe {
        libc::recv(
            client_fd,
            &mut msg as *mut ClientMessage as *mut c_void,
            std::mem::size_of::<ClientMessage>(),
            0,
        )
    };

    if bytes_received < std::mem::size_of::<ClientMessage>() as isize {
        println!("[server] Failed to receive complete ClientMessage, disconnecting");
        unsafe { libc::close(client_fd) };
        return;
    }

    let (fb_key, framebuffer_type, custom_dimensions) = if msg.msg_type == MESSAGE_INITIALIZE {
        println!("[server] MESSAGE_INITIALIZE received");
        let init = unsafe { msg.payload.init };
        (init.framebuffer_key, init.framebuffer_type, None)
    } else if msg.msg_type == MESSAGE_CUSTOM_INITIALIZE {
        let custom_init = unsafe { msg.payload.custom_init };
        println!(
            "[server] MESSAGE_CUSTOM_INITIALIZE received ({}x{})",
            custom_init.width, custom_init.height
        );
        (
            custom_init.framebuffer_key,
            custom_init.framebuffer_type,
            Some((custom_init.width as u32, custom_init.height as u32)),
        )
    } else {
        println!(
            "[server] Expected initialize message, received type {}",
            msg.msg_type
        );
        unsafe { libc::close(client_fd) };
        return;
    };

    let framebuffer_format = match FramebufferFormat::from_qtfb(framebuffer_type) {
        Ok(format) => format,
        Err(e) => {
            println!("[server] {}", e);
            unsafe { libc::close(client_fd) };
            return;
        }
    };

    let (width, height) = match custom_dimensions.or_else(|| default_dimensions(framebuffer_type)) {
        Some(dimensions) if dimensions.0 > 0 && dimensions.1 > 0 => dimensions,
        _ => {
            println!("[server] invalid framebuffer dimensions");
            unsafe { libc::close(client_fd) };
            return;
        }
    };

    let is_rgb16 = profile.format == BlightImageFormat::FormatRGB16;
    let shm_size = (width as usize) * (height as usize) * framebuffer_format.bytes_per_pixel();

    let mut backends = get_backends().lock().unwrap();
    let backend_info = if let Some(backend) = backends.get_mut(&fb_key) {
        if backend.framebuffer_format != framebuffer_format
            || backend.framebuffer_width != width
            || backend.framebuffer_height != height
        {
            println!(
                "[server] Refusing incompatible attachment for fb_key {}",
                fb_key
            );
            unsafe { libc::close(client_fd) };
            return;
        }
        backend.ref_count += 1;
        backend.client_fds.push(client_fd);
        println!(
            "[server] Reusing existing backend for fb_key {}. ref_count is now {}, client FDs: {:?}",
            fb_key, backend.ref_count, backend.client_fds
        );
        backend.clone()
    } else {
        println!("[server] Creating new backend for fb_key {}", fb_key);
        // 2. Allocate POSIX SHM
        let mut try_key = key;
        let mut shm = None;
        for _ in 0..10 {
            match ShmSegment::create(try_key, shm_size) {
                Ok(s) => {
                    shm = Some(s);
                    break;
                }
                Err(e) => {
                    println!(
                        "[server] SHM creation failed with key {}: {}, trying another key",
                        try_key, e
                    );
                    try_key = generate_random_key();
                }
            }
        }

        let shm = match shm {
            Some(s) => s,
            None => {
                println!("[server] Failed to create SHM segment after 10 attempts");
                unsafe { libc::close(client_fd) };
                return;
            }
        };

        // 3. Create Blight buffer and surface
        let blight_stride = profile.width * if is_rgb16 { 2 } else { 4 };
        let blight_buf_ptr = match libblight.create_buffer(
            0,
            0,
            profile.width,
            profile.height,
            blight_stride,
            profile.format,
            1.0,
        ) {
            Ok(b) => b,
            Err(e) => {
                println!("[server] blight_create_buffer failed: {}", e);
                unsafe { libc::close(client_fd) };
                return;
            }
        };

        let surface_id = match libblight.add_surface(bus, blight_buf_ptr) {
            Ok(id) => id,
            Err(e) => {
                println!("[server] blight_add_surface failed: {}", e);
                libblight.buffer_deref(blight_buf_ptr);
                unsafe { libc::close(client_fd) };
                return;
            }
        };

        println!("[server] Added surface id {}", surface_id);

        let _ = libblight.focus(blight_fd);
        let _ = libblight.raise(blight_fd, surface_id);

        let input_running = Arc::new(AtomicBool::new(true));

        // Touch Thread
        let touch_lib = Arc::clone(&libblight);
        let touch_running = Arc::clone(&input_running);
        let touch_profile = profile.clone();
        let touch_width = width;
        let touch_height = height;
        std::thread::spawn(move || {
            let thread_bus = match touch_lib.connect_bus() {
                Ok(b) => b,
                Err(e) => {
                    println!("[touch_thread] Failed to connect to DBus: {}", e);
                    return;
                }
            };
            let touch_buf =
                match touch_lib.service_input_open(thread_bus, touch_profile.touch_device) {
                    Ok(b) => b,
                    Err(e) => {
                        println!(
                            "[touch_thread] Failed to open input buffer for touch (device {}): {}",
                            touch_profile.touch_device, e
                        );
                        touch_lib.deref_bus(thread_bus);
                        return;
                    }
                };

            let mut last_tracking_ids = vec![-1; 16];
            let mut last_x = vec![0; 16];
            let mut last_y = vec![0; 16];

            #[derive(Copy, Clone)]
            struct SlotState {
                tracking_id: i32,
                x: i32,
                y: i32,
                dirty: bool,
            }

            let mut slots = [SlotState {
                tracking_id: -1,
                x: 0,
                y: 0,
                dirty: false,
            }; 16];
            let mut current_slot = 0;

            while touch_running.load(Ordering::Relaxed) {
                match touch_lib.event_from_buffer(touch_buf, true) {
                    Ok(ev_ptr) => {
                        let ev = unsafe { *ev_ptr };
                        touch_lib.event_free(ev_ptr);

                        if ev.type_ == EV_ABS {
                            if ev.code == ABS_MT_SLOT {
                                if ev.value >= 0 && ev.value < 16 {
                                    current_slot = ev.value as usize;
                                }
                            } else if ev.code == ABS_MT_TRACKING_ID {
                                slots[current_slot].tracking_id = ev.value;
                                slots[current_slot].dirty = true;
                            } else if ev.code == ABS_MT_POSITION_X {
                                slots[current_slot].x = ev.value;
                                slots[current_slot].dirty = true;
                            } else if ev.code == ABS_MT_POSITION_Y {
                                slots[current_slot].y = ev.value;
                                slots[current_slot].dirty = true;
                            }
                        } else if ev.type_ == EV_SYN && ev.code == SYN_REPORT {
                            for i in 0..16 {
                                if slots[i].dirty {
                                    let id = slots[i].tracking_id;
                                    let last_id = last_tracking_ids[i];

                                    let input_type = if id != -1 && last_id == -1 {
                                        INPUT_TOUCH_PRESS
                                    } else if id == -1 && last_id != -1 {
                                        INPUT_TOUCH_RELEASE
                                    } else if id != -1 && last_id != -1 {
                                        INPUT_TOUCH_UPDATE
                                    } else {
                                        -1
                                    };

                                    last_tracking_ids[i] = id;

                                    if input_type != -1 {
                                        let raw_x = if id != -1 { slots[i].x } else { last_x[i] };
                                        let raw_y = if id != -1 { slots[i].y } else { last_y[i] };

                                        if id != -1 {
                                            last_x[i] = slots[i].x;
                                            last_y[i] = slots[i].y;
                                        }

                                        let (raw_mx, raw_my) =
                                            touch_profile.transform_touch(raw_x, raw_y);
                                        let mx =
                                            scale_input(raw_mx, touch_profile.width, touch_width);
                                        let my =
                                            scale_input(raw_my, touch_profile.height, touch_height);

                                        let event_dev_id = if id != -1 { id } else { last_id };
                                        let resp = ServerMessage {
                                            msg_type: MESSAGE_USERINPUT,
                                            payload: ServerMessageUnion {
                                                user_input: UserInputContents {
                                                    input_type,
                                                    dev_id: event_dev_id,
                                                    x: mx,
                                                    y: my,
                                                    d: 0,
                                                },
                                            },
                                        };
                                        broadcast_server_message(fb_key, &resp);
                                    }
                                    slots[i].dirty = false;
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            touch_lib.input_buffer_deref(touch_buf);
            touch_lib.deref_bus(thread_bus);
            println!("[touch_thread] Touch thread terminated");
        });

        // Pen Thread
        let pen_lib = Arc::clone(&libblight);
        let pen_running = Arc::clone(&input_running);
        let pen_profile = profile.clone();
        let pen_width = width;
        let pen_height = height;
        std::thread::spawn(move || {
            let thread_bus = match pen_lib.connect_bus() {
                Ok(b) => b,
                Err(e) => {
                    println!("[pen_thread] Failed to connect to DBus: {}", e);
                    return;
                }
            };
            let pen_buf = match pen_lib.service_input_open(thread_bus, pen_profile.pen_device) {
                Ok(b) => b,
                Err(e) => {
                    println!(
                        "[pen_thread] Failed to open pen input buffer (device {}): {}",
                        pen_profile.pen_device, e
                    );
                    pen_lib.deref_bus(thread_bus);
                    return;
                }
            };

            let mut raw_x = 0;
            let mut raw_y = 0;
            let mut raw_pressure = 0;
            let mut touching = false;
            let mut last_touching = false;
            let mut dirty = false;

            while pen_running.load(Ordering::Relaxed) {
                match pen_lib.event_from_buffer(pen_buf, true) {
                    Ok(ev_ptr) => {
                        let ev = unsafe { *ev_ptr };
                        pen_lib.event_free(ev_ptr);

                        if ev.type_ == EV_ABS {
                            if ev.code == ABS_X {
                                raw_x = ev.value;
                                dirty = true;
                            } else if ev.code == ABS_Y {
                                raw_y = ev.value;
                                dirty = true;
                            } else if ev.code == ABS_PRESSURE {
                                raw_pressure = ev.value;
                                dirty = true;
                            }
                        } else if ev.type_ == EV_KEY {
                            if ev.code == BTN_TOUCH {
                                touching = ev.value == 1;
                                dirty = true;
                            }
                        } else if ev.type_ == EV_SYN && ev.code == SYN_REPORT {
                            if dirty {
                                let input_type = if touching && !last_touching {
                                    INPUT_PEN_PRESS
                                } else if !touching && last_touching {
                                    INPUT_PEN_RELEASE
                                } else {
                                    INPUT_PEN_UPDATE
                                };

                                let (raw_mx, raw_my, md) =
                                    pen_profile.transform_pen(raw_x, raw_y, raw_pressure);
                                let mx = scale_input(raw_mx, pen_profile.width, pen_width);
                                let my = scale_input(raw_my, pen_profile.height, pen_height);

                                let resp = ServerMessage {
                                    msg_type: MESSAGE_USERINPUT,
                                    payload: ServerMessageUnion {
                                        user_input: UserInputContents {
                                            input_type,
                                            dev_id: 0,
                                            x: mx,
                                            y: my,
                                            d: md,
                                        },
                                    },
                                };
                                broadcast_server_message(fb_key, &resp);
                                last_touching = touching;
                                dirty = false;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            pen_lib.input_buffer_deref(pen_buf);
            pen_lib.deref_bus(thread_bus);
            println!("[pen_thread] Pen thread terminated");
        });

        // Buttons Thread
        let btn_lib = Arc::clone(&libblight);
        let btn_running = Arc::clone(&input_running);
        let btn_profile = profile.clone();
        std::thread::spawn(move || {
            let thread_bus = match btn_lib.connect_bus() {
                Ok(b) => b,
                Err(_) => {
                    return;
                }
            };
            let btn_buf = match btn_lib.service_input_open(thread_bus, btn_profile.button_device) {
                Ok(b) => b,
                Err(_) => {
                    btn_lib.deref_bus(thread_bus);
                    return;
                }
            };

            while btn_running.load(Ordering::Relaxed) {
                match btn_lib.event_from_buffer(btn_buf, true) {
                    Ok(ev_ptr) => {
                        let ev = unsafe { *ev_ptr };
                        btn_lib.event_free(ev_ptr);

                        if ev.type_ == EV_KEY {
                            let key_idx = if ev.code == 105 {
                                0 // LEFT
                            } else if ev.code == 102 {
                                1 // HOME
                            } else if ev.code == 106 {
                                2 // RIGHT
                            } else {
                                -1
                            };

                            if key_idx >= 0 {
                                let input_type = if ev.value == 1 {
                                    INPUT_BTN_PRESS
                                } else {
                                    INPUT_BTN_RELEASE
                                };
                                let resp = ServerMessage {
                                    msg_type: MESSAGE_USERINPUT,
                                    payload: ServerMessageUnion {
                                        user_input: UserInputContents {
                                            input_type,
                                            dev_id: 0,
                                            x: key_idx,
                                            y: 0,
                                            d: 0,
                                        },
                                    },
                                };
                                broadcast_server_message(fb_key, &resp);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            btn_lib.input_buffer_deref(btn_buf);
            btn_lib.deref_bus(thread_bus);
            println!("[btn_thread] Buttons thread terminated");
        });

        let new_backend = ActiveBackend {
            shm_key: try_key,
            shm_size,
            shm_ptr: shm.ptr,
            shm_name: shm.name.clone(),
            framebuffer_format,
            framebuffer_width: width,
            framebuffer_height: height,
            blight_buf_ptr,
            surface_id,
            client_fds: vec![client_fd],
            input_running: Arc::clone(&input_running),
            ref_count: 1,
        };

        std::mem::forget(shm);
        backends.insert(fb_key, new_backend.clone());
        new_backend
    };
    drop(backends);

    // 4. Send response to client
    let resp = ServerMessage {
        msg_type: MESSAGE_INITIALIZE,
        payload: ServerMessageUnion {
            init: InitMessageResponseContents {
                shm_key_defined: backend_info.shm_key,
                shm_size: backend_info.shm_size,
            },
        },
    };

    if !send_server_message(client_fd, &resp) {
        println!("[server] Failed to send init response to client");
        let mut backends = get_backends().lock().unwrap();
        if let Some(backend) = backends.get_mut(&fb_key) {
            backend.ref_count -= 1;
            backend.client_fds.retain(|&fd| fd != client_fd);
            if backend.ref_count == 0 {
                backend_info.input_running.store(false, Ordering::Relaxed);
                let _ = libblight.remove_surface(blight_fd, backend_info.surface_id);
                libblight.buffer_deref(backend_info.blight_buf_ptr);
                let _shm_segment = ShmSegment {
                    name: backend_info.shm_name.clone(),
                    ptr: backend_info.shm_ptr,
                    size: backend_info.shm_size,
                };
                backends.remove(&fb_key);
            }
        }
        unsafe { libc::close(client_fd) };
        return;
    }

    let input_running = Arc::clone(&backend_info.input_running);

    // 6. Main server message loop
    let mut refresh_mode = 4; // Default to UI (REFRESH_MODE_UI)
    let mut last_loop_time = std::time::Instant::now();

    let mut poll_client = libc::pollfd {
        fd: client_fd,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        if !running.load(Ordering::Relaxed) || !input_running.load(Ordering::Relaxed) {
            break;
        }

        // Check if resumed from suspend
        let mut force_focus_raise = false;
        if crate::RESUMED.swap(false, Ordering::SeqCst) {
            println!("[server] SIGCONT received, re-focusing and re-raising surface");
            force_focus_raise = true;
        }

        let now = std::time::Instant::now();
        if now.duration_since(last_loop_time).as_secs_f32() > 3.0 {
            println!(
                "[server] Suspend detected via time-elapsed, re-focusing and re-raising surface"
            );
            force_focus_raise = true;
        }
        last_loop_time = now;

        if force_focus_raise {
            let _ = libblight.focus(blight_fd);
            let _ = libblight.raise(blight_fd, backend_info.surface_id);
        }

        let res = unsafe { libc::poll(&mut poll_client, 1, 500) };
        if res < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if res == 0 {
            // Timeout, loop again to check resume/suspend flags
            continue;
        }

        let mut client_msg: ClientMessage = unsafe { std::mem::zeroed() };
        let bytes_received = unsafe {
            libc::recv(
                client_fd,
                &mut client_msg as *mut ClientMessage as *mut c_void,
                std::mem::size_of::<ClientMessage>(),
                0,
            )
        };

        if bytes_received <= 0 {
            println!("[server] Client disconnected or socket error");
            break;
        }

        match client_msg.msg_type {
            MESSAGE_UPDATE => {
                let update = unsafe { client_msg.payload.update };
                let (x, y, w, h) = match update.update_type {
                    UPDATE_ALL => (0, 0, width as i32, height as i32),
                    UPDATE_PARTIAL => (update.x, update.y, update.w, update.h),
                    other => {
                        println!("[server] Ignoring unknown update type {}", other);
                        continue;
                    }
                };

                let Some(region) = clamp_region(x, y, w, h, width, height) else {
                    continue;
                };

                let (target_x, target_y, target_w, target_h) = unsafe {
                    let blight_buf = &*backend_info.blight_buf_ptr;
                    copy_region(
                        backend_info.shm_ptr as *const u8,
                        blight_buf.data,
                        width,
                        height,
                        profile.width,
                        profile.height,
                        region,
                        is_rgb16,
                        backend_info.framebuffer_format,
                    )
                };

                // Map refresh_mode to waveform mode
                let waveform = match refresh_mode {
                    0 => BlightWaveformMode::UltraFast,
                    1 => BlightWaveformMode::Fast,
                    2 => BlightWaveformMode::Animate,
                    3 => BlightWaveformMode::Content,
                    4 => BlightWaveformMode::UI,
                    _ => BlightWaveformMode::UI,
                };

                let repaint_res = libblight.surface_repaint(
                    blight_fd,
                    backend_info.surface_id,
                    target_x as i32,
                    target_y as i32,
                    target_w,
                    target_h,
                    waveform,
                    profile.color_type,
                    BlightUpdateMode::PartialUpdate,
                );
                if let Err(e) = repaint_res {
                    println!("[server] blight_surface_repaint failed: {}", e);
                }
            }
            MESSAGE_SET_REFRESH_MODE => {
                let mode = unsafe { client_msg.payload.refresh_mode };
                refresh_mode = mode;
                println!("[server] Set refresh mode: {}", refresh_mode);
            }
            MESSAGE_REQUEST_FULL_REFRESH => {
                println!("[server] MESSAGE_REQUEST_FULL_REFRESH received");
                // Trigger full refresh
                let repaint_res = libblight.surface_repaint(
                    blight_fd,
                    backend_info.surface_id,
                    0,
                    0,
                    width,
                    height,
                    BlightWaveformMode::Full,
                    profile.color_type,
                    BlightUpdateMode::FullUpdate,
                );
                if let Err(e) = repaint_res {
                    println!("[server] blight_surface_repaint full failed: {}", e);
                }
            }
            MESSAGE_TERMINATE => {
                println!("[server] MESSAGE_TERMINATE received");
                break;
            }
            other => {
                println!("[server] Received unhandled message type {}", other);
            }
        }
    }

    // Clean up
    println!("[server] Cleaning up client connection...");
    let mut backends = get_backends().lock().unwrap();
    if let Some(backend) = backends.get_mut(&fb_key) {
        backend.ref_count -= 1;
        backend.client_fds.retain(|&fd| fd != client_fd);
        if backend.ref_count == 0 {
            println!(
                "[server] Last connection for fb_key {} disconnected, destroying backend",
                fb_key
            );
            // Stop input forwarding before removing the backend from the
            // broadcast map.  Do not stop it for a still-attached keepalive
            // connection sharing this framebuffer.
            backend.input_running.store(false, Ordering::Relaxed);
            let _ = libblight.remove_surface(blight_fd, backend.surface_id);
            libblight.buffer_deref(backend.blight_buf_ptr);
            let _shm_segment = ShmSegment {
                name: backend.shm_name.clone(),
                ptr: backend.shm_ptr,
                size: backend.shm_size,
            };
            backends.remove(&fb_key);
        } else {
            println!(
                "[server] Backend for fb_key {} still has {} active connections",
                fb_key, backend.ref_count
            );
        }
    }
    drop(backends);

    unsafe { libc::close(client_fd) };
    println!("[server] Client handler exit");
}
