//! Windows backend: the few Win32 calls hyprclick needs, declared by hand so
//! there's no extra crate. Windows, the cursor, synthetic input, processes and
//! their network ports, elevation, and tray notifications.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use crate::desktop::Client;
use std::ffi::c_void;
use std::time::Duration;

pub type HWND = isize;
pub type HANDLE = isize;
type BOOL = i32;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Default)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct MSG {
    hwnd: HWND,
    pub message: u32,
    pub wParam: usize,
    lParam: isize,
    time: u32,
    pt: POINT,
    lPrivate: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouseData: u32,
    dwFlags: u32,
    time: u32,
    dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    wVk: u16,
    wScan: u16,
    dwFlags: u32,
    time: u32,
    dwExtraInfo: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
union INPUT_0 {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct INPUT {
    kind: u32,
    u: INPUT_0,
}

#[repr(C)]
struct LASTINPUTINFO {
    cbSize: u32,
    dwTime: u32,
}

#[repr(C)]
struct PROCESSENTRY32W {
    dwSize: u32,
    cntUsage: u32,
    th32ProcessID: u32,
    th32DefaultHeapID: usize,
    th32ModuleID: u32,
    cntThreads: u32,
    th32ParentProcessID: u32,
    pcPriClassBase: i32,
    dwFlags: u32,
    szExeFile: [u16; 260],
}

#[repr(C)]
struct NOTIFYICONDATAW {
    cbSize: u32,
    hWnd: HWND,
    uID: u32,
    uFlags: u32,
    uCallbackMessage: u32,
    hIcon: HANDLE,
    szTip: [u16; 128],
    dwState: u32,
    dwStateMask: u32,
    szInfo: [u16; 256],
    uVersion: u32,
    szInfoTitle: [u16; 64],
    dwInfoFlags: u32,
    guidItem: [u8; 16],
    hBalloonIcon: HANDLE,
}

type EnumProc = unsafe extern "system" fn(HWND, isize) -> BOOL;

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(cb: EnumProc, lparam: isize) -> BOOL;
    fn IsWindowVisible(hwnd: HWND) -> BOOL;
    fn IsIconic(hwnd: HWND) -> BOOL;
    fn GetWindowTextW(hwnd: HWND, text: *mut u16, max: i32) -> i32;
    fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut u32) -> u32;
    fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
    fn GetForegroundWindow() -> HWND;
    fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    fn BringWindowToTop(hwnd: HWND) -> BOOL;
    fn AttachThreadInput(attach: u32, to: u32, on: BOOL) -> BOOL;
    fn SwitchToThisWindow(hwnd: HWND, alt_tab: BOOL);
    fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
    fn GetCursorPos(point: *mut POINT) -> BOOL;
    fn SendInput(count: u32, inputs: *const INPUT, size: i32) -> u32;
    fn GetLastInputInfo(info: *mut LASTINPUTINFO) -> BOOL;
    fn GetAsyncKeyState(vk: i32) -> i16;
    pub fn RegisterHotKey(hwnd: HWND, id: i32, mods: u32, vk: u32) -> BOOL;
    pub fn UnregisterHotKey(hwnd: HWND, id: i32) -> BOOL;
    pub fn GetMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32) -> BOOL;
    pub fn PeekMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32, remove: u32) -> BOOL;
    pub fn PostThreadMessageW(thread: u32, msg: u32, wparam: usize, lparam: isize) -> BOOL;
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
    fn LoadIconW(instance: HANDLE, name: *const u16) -> HANDLE;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetCurrentThreadId() -> u32;
    fn GetTickCount() -> u32;
    fn OpenProcess(access: u32, inherit: BOOL, pid: u32) -> HANDLE;
    fn QueryFullProcessImageNameW(process: HANDLE, flags: u32, name: *mut u16, size: *mut u32) -> BOOL;
    pub fn CloseHandle(handle: HANDLE) -> BOOL;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> HANDLE;
    fn Process32FirstW(snapshot: HANDLE, entry: *mut PROCESSENTRY32W) -> BOOL;
    fn Process32NextW(snapshot: HANDLE, entry: *mut PROCESSENTRY32W) -> BOOL;
    pub fn LoadLibraryW(name: *const u16) -> HANDLE;
    pub fn GetProcAddress(module: HANDLE, name: *const u8) -> *const c_void;
    pub fn GetLastError() -> u32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn IsUserAnAdmin() -> BOOL;
    fn ShellExecuteW(hwnd: HWND, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
    fn Shell_NotifyIconW(message: u32, data: *const NOTIFYICONDATAW) -> BOOL;
}

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetExtendedTcpTable(table: *mut c_void, size: *mut u32, order: BOOL, af: u32, class: i32, reserved: u32) -> u32;
    fn GetExtendedUdpTable(table: *mut c_void, size: *mut u32, order: BOOL, af: u32, class: i32, reserved: u32) -> u32;
}

