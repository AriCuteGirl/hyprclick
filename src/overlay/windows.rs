//! Windows: a layered, topmost, click-through tool window filled with
//! UpdateLayeredWindow. lagswitch's engine lives in the hyprclick window's
//! process here, so the overlay is a thread in it too, asking the engine for
//! its status ten times a second. It shows over borderless fullscreen games
//! (Roblox's kind), not exclusive fullscreen ones.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use super::{Banner, Feed, Fonts, TICK, W, height, render};
use crate::config::{Config, OverlayPlace};
use crate::win::{HWND, MSG, PeekMessageW, POINT, wide};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

type HANDLE = isize;
type BOOL = i32;

#[repr(C)]
struct SIZE {
    cx: i32,
    cy: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy, PartialEq)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Default)]
struct MONITORINFO {
    cbSize: u32,
    rcMonitor: RECT,
    rcWork: RECT,
    dwFlags: u32,
}

#[repr(C)]
struct BLENDFUNCTION {
    BlendOp: u8,
    BlendFlags: u8,
    SourceConstantAlpha: u8,
    AlphaFormat: u8,
}

#[repr(C)]
#[derive(Default)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
#[derive(Default)]
struct BITMAPINFO {
    bmiHeader: BITMAPINFOHEADER,
    bmiColors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HANDLE,
        instance: HANDLE,
        param: *const c_void,
    ) -> HWND;
    fn UpdateLayeredWindow(
        hwnd: HWND,
        screen: HANDLE,
        dst: *const POINT,
        size: *const SIZE,
        src_dc: HANDLE,
        src: *const POINT,
        key: u32,
        blend: *const BLENDFUNCTION,
        flags: u32,
    ) -> BOOL;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
    fn SetWindowPos(hwnd: HWND, after: HWND, x: i32, y: i32, w: i32, h: i32, flags: u32) -> BOOL;
    fn GetForegroundWindow() -> HWND;
    fn MonitorFromWindow(hwnd: HWND, flags: u32) -> HANDLE;
    fn GetMonitorInfoW(monitor: HANDLE, info: *mut MONITORINFO) -> BOOL;
    fn TranslateMessage(msg: *const MSG) -> BOOL;
    fn DispatchMessageW(msg: *const MSG) -> isize;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: HANDLE) -> HANDLE;
    fn CreateDIBSection(dc: HANDLE, info: *const BITMAPINFO, usage: u32, bits: *mut *mut c_void, section: HANDLE, offset: u32) -> HANDLE;
    fn SelectObject(dc: HANDLE, object: HANDLE) -> HANDLE;
    fn DeleteObject(object: HANDLE) -> BOOL;
    fn DeleteDC(dc: HANDLE) -> BOOL;
}

const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x8;
const WS_EX_TRANSPARENT: u32 = 0x20;
const WS_EX_TOOLWINDOW: u32 = 0x80;
const WS_EX_LAYERED: u32 = 0x8_0000;
const WS_EX_NOACTIVATE: u32 = 0x800_0000;
const SW_HIDE: i32 = 0;
const HWND_TOPMOST: HWND = -1;
const SWP_NOSIZE: u32 = 0x1;
const SWP_NOMOVE: u32 = 0x2;
const SWP_NOACTIVATE: u32 = 0x10;
const SWP_SHOWWINDOW: u32 = 0x40;
const MONITOR_DEFAULTTOPRIMARY: u32 = 1;
const ULW_ALPHA: u32 = 2;
const AC_SRC_ALPHA: u8 = 1;
const PM_REMOVE: u32 = 1;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Start the overlay thread if it's turned on and isn't running yet.
pub fn ensure_running() {
    if !Config::load().lag_overlay || !crate::lag::installed() || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| run(crate::lag::status_text, None));
}

/// `hyprclick overlay-demo`: a made-up 3 s timed freeze through the real
/// window, then the "off" flash, to check the drawing without the driver.
pub fn demo() {
    let start = std::time::Instant::now();
    let status = move || {
        let left = 3000u128.saturating_sub(start.elapsed().as_millis());
        Some(if left > 0 { format!("frozen timed both {left} demo") } else { "idle".to_string() })
    };
    run(status, Some(std::time::Duration::from_secs(5)));
}

