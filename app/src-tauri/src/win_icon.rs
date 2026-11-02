// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Crisp window and taskbar icons on Windows.
//!
//! Tauri gives Windows one RGBA image for the window icon (the largest entry
//! of `icon.ico`, 256 px), and Windows shrinks it to 16–32 px with a crude
//! resize, so the taskbar icon looks jagged. Instead, load the icon that
//! tauri-build embeds in the exe at the exact small and big sizes for the
//! window's DPI; `LoadImageW` then picks the best frame from the .ico.

use std::ffi::c_void;

use tauri::Manager;

type Handle = *mut c_void;

const IMAGE_ICON: u32 = 1;
const LR_DEFAULTCOLOR: u32 = 0;
const WM_SETICON: u32 = 0x0080;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;
const SM_CXICON: i32 = 11;
const SM_CYICON: i32 = 12;
const SM_CXSMICON: i32 = 49;
const SM_CYSMICON: i32 = 50;

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Handle;
}

#[link(name = "user32")]
extern "system" {
    fn LoadImageW(
        inst: Handle,
        name: *const u16,
        kind: u32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> Handle;
    fn SendMessageW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetDpiForWindow(hwnd: Handle) -> u32;
    fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
}

/// Replaces the main window's small and big icons with ones loaded from the
/// exe's icon resource. Leaves Tauri's icon in place if anything fails.
pub fn apply(app: &tauri::App) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = hwnd.0 as Handle;
    // MAKEINTRESOURCE: the resource id passed as a pointer value.
    let id = tauri::utils::platform::WINDOWS_APP_ICON_RESOURCE_ID as usize as *const u16;
    unsafe {
        let inst = GetModuleHandleW(std::ptr::null());
        let dpi = GetDpiForWindow(hwnd).max(96);
        for (which, cx, cy) in [
            (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
            (ICON_BIG, SM_CXICON, SM_CYICON),
        ] {
            let (w, h) = (
                GetSystemMetricsForDpi(cx, dpi),
                GetSystemMetricsForDpi(cy, dpi),
            );
            let icon = LoadImageW(inst, id, IMAGE_ICON, w, h, LR_DEFAULTCOLOR);
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, which, icon as isize);
            }
        }
    }
}