pub const WM_HOTKEY: u32 = 0x0312;
pub const WM_USER: u32 = 0x0400;
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;
pub const MOD_NOREPEAT: u32 = 0x4000;

const INPUT_MOUSE: u32 = 0;
const MOUSEEVENTF_MOVE: u32 = 0x1;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TH32CS_SNAPPROCESS: u32 = 0x2;
const INVALID_HANDLE: HANDLE = -1;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;
const TCP_TABLE_OWNER_PID_ALL: i32 = 5;
const UDP_TABLE_OWNER_PID: i32 = 1;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const SW_RESTORE: i32 = 9;

/// A NUL-terminated UTF-16 copy, for the W calls.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// The executable's file name for a process, e.g. "RobloxPlayerBeta.exe".
pub fn exe_name(pid: u32) -> Option<String> {
    // SAFETY: the handle is checked and closed; the buffer and its size go together.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process == 0 {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}

/// Every running process as (pid, exe name).
pub fn processes() -> Vec<(u32, String)> {
    let mut found = Vec::new();
    // SAFETY: the snapshot is checked and closed; the entry has its size set.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snapshot, &mut entry);
        while ok != 0 {
            found.push((entry.th32ProcessID, from_wide(&entry.szExeFile)));
            ok = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
    }
    found
}

fn table(af: u32, tcp: bool) -> Vec<u8> {
    let mut size = 0u32;
    let mut buf: Vec<u8> = Vec::new();
    for _ in 0..4 {
        // SAFETY: buf has `size` bytes (or is empty with size 0, which only asks for the size).
        let rc = unsafe {
            let ptr = if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr().cast() };
            if tcp {
                GetExtendedTcpTable(ptr, &mut size, 0, af, TCP_TABLE_OWNER_PID_ALL, 0)
            } else {
                GetExtendedUdpTable(ptr, &mut size, 0, af, UDP_TABLE_OWNER_PID, 0)
            }
        };
        match rc {
            0 if !buf.is_empty() => return buf,
            0 | ERROR_INSUFFICIENT_BUFFER => buf = vec![0; size as usize + 1024],
            _ => return Vec::new(),
        }
        size = buf.len() as u32;
    }
    Vec::new()
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// (local port, owning pid) for each row of a MIB_*_OWNER_PID table.
/// `row` is the row size, `port` and `pid` the offsets in it.
fn rows(buf: &[u8], row: usize, port: usize, pid: usize) -> Vec<(u16, u32)> {
    let count = u32_at(buf, 0).unwrap_or(0) as usize;
    (0..count)
        .filter_map(|i| {
            let at = 4 + i * row;
            // The port sits in the low 16 bits, in network order.
            let raw = u32_at(buf, at + port)? as u16;
            Some((u16::from_be(raw), u32_at(buf, at + pid)?))
        })
        .collect()
}

/// Every process that owns a TCP or UDP socket.
pub fn networked_pids() -> std::collections::HashSet<u32> {
    let mut pids = std::collections::HashSet::new();
    for (buf, row, port, pid) in [
        (table(AF_INET, true), 24, 8, 20),
        (table(AF_INET6, true), 56, 20, 52),
        (table(AF_INET, false), 12, 4, 8),
        (table(AF_INET6, false), 28, 20, 24),
    ] {
        pids.extend(rows(&buf, row, port, pid).into_iter().map(|(_, pid)| pid));
    }
    pids
}

/// Local TCP and UDP ports (IPv4 and IPv6) owned by any of these processes.
pub fn ports_of(pids: &[u32]) -> (Vec<u16>, Vec<u16>) {
    let mine = |rows: Vec<(u16, u32)>| -> Vec<u16> {
        rows.into_iter().filter(|(_, pid)| pids.contains(pid)).map(|(port, _)| port).collect()
    };
    let mut tcp = mine(rows(&table(AF_INET, true), 24, 8, 20));
    tcp.extend(mine(rows(&table(AF_INET6, true), 56, 20, 52)));
    let mut udp = mine(rows(&table(AF_INET, false), 12, 4, 8));
    udp.extend(mine(rows(&table(AF_INET6, false), 28, 20, 24)));
    for list in [&mut tcp, &mut udp] {
        list.sort_unstable();
        list.dedup();
    }
    (tcp, udp)
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer and its length go together.
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: isize) -> BOOL {
    // SAFETY: lparam is the &mut Vec<HWND> clients() passed in, alive for the call.
    let list = unsafe { &mut *(lparam as *mut Vec<HWND>) };
    list.push(hwnd);
    1
}

/// Visible top-level windows with a title, front to back. The class is the
/// owning program's exe name, the address the window handle.
pub fn clients() -> Vec<Client> {
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: collect only pushes into the Vec passed as lparam, which outlives the call.
    unsafe { EnumWindows(collect, (&raw mut handles) as isize) };
    let front = foreground();
    handles
        .into_iter()
        .enumerate()
        .filter_map(|(z, hwnd)| {
            // SAFETY: plain queries on a window handle; a stale one just fails.
            unsafe {
                if IsWindowVisible(hwnd) == 0 {
                    return None;
                }
                let title = window_title(hwnd);
                if title.is_empty() {
                    return None;
                }
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                let mut r = RECT::default();
                GetWindowRect(hwnd, &mut r);
                Some(Client {
                    address: format!("{hwnd:#x}"),
                    class: exe_name(pid).unwrap_or_else(|| format!("pid {pid}")),
                    title,
                    at: [r.left as f64, r.top as f64],
                    size: [(r.right - r.left) as f64, (r.bottom - r.top) as f64],
                    xwayland: None,
                    workspace: String::new(),
                    visible: IsIconic(hwnd) == 0,
                    recency: if hwnd == front { -1 } else { z as i64 },
                })
            }
        })
        .collect()
}

fn foreground() -> HWND {
    // SAFETY: no preconditions.
    unsafe { GetForegroundWindow() }
}

pub fn active_address() -> Option<String> {
    let hwnd = foreground();
    (hwnd != 0).then(|| format!("{hwnd:#x}"))
}

fn parse_address(address: &str) -> Option<HWND> {
    isize::from_str_radix(address.trim_start_matches("0x"), 16).ok()
}

pub fn cursor_pos() -> Option<(f64, f64)> {
    let mut p = POINT::default();
    // SAFETY: p is valid for writing.
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x as f64, p.y as f64))
}

