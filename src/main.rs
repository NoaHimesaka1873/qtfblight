#![cfg_attr(
    target_arch = "arm",
    feature(
        arm_target_feature,
        stdarch_arm_feature_detection,
        stdarch_arm_neon_intrinsics
    )
)]

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

pub mod blight;
pub mod device;
pub mod qtfb;
pub mod server;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(true);
pub static RESUMED: AtomicBool = AtomicBool::new(false);

#[derive(Copy, Clone)]
pub struct SendPtr(pub *mut std::os::raw::c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

impl SendPtr {
    pub fn get(self) -> *mut std::os::raw::c_void {
        self.0
    }
}

extern "C" fn handle_sigint(_: libc::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

extern "C" fn handle_sigcont(_: libc::c_int) {
    RESUMED.store(true, Ordering::SeqCst);
}

fn main() {
    // 1. Setup Signal Handlers
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_sigint as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_sigint as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGCONT,
            handle_sigcont as *const () as libc::sighandler_t,
        );
    }

    // Get socket path from environment
    let socket_path = std::env::var("QTFB_SOCKET").unwrap_or_else(|_| "/tmp/qtfb.sock".to_string());

    // 2. Load LibBlight
    let libblight = match blight::LibBlight::load() {
        Ok(l) => Arc::new(l),
        Err(e) => {
            eprintln!("[main] Error loading libblight_protocol: {}", e);
            std::process::exit(1);
        }
    };

    // 3. Device detection
    let dev_type = device::detect_device_type();
    let profile = device::DeviceProfile::new(dev_type);
    println!(
        "[main] Starting qtfblight on {} (format: {:?})",
        profile.name, profile.format
    );

    // 4. DBus Connection
    let bus = match libblight.connect_bus() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[main] Error connecting to DBus: {}", e);
            std::process::exit(1);
        }
    };

    // 5. Open Blight Service
    let blight_fd = match unsafe { libblight.service_open(bus) } {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("[main] Error opening blight service: {}", e);
            unsafe { libblight.deref_bus(bus) };
            std::process::exit(1);
        }
    };

    // 6. Bind socket listener
    let listener = match server::SeqPacketListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[main] Error binding SeqPacket socket {}: {}",
                socket_path, e
            );
            unsafe { libc::close(blight_fd) };
            unsafe { libblight.deref_bus(bus) };
            std::process::exit(1);
        }
    };

    // The qtfb protocol has no authentication and the tablet is single-user;
    // let unprivileged clients (e.g. sway running as a normal user inside a
    // chroot) connect. bind() masks the socket mode with umask, so under
    // root's usual 022 the socket would otherwise be root-connect-only.
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(
            &socket_path,
            std::fs::Permissions::from_mode(0o666),
        ) {
            eprintln!(
                "[main] Warning: failed to set permissions on {}: {}",
                socket_path, e
            );
        }
    }

    // Generate the shared memory key
    let key = server::generate_random_key();

    // Spawn child process if arguments or _QTFBLIGHT_COMMAND environment variable are provided
    let args: Vec<String> = std::env::args().collect();
    let mut child = None;
    let mut launch_cmd = None;

    if args.len() > 1 {
        launch_cmd = Some((args[1].clone(), args[2..].to_vec(), false));
    } else if let Ok(cmd) = std::env::var("_QTFBLIGHT_COMMAND") {
        launch_cmd = Some(("sh".to_string(), vec!["-c".to_string(), cmd], true));
    }

    if let Some((bin, cmd_args, use_shell)) = launch_cmd {
        let child_res = std::process::Command::new(&bin)
            .args(&cmd_args)
            .env("QTFB_SOCKET", &socket_path)
            .env("QTFB_KEY", key.to_string())
            .spawn();
        match child_res {
            Ok(c) => {
                child = Some(c);
                if use_shell {
                    println!("[main] Spawned child shell process for _QTFBLIGHT_COMMAND");
                } else {
                    println!("[main] Spawned child process: {:?}", bin);
                }
            }
            Err(e) => {
                eprintln!("[main] Failed to spawn child process: {}", e);
                unsafe { libc::close(blight_fd) };
                unsafe { libblight.deref_bus(bus) };
                std::process::exit(1);
            }
        }
    }

    // One connection thread owns acknowledgement processing for this service
    // FD. All surfaces share that FD, so creating one thread per surface is
    // invalid and makes teardown race delete acknowledgements.
    let blight_thread = match libblight.start_connection_thread(blight_fd) {
        Ok(thread) => thread,
        Err(e) => {
            eprintln!("[main] Error starting blight connection thread: {}", e);
            unsafe { libc::close(blight_fd) };
            unsafe { libblight.deref_bus(bus) };
            std::process::exit(1);
        }
    };

    let running_arc = Arc::new(AtomicBool::new(true));
    // Keep the shared libblight service alive until all client handlers have
    // finished destroying their surfaces.
    let active_handlers = Arc::new(AtomicUsize::new(0));
    let mut pollfd = libc::pollfd {
        fd: listener.fd,
        events: libc::POLLIN,
        revents: 0,
    };

    println!("[main] Server listening on {}...", socket_path);

    let mut child_exit_code = 0;

    while RUNNING.load(Ordering::SeqCst) {
        if let Some(ref mut c) = child {
            match c.try_wait() {
                Ok(Some(status)) => {
                    println!("[main] Child process exited with status: {}", status);
                    child_exit_code = status.code().unwrap_or(0);
                    RUNNING.store(false, Ordering::SeqCst);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[main] Error checking child status: {}", e);
                    RUNNING.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }

        let res = unsafe { libc::poll(&mut pollfd, 1, 500) }; // 500ms timeout
        if res < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("[main] poll() error: {}", err);
            break;
        }

        if res > 0 && (pollfd.revents & libc::POLLIN) != 0 {
            match listener.accept() {
                Ok(client_fd) => {
                    let libblight_clone = Arc::clone(&libblight);
                    let profile_clone = profile.clone();
                    let running_arc_clone = Arc::clone(&running_arc);
                    let active_handlers_clone = Arc::clone(&active_handlers);
                    let bus_send = SendPtr(bus);
                    active_handlers.fetch_add(1, Ordering::SeqCst);
                    std::thread::spawn(move || {
                        unsafe {
                            server::handle_client(
                                client_fd,
                                libblight_clone,
                                bus_send.get(),
                                blight_fd,
                                profile_clone,
                                running_arc_clone,
                                key,
                            );
                        }
                        active_handlers_clone.fetch_sub(1, Ordering::SeqCst);
                    });
                }
                Err(e) => {
                    eprintln!("[main] Error accepting client: {}", e);
                }
            }
        }
    }

    running_arc.store(false, Ordering::SeqCst);
    while active_handlers.load(Ordering::SeqCst) != 0 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    println!("[main] Shutting down, cleaning up socket...");
    // The listener owns and removes the socket path when it is dropped.
    // The thread must outlive every surface deletion, but be joined before the
    // service FD itself is closed.
    unsafe { libblight.connection_thread_deref(blight_thread) };
    unsafe { libc::close(blight_fd) };
    unsafe { libblight.deref_bus(bus) };
    println!("[main] Shutdown complete");

    if child.is_some() {
        std::process::exit(child_exit_code);
    }
}
