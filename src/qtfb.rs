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

pub const MESSAGE_INITIALIZE: u8 = 0;
pub const MESSAGE_UPDATE: u8 = 1;
pub const MESSAGE_CUSTOM_INITIALIZE: u8 = 2;
pub const MESSAGE_TERMINATE: u8 = 3;
pub const MESSAGE_USERINPUT: u8 = 4;
pub const MESSAGE_SET_REFRESH_MODE: u8 = 5;
pub const MESSAGE_REQUEST_FULL_REFRESH: u8 = 6;

pub const UPDATE_ALL: i32 = 0;
pub const UPDATE_PARTIAL: i32 = 1;

pub const FBFMT_RM2FB: u8 = 0;
pub const FBFMT_RMPP_RGB888: u8 = 1;
pub const FBFMT_RMPP_RGBA8888: u8 = 2;
pub const FBFMT_RMPP_RGB565: u8 = 3;
pub const FBFMT_RMPPM_RGB888: u8 = 4;
pub const FBFMT_RMPPM_RGBA8888: u8 = 5;
pub const FBFMT_RMPPM_RGB565: u8 = 6;

pub const INPUT_TOUCH_PRESS: i32 = 0x10;
pub const INPUT_TOUCH_RELEASE: i32 = 0x11;
pub const INPUT_TOUCH_UPDATE: i32 = 0x12;

pub const INPUT_PEN_PRESS: i32 = 0x20;
pub const INPUT_PEN_RELEASE: i32 = 0x21;
pub const INPUT_PEN_UPDATE: i32 = 0x22;

pub const INPUT_BTN_PRESS: i32 = 0x30;
pub const INPUT_BTN_RELEASE: i32 = 0x31;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct InitMessageContents {
    pub framebuffer_key: i32,
    pub framebuffer_type: u8,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct CustomInitMessageContents {
    pub framebuffer_key: i32,
    pub framebuffer_type: u8,
    pub width: u16,
    pub height: u16,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct UpdateRegionMessageContents {
    pub update_type: i32, // 0 = UPDATE_ALL, 1 = UPDATE_PARTIAL
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union ClientMessageUnion {
    pub init: InitMessageContents,
    pub update: UpdateRegionMessageContents,
    pub custom_init: CustomInitMessageContents,
    pub refresh_mode: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ClientMessage {
    pub msg_type: u8,
    pub payload: ClientMessageUnion,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct InitMessageResponseContents {
    pub shm_key_defined: i32,
    pub shm_size: usize,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct UserInputContents {
    pub input_type: i32,
    pub dev_id: i32,
    pub x: i32,
    pub y: i32,
    pub d: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union ServerMessageUnion {
    pub init: InitMessageResponseContents,
    pub user_input: UserInputContents,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ServerMessage {
    pub msg_type: u8,
    pub payload: ServerMessageUnion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn test_struct_alignments_and_sizes() {
        assert_eq!(
            std::mem::offset_of!(InitMessageResponseContents, shm_key_defined),
            0
        );
        if size_of::<usize>() == 8 {
            assert_eq!(
                std::mem::offset_of!(InitMessageResponseContents, shm_size),
                8
            );
            assert_eq!(size_of::<ClientMessage>(), 24);
            assert_eq!(size_of::<ServerMessage>(), 32);
            assert_eq!(align_of::<ServerMessage>(), 8);
        } else {
            assert_eq!(
                std::mem::offset_of!(InitMessageResponseContents, shm_size),
                4
            );
            assert_eq!(size_of::<ClientMessage>(), 24);
            assert_eq!(size_of::<ServerMessage>(), 24);
            assert_eq!(align_of::<ServerMessage>(), 4);
        }
    }
}