/// GetTickCount when hyprclick last sent input, so its own wiggles don't
/// count as you using the PC.
static LAST_SENT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn send(inputs: &[INPUT]) -> Result<(), String> {
    // SAFETY: no preconditions.
    LAST_SENT.store(unsafe { GetTickCount() }, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: the slice and its length go together, and INPUT's size is passed.
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err("Windows blocked the input (is the game running as administrator?)".to_string())
    }
}

fn mouse(dx: i32, dy: i32, flags: u32) -> INPUT {
    INPUT { kind: INPUT_MOUSE, u: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
}

/// Press or release a mouse button, by its Linux button code (BTN_LEFT = 272...).
pub fn mouse_button(code: u16, down: bool) -> Result<(), String> {
    // MOUSEEVENTF_{LEFT,RIGHT,MIDDLE}{DOWN,UP}
    let (press, release) = match code {
        273 => (0x0008, 0x0010),
        274 => (0x0020, 0x0040),
        _ => (0x0002, 0x0004),
    };
    send(&[mouse(0, 0, if down { press } else { release })])
}

pub fn mouse_move(dx: i32, dy: i32) -> Result<(), String> {
    send(&[mouse(dx, dy, MOUSEEVENTF_MOVE)])
}

/// Milliseconds since you last touched the keyboard or mouse. Input
/// hyprclick sent itself doesn't count.
pub fn idle_ms() -> u32 {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: info has its size set and is valid for writing.
    unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return 0;
        }
        let ours = LAST_SENT.load(std::sync::atomic::Ordering::Relaxed);
        if info.dwTime.wrapping_sub(ours) < 1000 {
            return u32::MAX;
        }
        GetTickCount().wrapping_sub(info.dwTime)
    }
}