/// Show banners for `status` until `stop_after`, or for good.
fn run(mut status: impl FnMut() -> Option<String>, stop_after: Option<std::time::Duration>) {
    let started = std::time::Instant::now();
    let Ok(fonts) = Fonts::load() else { return };
    let class = wide("STATIC");
    let name = wide("hyprclick overlay");
    let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    // SAFETY: the strings outlive the call. A layered popup of the stock STATIC
    // class needs no window procedure of ours.
    let hwnd = unsafe { CreateWindowExW(ex, class.as_ptr(), name.as_ptr(), WS_POPUP, 0, 0, 0, 0, 0, 0, 0, std::ptr::null()) };
    if hwnd == 0 {
        return;
    }
    let mut feed = Feed::new();
    let mut shown: Option<(Banner, POINT, i32)> = None;
    loop {
        pump();
        if stop_after.is_some_and(|d| started.elapsed() >= d) {
            return;
        }
        match feed.next(status()) {
            None => {
                if shown.take().is_some() {
                    // SAFETY: hwnd is our window.
                    unsafe { ShowWindow(hwnd, SW_HIDE) };
                }
            }
            Some(banner) => {
                let h = height(feed.place) as i32;
                let at = position(feed.place, h);
                let same = shown.as_ref().is_some_and(|(b, p, sh)| *b == banner && p.x == at.x && p.y == at.y && *sh == h);
                if !same && draw(hwnd, &banner, &fonts, at, h) {
                    shown = Some((banner, at, h));
                }
            }
        }
        std::thread::sleep(TICK);
    }
}

/// Handle whatever Windows sent the window, so it never counts as hung.
fn pump() {
    let mut msg = MSG::default();
    // SAFETY: msg is valid for writing; 0 takes every message of this thread.
    while unsafe { PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) } != 0 {
        // SAFETY: msg came from PeekMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Top left corner on the monitor of the window in front, where the game is.
fn position(place: OverlayPlace, h: i32) -> POINT {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: info is valid for writing with cbSize set; the primary monitor is the fallback.
    unsafe { GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut info) };
    let (screen, work) = (info.rcMonitor, info.rcWork);
    let x = screen.left + (screen.right - screen.left - W as i32) / 2;
    let y = match place {
        // The work area leaves out the taskbar.
        OverlayPlace::Top => work.top + 16,
        OverlayPlace::Middle => screen.top + (screen.bottom - screen.top - h) / 2,
        OverlayPlace::Bottom => work.bottom - 140 - h,
    };
    POINT { x, y }
}

/// Put the banner on screen at `at`. False if Windows refused.
fn draw(hwnd: HWND, banner: &Banner, fonts: &Fonts, at: POINT, h: i32) -> bool {
    let pixels = render(banner, fonts);
    let mut info = BITMAPINFO::default();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: W as i32,
        // Negative: rows go top to bottom, like the pixels.
        biHeight: -h,
        biPlanes: 1,
        biBitCount: 32,
        ..Default::default()
    };
    // SAFETY: every handle made here is checked and freed before returning; the
    // DIB holds W * h 32-bit pixels, which is what gets copied in.
    unsafe {
        let dc = CreateCompatibleDC(0);
        if dc == 0 {
            return false;
        }
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &info, 0, &mut bits, 0, 0);
        if bitmap == 0 || bits.is_null() {
            DeleteDC(dc);
            return false;
        }
        // Premultiplied BGRA in memory, which is the u32 ARGB render() makes.
        let count = (W as usize * h as usize).min(pixels.len());
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, count);
        let old = SelectObject(dc, bitmap);
        let size = SIZE { cx: W as i32, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA };
        let ok = UpdateLayeredWindow(hwnd, 0, &at, &size, dc, &src, 0, &blend, ULW_ALPHA) != 0;
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        // Above everything, including windows that went topmost after us.
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_have_the_win32_size() {
        assert_eq!(std::mem::size_of::<BITMAPINFOHEADER>(), 40);
        assert_eq!(std::mem::size_of::<BITMAPINFO>(), 44);
        assert_eq!(std::mem::size_of::<MONITORINFO>(), 40);
        assert_eq!(std::mem::size_of::<BLENDFUNCTION>(), 4);
    }
}
