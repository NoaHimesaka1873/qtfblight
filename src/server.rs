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
    BlightBufferSpec, BlightBus, BlightImageFormat, BlightRepaintRequest, BlightUpdateMode,
    BlightWaveformMode, LibBlight,
};
use crate::device::DeviceProfile;
use crate::qtfb::{
    ClientMessage, FBFMT_RM2FB, FBFMT_RMPP_RGB565, FBFMT_RMPP_RGB888, FBFMT_RMPP_RGBA8888,
    FBFMT_RMPPM_RGB565, FBFMT_RMPPM_RGB888, FBFMT_RMPPM_RGBA8888, INPUT_BTN_PRESS,
    INPUT_BTN_RELEASE, INPUT_BTN_X_HOME, INPUT_BTN_X_LEFT, INPUT_BTN_X_RIGHT, INPUT_PEN_PRESS,
    INPUT_PEN_RELEASE, INPUT_PEN_UPDATE, INPUT_TOUCH_PRESS, INPUT_TOUCH_RELEASE,
    INPUT_TOUCH_UPDATE, InitMessageResponseContents, MESSAGE_CUSTOM_INITIALIZE, MESSAGE_INITIALIZE,
    MESSAGE_REQUEST_FULL_REFRESH, MESSAGE_SET_REFRESH_MODE, MESSAGE_TERMINATE, MESSAGE_UPDATE,
    MESSAGE_USERINPUT, REFRESH_MODE_ANIMATE, REFRESH_MODE_CONTENT, REFRESH_MODE_FAST,
    REFRESH_MODE_UI, REFRESH_MODE_ULTRA_FAST, ServerMessage, ServerMessageUnion, UPDATE_ALL,
    UPDATE_PARTIAL, UserInputContents,
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

struct ShmSegment {
    key: i32,
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

        // AppLoad initializes a new QTFB framebuffer to white.  Keep the
        // same first-frame state for all supported pixel formats.
        unsafe { std::ptr::write_bytes(ptr as *mut u8, 0xFF, size) };

        Ok(ShmSegment {
            key,
            name,
            ptr,
            size,
        })
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

// The mapping is shared between the QTFB client and the server by design.
// Ownership is synchronized through `Arc`; this type does not expose safe
// references to the mapped bytes.
unsafe impl Send for ShmSegment {}
unsafe impl Sync for ShmSegment {}

pub struct SeqPacketListener {
    pub fd: c_int,
    path: CString,
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

        let c_path =
            CString::new(path).map_err(|_| "Socket path contains a NUL byte".to_string())?;
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
            unsafe {
                libc::close(fd);
                libc::unlink(c_path.as_ptr());
            }
            return Err(format!("listen() failed: {}", err));
        }

        Ok(SeqPacketListener { fd, path: c_path })
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
        unsafe {
            libc::close(self.fd);
            libc::unlink(self.path.as_ptr());
        }
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
    ((u32::from_ne_bytes(buf) & i32::MAX as u32) % 1_000_000) as i32
}

fn send_server_message(fd: c_int, msg: &ServerMessage) -> bool {
    let ptr = msg as *const ServerMessage as *const c_void;
    let len = std::mem::size_of::<ServerMessage>();
    // Input forwarding must not stall if a client is not reading its socket.
    // Match AppLoad by dropping packets when the socket buffer is full.
    let bytes_sent = unsafe { libc::send(fd, ptr, len, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
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
struct Dimensions {
    width: u32,
    height: u32,
}

impl Dimensions {
    const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

type Region = (u32, u32, u32, u32);

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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Framebuffer {
    format: FramebufferFormat,
    dimensions: Dimensions,
}

fn default_dimensions(framebuffer_type: u8) -> Option<Dimensions> {
    match framebuffer_type {
        FBFMT_RM2FB => Some(Dimensions::new(1404, 1872)),
        FBFMT_RMPP_RGB888 | FBFMT_RMPP_RGBA8888 | FBFMT_RMPP_RGB565 => {
            Some(Dimensions::new(1620, 2160))
        }
        FBFMT_RMPPM_RGB888 | FBFMT_RMPPM_RGBA8888 | FBFMT_RMPPM_RGB565 => {
            Some(Dimensions::new(954, 1696))
        }
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

fn clamp_region(x: i32, y: i32, w: i32, h: i32, dimensions: Dimensions) -> Option<Region> {
    let x = x.max(0).min(dimensions.width as i32);
    let y = y.max(0).min(dimensions.height as i32);
    let w = w.max(0).min(dimensions.width as i32 - x);
    let h = h.max(0).min(dimensions.height as i32 - y);
    (w > 0 && h > 0).then_some((x as u32, y as u32, w as u32, h as u32))
}

fn scale_region(region: Region, source: Dimensions, target: Dimensions) -> Region {
    let (x, y, w, h) = region;
    let x0 = x as u64 * target.width as u64 / source.width as u64;
    let y0 = y as u64 * target.height as u64 / source.height as u64;
    let x1 = ((x + w) as u64 * target.width as u64).div_ceil(source.width as u64);
    let y1 = ((y + h) as u64 * target.height as u64).div_ceil(source.height as u64);
    (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
}

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
mod neon {
    use super::FramebufferFormat;
    #[cfg(target_arch = "aarch64")]
    use std::arch::aarch64::*;
    #[cfg(target_arch = "arm")]
    use std::arch::arm::*;

    /// Converts as many pixels as possible in groups of eight. The caller owns
    /// the scalar tail and guarantees that both row pointers are valid.
    pub fn convert_row(
        source: FramebufferFormat,
        target_is_rgb16: bool,
        source_ptr: *const u8,
        target_ptr: *mut u8,
        pixels: usize,
    ) -> usize {
        if pixels < 8 {
            return 0;
        }

        #[cfg(target_arch = "arm")]
        if !std::arch::is_arm_feature_detected!("neon") {
            return 0;
        }

        // SAFETY: AArch64 requires ASIMD. ARMv7 reaches this call only after
        // runtime NEON detection; the caller provides enough bytes for every
        // eight-pixel vector load/store.
        unsafe { convert_row_neon(source, target_is_rgb16, source_ptr, target_ptr, pixels) }
    }

    #[target_feature(enable = "neon")]
    unsafe fn convert_row_neon(
        source: FramebufferFormat,
        target_is_rgb16: bool,
        source_ptr: *const u8,
        target_ptr: *mut u8,
        pixels: usize,
    ) -> usize {
        let vector_pixels = pixels & !7;
        let mut offset = 0;

        while offset < vector_pixels {
            // SAFETY: `convert_row`'s contract guarantees full vector-sized
            // source and target ranges for every iteration.
            unsafe {
                match (source, target_is_rgb16) {
                    (FramebufferFormat::Rgb565, false) => {
                        let pixel = vld1q_u16(source_ptr.add(offset * 2) as *const u16);
                        let r5 = vandq_u16(vshrq_n_u16::<11>(pixel), vdupq_n_u16(0x1f));
                        let g6 = vandq_u16(vshrq_n_u16::<5>(pixel), vdupq_n_u16(0x3f));
                        let b5 = vandq_u16(pixel, vdupq_n_u16(0x1f));
                        let r8 = vmovn_u16(vorrq_u16(vshlq_n_u16::<3>(r5), vshrq_n_u16::<2>(r5)));
                        let g8 = vmovn_u16(vorrq_u16(vshlq_n_u16::<2>(g6), vshrq_n_u16::<4>(g6)));
                        let b8 = vmovn_u16(vorrq_u16(vshlq_n_u16::<3>(b5), vshrq_n_u16::<2>(b5)));
                        vst4_u8(
                            target_ptr.add(offset * 4),
                            uint8x8x4_t(r8, g8, b8, vdup_n_u8(0xff)),
                        );
                    }
                    (FramebufferFormat::Rgb888, false) => {
                        let rgb = vld3_u8(source_ptr.add(offset * 3));
                        vst4_u8(
                            target_ptr.add(offset * 4),
                            uint8x8x4_t(rgb.0, rgb.1, rgb.2, vdup_n_u8(0xff)),
                        );
                    }
                    (FramebufferFormat::Rgb888, true) => {
                        let rgb = vld3_u8(source_ptr.add(offset * 3));
                        vst1q_u16(
                            target_ptr.add(offset * 2) as *mut u16,
                            pack_rgb565(rgb.0, rgb.1, rgb.2),
                        );
                    }
                    (FramebufferFormat::Rgba8888, true) => {
                        let rgba = vld4_u8(source_ptr.add(offset * 4));
                        vst1q_u16(
                            target_ptr.add(offset * 2) as *mut u16,
                            pack_rgb565(rgba.0, rgba.1, rgba.2),
                        );
                    }
                    // Matching RGB565/RGB16 and RGBA/RGBA cases use the
                    // byte-copy fast path; no other format pairs exist.
                    _ => return offset,
                }
            }
            offset += 8;
        }

        offset
    }

    #[target_feature(enable = "neon")]
    #[allow(unused_unsafe)]
    unsafe fn pack_rgb565(r: uint8x8_t, g: uint8x8_t, b: uint8x8_t) -> uint16x8_t {
        // Keep the high 5/6/5 bits of each eight-bit channel in RGB565 layout.
        unsafe {
            let r = vandq_u16(vshlq_n_u16::<8>(vmovl_u8(r)), vdupq_n_u16(0xf800));
            let g = vandq_u16(vshlq_n_u16::<3>(vmovl_u8(g)), vdupq_n_u16(0x07e0));
            let b = vandq_u16(vmovl_u8(b), vdupq_n_u16(0x001f));
            vorrq_u16(vorrq_u16(r, g), b)
        }
    }
}

#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
mod neon {
    use super::FramebufferFormat;

    pub fn convert_row(_: FramebufferFormat, _: bool, _: *const u8, _: *mut u8, _: usize) -> usize {
        0
    }
}

unsafe fn copy_pixel(
    source_ptr: *const u8,
    target_ptr: *mut u8,
    source_format: FramebufferFormat,
    target_is_rgb16: bool,
    source_index: usize,
    target_index: usize,
) {
    // SAFETY: Callers calculate in-bounds pixel offsets from validated regions.
    unsafe {
        let (r8, g8, b8, a8) = match source_format {
            FramebufferFormat::Rgb565 => {
                let pixel = u16::from_ne_bytes([
                    *source_ptr.add(source_index),
                    *source_ptr.add(source_index + 1),
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
                *source_ptr.add(source_index),
                *source_ptr.add(source_index + 1),
                *source_ptr.add(source_index + 2),
                0xFF,
            ),
            FramebufferFormat::Rgba8888 => (
                *source_ptr.add(source_index),
                *source_ptr.add(source_index + 1),
                *source_ptr.add(source_index + 2),
                *source_ptr.add(source_index + 3),
            ),
        };

        if target_is_rgb16 {
            let pixel = ((r8 as u16 >> 3) << 11) | ((g8 as u16 >> 2) << 5) | (b8 as u16 >> 3);
            let bytes = pixel.to_ne_bytes();
            *target_ptr.add(target_index) = bytes[0];
            *target_ptr.add(target_index + 1) = bytes[1];
        } else {
            *target_ptr.add(target_index) = r8;
            *target_ptr.add(target_index + 1) = g8;
            *target_ptr.add(target_index + 2) = b8;
            *target_ptr.add(target_index + 3) = a8;
        }
    }
}

fn copy_unscaled_region(
    source_ptr: *const u8,
    target_ptr: *mut u8,
    source: Framebuffer,
    region: Region,
    target_is_rgb16: bool,
) {
    let (x, y, width, height) = region;
    let source_bpp = source.format.bytes_per_pixel();
    let target_bpp = if target_is_rgb16 { 2 } else { 4 };

    for row in y..y + height {
        let source_row = unsafe {
            source_ptr
                .add((row as usize * source.dimensions.width as usize + x as usize) * source_bpp)
        };
        let target_row = unsafe {
            target_ptr
                .add((row as usize * source.dimensions.width as usize + x as usize) * target_bpp)
        };

        if matches!(
            (source.format, target_is_rgb16),
            (FramebufferFormat::Rgb565, true) | (FramebufferFormat::Rgba8888, false)
        ) {
            // SAFETY: source and target point to different framebuffer mappings
            // and each row range is fully contained in its respective buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(source_row, target_row, width as usize * source_bpp)
            };
            continue;
        }

        let vector_pixels = neon::convert_row(
            source.format,
            target_is_rgb16,
            source_row,
            target_row,
            width as usize,
        );
        for column in vector_pixels..width as usize {
            // SAFETY: the scalar tail begins after all vector-sized chunks and
            // remains within the validated row range.
            unsafe {
                copy_pixel(
                    source_row,
                    target_row,
                    source.format,
                    target_is_rgb16,
                    column * source_bpp,
                    column * target_bpp,
                );
            }
        }
    }
}

fn copy_region(
    shm_ptr: *const u8,
    blight_ptr: *mut u8,
    source: Framebuffer,
    target: Dimensions,
    region: Region,
    target_is_rgb16: bool,
) -> Region {
    let target_region = scale_region(region, source.dimensions, target);
    if source.dimensions == target {
        copy_unscaled_region(shm_ptr, blight_ptr, source, region, target_is_rgb16);
        return target_region;
    }

    let (target_x, target_y, target_w, target_h) = target_region;

    for target_row in target_y..target_y + target_h {
        let source_row =
            (target_row as u64 * source.dimensions.height as u64 / target.height as u64) as u32;
        for target_col in target_x..target_x + target_w {
            let source_col =
                (target_col as u64 * source.dimensions.width as u64 / target.width as u64) as u32;
            let source_index = (source_row as usize * source.dimensions.width as usize
                + source_col as usize)
                * source.format.bytes_per_pixel();
            let target_index = (target_row as usize * target.width as usize + target_col as usize)
                * if target_is_rgb16 { 2 } else { 4 };
            // SAFETY: clamped input and scaled output regions keep both pixel
            // indices inside their corresponding framebuffer mappings.
            unsafe {
                copy_pixel(
                    shm_ptr,
                    blight_ptr,
                    source.format,
                    target_is_rgb16,
                    source_index,
                    target_index,
                );
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
    shm: Arc<ShmSegment>,
    framebuffer: Framebuffer,
    blight_buf_ptr: *mut crate::blight::BlightBuf,
    surface_id: u16,
    client_fds: Vec<c_int>,
    input_running: Arc<AtomicBool>,
}

// libblight owns the backing buffer's lifetime. Access is serialized by the
// QTFB protocol handler and its surface connection thread.
unsafe impl Send for ActiveBackend {}
unsafe impl Sync for ActiveBackend {}

static BACKENDS: OnceLock<Mutex<HashMap<i32, ActiveBackend>>> = OnceLock::new();

fn get_backends() -> &'static Mutex<HashMap<i32, ActiveBackend>> {
    BACKENDS.get_or_init(|| Mutex::new(HashMap::new()))
}

enum ClientDetach {
    Last(ActiveBackend),
    StillAttached(usize),
    NotAttached,
}

fn detach_client(fb_key: i32, client_fd: c_int) -> ClientDetach {
    let mut backends = get_backends().lock().unwrap();
    let Some(backend) = backends.get_mut(&fb_key) else {
        return ClientDetach::NotAttached;
    };

    backend.client_fds.retain(|&fd| fd != client_fd);
    if backend.client_fds.is_empty() {
        ClientDetach::Last(
            backends
                .remove(&fb_key)
                .expect("backend disappeared while holding its mutex"),
        )
    } else {
        ClientDetach::StillAttached(backend.client_fds.len())
    }
}

fn cleanup_client_connection(
    fb_key: i32,
    client_fd: c_int,
    libblight: &LibBlight,
    blight_fd: c_int,
) {
    match detach_client(fb_key, client_fd) {
        ClientDetach::Last(backend) => {
            println!(
                "[server] Last connection for fb_key {} disconnected, destroying backend",
                fb_key
            );
            backend.input_running.store(false, Ordering::Relaxed);
            if let Err(e) = libblight.remove_surface(blight_fd, backend.surface_id) {
                println!("[server] Failed to remove surface: {}", e);
            }
            unsafe { libblight.buffer_deref(backend.blight_buf_ptr) };
        }
        ClientDetach::StillAttached(connection_count) => {
            println!(
                "[server] Backend for fb_key {} still has {} active connections",
                fb_key, connection_count
            );
        }
        ClientDetach::NotAttached => {}
    }
}

/// # Safety
///
/// `bus` must be a live libblight bus handle for the entire client-handler
/// lifetime. It is shared with the main libblight service connection.
pub unsafe fn handle_client(
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
            Some(Dimensions::new(
                custom_init.width as u32,
                custom_init.height as u32,
            )),
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

    let dimensions = match custom_dimensions.or_else(|| default_dimensions(framebuffer_type)) {
        Some(dimensions) if dimensions.width > 0 && dimensions.height > 0 => dimensions,
        _ => {
            println!("[server] invalid framebuffer dimensions");
            unsafe { libc::close(client_fd) };
            return;
        }
    };

    let framebuffer = Framebuffer {
        format: framebuffer_format,
        dimensions,
    };

    let is_rgb16 = profile.format == BlightImageFormat::FormatRGB16;
    let shm_size = dimensions.width as usize
        * dimensions.height as usize
        * framebuffer_format.bytes_per_pixel();

    let mut backends = get_backends().lock().unwrap();
    let backend_info = if let Some(backend) = backends.get_mut(&fb_key) {
        if backend.framebuffer != framebuffer {
            println!(
                "[server] Refusing incompatible attachment for fb_key {}",
                fb_key
            );
            unsafe { libc::close(client_fd) };
            return;
        }
        backend.client_fds.push(client_fd);
        println!(
            "[server] Reusing existing backend for fb_key {}. Client FDs: {:?}",
            fb_key, backend.client_fds
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
            Some(shm) => Arc::new(shm),
            None => {
                println!("[server] Failed to create SHM segment after 10 attempts");
                unsafe { libc::close(client_fd) };
                return;
            }
        };

        // 3. Create Blight buffer and surface
        let blight_stride = profile.width * if is_rgb16 { 2 } else { 4 };
        let blight_buf_ptr = match libblight.create_buffer(BlightBufferSpec {
            x: 0,
            y: 0,
            width: profile.width,
            height: profile.height,
            stride: blight_stride,
            format: profile.format,
            scale: 1.0,
        }) {
            Ok(b) => b,
            Err(e) => {
                println!("[server] blight_create_buffer failed: {}", e);
                unsafe { libc::close(client_fd) };
                return;
            }
        };

        let surface_id = match unsafe { libblight.add_surface(bus, blight_buf_ptr) } {
            Ok(id) => id,
            Err(e) => {
                println!("[server] blight_add_surface failed: {}", e);
                unsafe { libblight.buffer_deref(blight_buf_ptr) };
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
        let touch_dimensions = dimensions;
        let touch_surface_id = surface_id;
        let touch_blight_fd = blight_fd;
        std::thread::spawn(move || {
            let thread_bus = match touch_lib.connect_bus() {
                Ok(b) => b,
                Err(e) => {
                    println!("[touch_thread] Failed to connect to DBus: {}", e);
                    return;
                }
            };
            let touch_buf = match unsafe {
                touch_lib.service_input_open(thread_bus, touch_profile.touch_device)
            } {
                Ok(b) => b,
                Err(e) => {
                    println!(
                        "[touch_thread] Failed to open input buffer for touch (device {}): {}",
                        touch_profile.touch_device, e
                    );
                    unsafe { touch_lib.deref_bus(thread_bus) };
                    return;
                }
            };

            let mut last_tracking_ids = [-1; 16];
            let mut last_x = [0; 16];
            let mut last_y = [0; 16];

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
            let mut five_finger_refresh_sent = false;

            while touch_running.load(Ordering::Relaxed) {
                match unsafe { touch_lib.event_from_buffer(touch_buf, true) } {
                    Ok(ev_ptr) => {
                        let ev = unsafe { *ev_ptr };
                        unsafe { touch_lib.event_free(ev_ptr) };

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
                            let active_touches =
                                slots.iter().filter(|slot| slot.tracking_id != -1).count();
                            if active_touches == 5 && !five_finger_refresh_sent {
                                if let Err(e) = touch_lib.surface_repaint(
                                    touch_blight_fd,
                                    BlightRepaintRequest {
                                        surface_id: touch_surface_id,
                                        x: 0,
                                        y: 0,
                                        width: touch_profile.width,
                                        height: touch_profile.height,
                                        waveform: BlightWaveformMode::Full,
                                        content_type: touch_profile.color_type,
                                        update_mode: BlightUpdateMode::FullUpdate,
                                    },
                                ) {
                                    println!(
                                        "[touch_thread] five-finger full refresh failed: {}",
                                        e
                                    );
                                }
                                five_finger_refresh_sent = true;
                            } else if active_touches == 0 {
                                five_finger_refresh_sent = false;
                            }

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
                                        let mx = scale_input(
                                            raw_mx,
                                            touch_profile.width,
                                            touch_dimensions.width,
                                        );
                                        let my = scale_input(
                                            raw_my,
                                            touch_profile.height,
                                            touch_dimensions.height,
                                        );

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
            unsafe { touch_lib.input_buffer_deref(touch_buf) };
            unsafe { touch_lib.deref_bus(thread_bus) };
            println!("[touch_thread] Touch thread terminated");
        });

        // Pen Thread
        let pen_lib = Arc::clone(&libblight);
        let pen_running = Arc::clone(&input_running);
        let pen_profile = profile.clone();
        let pen_dimensions = dimensions;
        std::thread::spawn(move || {
            let thread_bus = match pen_lib.connect_bus() {
                Ok(b) => b,
                Err(e) => {
                    println!("[pen_thread] Failed to connect to DBus: {}", e);
                    return;
                }
            };
            let pen_buf =
                match unsafe { pen_lib.service_input_open(thread_bus, pen_profile.pen_device) } {
                    Ok(b) => b,
                    Err(e) => {
                        println!(
                            "[pen_thread] Failed to open pen input buffer (device {}): {}",
                            pen_profile.pen_device, e
                        );
                        unsafe { pen_lib.deref_bus(thread_bus) };
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
                match unsafe { pen_lib.event_from_buffer(pen_buf, true) } {
                    Ok(ev_ptr) => {
                        let ev = unsafe { *ev_ptr };
                        unsafe { pen_lib.event_free(ev_ptr) };

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
                        } else if ev.type_ == EV_SYN && ev.code == SYN_REPORT && dirty {
                            let input_type = if touching && !last_touching {
                                INPUT_PEN_PRESS
                            } else if !touching && last_touching {
                                INPUT_PEN_RELEASE
                            } else {
                                INPUT_PEN_UPDATE
                            };

                            let (raw_mx, raw_my, md) =
                                pen_profile.transform_pen(raw_x, raw_y, raw_pressure);
                            let mx = scale_input(raw_mx, pen_profile.width, pen_dimensions.width);
                            let my = scale_input(raw_my, pen_profile.height, pen_dimensions.height);

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
                    Err(_) => break,
                }
            }
            unsafe { pen_lib.input_buffer_deref(pen_buf) };
            unsafe { pen_lib.deref_bus(thread_bus) };
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
            let btn_buf = match unsafe {
                btn_lib.service_input_open(thread_bus, btn_profile.button_device)
            } {
                Ok(b) => b,
                Err(_) => {
                    unsafe { btn_lib.deref_bus(thread_bus) };
                    return;
                }
            };

            while btn_running.load(Ordering::Relaxed) {
                match unsafe { btn_lib.event_from_buffer(btn_buf, true) } {
                    Ok(ev_ptr) => {
                        let ev = unsafe { *ev_ptr };
                        unsafe { btn_lib.event_free(ev_ptr) };

                        if ev.type_ == EV_KEY {
                            let key_idx = if ev.code == 105 {
                                INPUT_BTN_X_LEFT
                            } else if ev.code == 102 {
                                INPUT_BTN_X_HOME
                            } else if ev.code == 106 {
                                INPUT_BTN_X_RIGHT
                            } else {
                                -1
                            };

                            if key_idx >= 0 {
                                let input_type = if ev.value == 0 {
                                    INPUT_BTN_RELEASE
                                } else {
                                    // Linux reports key-repeat as 2; QTFB treats it as another
                                    // press rather than a release.
                                    INPUT_BTN_PRESS
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
            unsafe { btn_lib.input_buffer_deref(btn_buf) };
            unsafe { btn_lib.deref_bus(thread_bus) };
            println!("[btn_thread] Buttons thread terminated");
        });

        let new_backend = ActiveBackend {
            shm,
            framebuffer,
            blight_buf_ptr,
            surface_id,
            client_fds: vec![client_fd],
            input_running: Arc::clone(&input_running),
        };

        backends.insert(fb_key, new_backend.clone());
        new_backend
    };
    drop(backends);

    // 4. Send response to client
    let resp = ServerMessage {
        msg_type: MESSAGE_INITIALIZE,
        payload: ServerMessageUnion {
            init: InitMessageResponseContents {
                shm_key_defined: backend_info.shm.key,
                shm_size: backend_info.shm.size,
            },
        },
    };

    if !send_server_message(client_fd, &resp) {
        println!("[server] Failed to send init response to client");
        cleanup_client_connection(fb_key, client_fd, &libblight, blight_fd);
        unsafe { libc::close(client_fd) };
        return;
    }

    let input_running = Arc::clone(&backend_info.input_running);

    // 6. Main server message loop
    let mut refresh_mode = REFRESH_MODE_UI;
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
                    UPDATE_ALL => (0, 0, dimensions.width as i32, dimensions.height as i32),
                    UPDATE_PARTIAL => (update.x, update.y, update.w, update.h),
                    other => {
                        println!("[server] Ignoring unknown update type {}", other);
                        continue;
                    }
                };

                let Some(region) = clamp_region(x, y, w, h, dimensions) else {
                    continue;
                };

                let (target_x, target_y, target_w, target_h) = unsafe {
                    let blight_buf = &*backend_info.blight_buf_ptr;
                    copy_region(
                        backend_info.shm.ptr as *const u8,
                        blight_buf.data,
                        backend_info.framebuffer,
                        Dimensions::new(profile.width, profile.height),
                        region,
                        is_rgb16,
                    )
                };

                // Map refresh_mode to waveform mode
                let waveform = match refresh_mode {
                    REFRESH_MODE_ULTRA_FAST => BlightWaveformMode::UltraFast,
                    REFRESH_MODE_FAST => BlightWaveformMode::Fast,
                    REFRESH_MODE_ANIMATE => BlightWaveformMode::Animate,
                    REFRESH_MODE_CONTENT => BlightWaveformMode::Content,
                    REFRESH_MODE_UI => BlightWaveformMode::UI,
                    _ => BlightWaveformMode::UI,
                };

                let repaint_res = libblight.surface_repaint(
                    blight_fd,
                    BlightRepaintRequest {
                        surface_id: backend_info.surface_id,
                        x: target_x as i32,
                        y: target_y as i32,
                        width: target_w,
                        height: target_h,
                        waveform,
                        content_type: profile.color_type,
                        update_mode: BlightUpdateMode::PartialUpdate,
                    },
                );
                if let Err(e) = repaint_res {
                    println!("[server] blight_surface_repaint failed: {}", e);
                }
            }
            MESSAGE_SET_REFRESH_MODE => {
                let mode = unsafe { client_msg.payload.refresh_mode };
                if !(REFRESH_MODE_ULTRA_FAST..=REFRESH_MODE_UI).contains(&mode) {
                    println!("[server] Invalid refresh mode: {}", mode);
                    break;
                }
                refresh_mode = mode;
                println!("[server] Set refresh mode: {}", refresh_mode);
            }
            MESSAGE_REQUEST_FULL_REFRESH => {
                println!("[server] MESSAGE_REQUEST_FULL_REFRESH received");
                // Trigger full refresh
                let repaint_res = libblight.surface_repaint(
                    blight_fd,
                    BlightRepaintRequest {
                        surface_id: backend_info.surface_id,
                        x: 0,
                        y: 0,
                        width: profile.width,
                        height: profile.height,
                        waveform: BlightWaveformMode::Full,
                        content_type: profile.color_type,
                        update_mode: BlightUpdateMode::FullUpdate,
                    },
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
    cleanup_client_connection(fb_key, client_fd, &libblight, blight_fd);

    unsafe { libc::close(client_fd) };
    println!("[server] Client handler exit");
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
        assert_eq!(
            default_dimensions(FBFMT_RM2FB),
            Some(Dimensions::new(1404, 1872))
        );
        assert_eq!(
            default_dimensions(FBFMT_RMPP_RGB888),
            Some(Dimensions::new(1620, 2160))
        );
        assert_eq!(
            default_dimensions(FBFMT_RMPPM_RGB888),
            Some(Dimensions::new(954, 1696))
        );
    }

    #[test]
    fn scaled_region_covers_the_corresponding_physical_area() {
        assert_eq!(
            scale_region(
                (0, 0, 1404, 1872),
                Dimensions::new(1404, 1872),
                Dimensions::new(1620, 2160),
            ),
            (0, 0, 1620, 2160)
        );
        assert_eq!(
            scale_region(
                (702, 936, 1, 1),
                Dimensions::new(1404, 1872),
                Dimensions::new(1620, 2160),
            ),
            (810, 1080, 2, 2)
        );
    }

    fn copy_unscaled_pixels(
        source_bytes: &[u8],
        source_format: FramebufferFormat,
        target_is_rgb16: bool,
    ) -> Vec<u8> {
        let dimensions = Dimensions::new(2, 1);
        let mut target = vec![0; 2 * if target_is_rgb16 { 2 } else { 4 }];
        let copied = copy_region(
            source_bytes.as_ptr(),
            target.as_mut_ptr(),
            Framebuffer {
                format: source_format,
                dimensions,
            },
            dimensions,
            (0, 0, 2, 1),
            target_is_rgb16,
        );
        assert_eq!(copied, (0, 0, 2, 1));
        target
    }

    #[test]
    fn rgb888_converts_to_rgba8888() {
        assert_eq!(
            copy_unscaled_pixels(
                &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60],
                FramebufferFormat::Rgb888,
                false
            ),
            vec![0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff]
        );
    }

    #[test]
    fn rgba8888_converts_to_rgb565() {
        let actual = copy_unscaled_pixels(
            &[0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0x00, 0xff],
            FramebufferFormat::Rgba8888,
            true,
        );
        let mut expected = Vec::new();
        expected.extend_from_slice(&0xf800_u16.to_ne_bytes());
        expected.extend_from_slice(&0x07e0_u16.to_ne_bytes());
        assert_eq!(actual, expected);
    }

    #[test]
    fn rgb565_fast_path_preserves_pixels() {
        let mut source = Vec::new();
        source.extend_from_slice(&0x1234_u16.to_ne_bytes());
        source.extend_from_slice(&0xabcd_u16.to_ne_bytes());
        assert_eq!(
            copy_unscaled_pixels(&source, FramebufferFormat::Rgb565, true),
            source
        );
    }
}