/// Is this virtual key held down right now?
pub fn key_down(vk: u32) -> bool {
    // SAFETY: no preconditions.
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

/// Bring a window to the front. Windows only lets the foreground app do
/// that, so join the foreground window's input queue for a moment, which
/// counts as being it.
pub fn focus(address: &str) -> bool {
    let Some(hwnd) = parse_address(address) else { return false };
    // SAFETY: plain calls on window handles and thread ids; stale ones just fail,
    // and the input queues are detached again before returning.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        let front = GetForegroundWindow();
        let front_thread = GetWindowThreadProcessId(front, std::ptr::null_mut());
        let me = GetCurrentThreadId();
        let attached = front_thread != 0 && front_thread != me && AttachThreadInput(me, front_thread, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(me, front_thread, 0);
        }
        if in_front(hwnd) {
            return true;
        }
        // What Alt+Tab uses; it gets past windows that refuse the queue trick.
        SwitchToThisWindow(hwnd, 1);
        in_front(hwnd)
    }
}

/// Windows switches the front window asynchronously, so give it a moment.
fn in_front(hwnd: HWND) -> bool {
    for _ in 0..15 {
        if foreground() == hwnd {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

pub fn focus_class(class: &str) {
    if let Some(window) = clients().into_iter().find(|c| c.class.eq_ignore_ascii_case(class) || c.title == class) {
        focus(&window.address);
    }
}

/// Anti-AFK on Windows: switch to the game, wiggle the mouse (or right-click)
/// and switch back. Skipped while you're using the game yourself.
pub fn poke(address: &str, dx: i32, dy: i32, right_click: bool, hold: Duration) -> Result<bool, String> {
    let before = foreground();
    if active_address().as_deref() == Some(address) && idle_ms() < 60_000 {
        return Ok(false);
    }
    if !focus(address) {
        let front = window_title(foreground());
        return Err(format!("Windows didn't let hyprclick switch to the game (\"{front}\" is holding the front)"));
    }
    std::thread::sleep(Duration::from_millis(150));
    if right_click {
        send(&[mouse(0, 0, 0x0008)])?;
        std::thread::sleep(hold);
        send(&[mouse(0, 0, 0x0010)])?;
    } else {
        mouse_move(dx, dy)?;
        std::thread::sleep(hold);
        mouse_move(-dx, -dy)?;
    }
    std::thread::sleep(Duration::from_millis(100));
    // Best effort: the nudge is done, and staying on the game while you're away does no harm.
    let back = format!("{before:#x}");
    if before != 0 && back != address {
        focus(&back);
    }
    Ok(true)
}

pub fn is_admin() -> bool {
    // SAFETY: no preconditions.
    unsafe { IsUserAnAdmin() != 0 }
}

/// Start this program again with "Run as administrator". False if refused.
pub fn relaunch_as_admin() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = wide(&exe.to_string_lossy());
    let verb = wide("runas");
    // SAFETY: every string is NUL-terminated and outlives the call.
    let rc = unsafe { ShellExecuteW(0, verb.as_ptr(), exe.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
    rc > 32
}

/// A tray balloon, which Windows 10 and 11 show as a toast. A window lives
/// as long as the thread that made it, so one thread owns the tray icon and
/// shows whatever is queued for it.
pub fn notify(text: &str) {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, OnceLock};
    type Queue = Arc<Mutex<VecDeque<String>>>;
    static NOTIFIER: OnceLock<Option<(u32, Queue)>> = OnceLock::new();
    let notifier = NOTIFIER.get_or_init(|| {
        let queue = Queue::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = queue.clone();
        std::thread::spawn(move || tray_thread(shared, tx));
        rx.recv().ok().map(|thread| (thread, queue))
    });
    if let Some((thread, queue)) = notifier {
        queue.lock().unwrap().push_back(text.to_string());
        // SAFETY: posting a plain message to a thread that runs a message loop.
        unsafe { PostThreadMessageW(*thread, WM_USER, 0, 0) };
    }
}

fn tray_thread(queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>, ready: std::sync::mpsc::Sender<u32>) {
    const NIM_ADD: u32 = 0;
    const NIM_MODIFY: u32 = 1;
    const NIF_ICON: u32 = 0x2;
    const NIF_TIP: u32 = 0x4;
    const NIF_INFO: u32 = 0x10;
    const HWND_MESSAGE: HWND = -3;
    let class = wide("STATIC");
    let name = wide("hyprclick");
    // SAFETY: the strings outlive the call; a message-only window needs nothing else.
    let hwnd = unsafe { CreateWindowExW(0, class.as_ptr(), name.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, 0, 0, std::ptr::null()) };
    if hwnd == 0 {
        return;
    }
    // SAFETY: no preconditions. The window gives this thread its message queue.
    let _ = ready.send(unsafe { GetCurrentThreadId() });
    let copy = |dst: &mut [u16], src: &str| {
        let room = dst.len() - 1;
        for (d, s) in dst.iter_mut().zip(src.encode_utf16().take(room)) {
            *d = s;
        }
    };
    let mut added = false;
    let mut msg = MSG::default();
    // SAFETY: msg is valid for writing; 0 for hwnd takes every message of this thread.
    while unsafe { GetMessageW(&mut msg, 0, 0, 0) } > 0 {
        if msg.message != WM_USER {
            continue;
        }
        while let Some(text) = queue.lock().unwrap().pop_front() {
            // SAFETY: an all-zero NOTIFYICONDATAW is valid; 32512 is the stock IDI_APPLICATION icon.
            unsafe {
                let mut data: NOTIFYICONDATAW = std::mem::zeroed();
                data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                data.hWnd = hwnd;
                data.uID = 1;
                data.uFlags = NIF_ICON | NIF_TIP | NIF_INFO;
                data.hIcon = LoadIconW(0, 32512 as *const u16);
                copy(&mut data.szTip, "hyprclick");
                copy(&mut data.szInfoTitle, "hyprclick");
                copy(&mut data.szInfo, &text);
                Shell_NotifyIconW(if added { NIM_MODIFY } else { NIM_ADD }, &data);
            }
            added = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_port_tables() {
        // Two IPv4 UDP rows: addr, port (network order), pid.
        let mut buf = 2u32.to_ne_bytes().to_vec();
        for (port, pid) in [(50153u16, 7u32), (53, 9)] {
            buf.extend(0u32.to_ne_bytes());
            buf.extend(u32::from(port.to_be()).to_ne_bytes());
            buf.extend(pid.to_ne_bytes());
        }
        assert_eq!(rows(&buf, 12, 4, 8), [(50153, 7), (53, 9)]);
        assert!(rows(&buf[..10], 12, 4, 8).is_empty());
    }

    #[test]
    fn input_has_the_win32_size() {
        assert_eq!(std::mem::size_of::<INPUT>(), 40);
        assert_eq!(std::mem::size_of::<PROCESSENTRY32W>(), 568);
        assert_eq!(std::mem::size_of::<NOTIFYICONDATAW>(), 976);
    }
}
