//! The Lag tab on Windows: lagswitch's freezes and fake lag, run inside
//! hyprclick over WinDivert (WinDivert.dll and WinDivert64.sys next to the
//! exe; it needs "Run as administrator").
//!
//! WinDivert hands every non-loopback TCP and UDP packet to us while a freeze
//! or fake lag is on, and we send back whatever isn't dropped or held. A
//! packet belongs to the target app when one of its processes owns the local
//! port, which is re-read twice a second, so restarts are followed too.
//! Whole PC freezes take every non-loopback packet.
//!
//! The rules (toggle, hold, caps) and fake lag's timing come from lagswitch
//! itself, so both platforms behave the same.

use crate::win;
use lagswitch::cli::Backend;
use lagswitch::proto::{FreezeRequest, Request, Target};
use lagswitch::rules::Direction;
use lagswitch::shape::{Schedule, Shape};
use lagswitch::state::{self, Active, Decision};
use std::collections::{HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(100);
const REFRESH_EVERY: Duration = Duration::from_millis(500);
/// WINDIVERT_MTU_MAX.
const MTU: usize = 40 + 0xFFFF;

/// WINDIVERT_ADDRESS: an INT64 timestamp, a bitfield word, a reserved word and
/// a 64-byte union. Bit 17 of the bitfield is Outbound.
#[repr(C)]
#[derive(Clone, Copy)]
struct Address {
    timestamp: i64,
    flags: u32,
    reserved: u32,
    data: [u8; 64],
}

impl Address {
    fn outbound(&self) -> bool {
        self.flags & (1 << 17) != 0
    }
}

type Open = unsafe extern "C" fn(*const u8, i32, i16, u64) -> win::HANDLE;
type Recv = unsafe extern "C" fn(win::HANDLE, *mut c_void, u32, *mut u32, *mut Address) -> i32;
type Send = unsafe extern "C" fn(win::HANDLE, *const c_void, u32, *mut u32, *const Address) -> i32;
type Shutdown = unsafe extern "C" fn(win::HANDLE, i32) -> i32;
type Close = unsafe extern "C" fn(win::HANDLE) -> i32;

/// WinDivert.dll's entry points, loaded from the exe's folder.
struct Dll {
    open: Open,
    recv: Recv,
    send: Send,
    shutdown: Shutdown,
    close: Close,
}

fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf())
}

/// WinDivert's files are next to the exe.
pub fn available() -> bool {
    exe_dir().is_some_and(|d| d.join("WinDivert.dll").exists() && d.join("WinDivert64.sys").exists())
}

fn dll() -> Result<&'static Dll, String> {
    static DLL: OnceLock<Result<Dll, String>> = OnceLock::new();
    DLL.get_or_init(|| {
        let path = exe_dir().ok_or("can't find hyprclick's folder")?.join("WinDivert.dll");
        let name = win::wide(&path.to_string_lossy());
        // SAFETY: the name is NUL-terminated; each symbol is checked before it's
        // turned into a function pointer of WinDivert's documented signature.
        unsafe {
            let module = win::LoadLibraryW(name.as_ptr());
            if module == 0 {
                return Err("WinDivert.dll is missing, keep it next to hyprclick.exe".to_string());
            }
            let get = |symbol: &[u8]| {
                let f = win::GetProcAddress(module, symbol.as_ptr());
                (!f.is_null()).then_some(f).ok_or_else(|| "WinDivert.dll is the wrong version, use 2.2".to_string())
            };
            Ok(Dll {
                open: std::mem::transmute::<*const c_void, Open>(get(b"WinDivertOpen\0")?),
                recv: std::mem::transmute::<*const c_void, Recv>(get(b"WinDivertRecv\0")?),
                send: std::mem::transmute::<*const c_void, Send>(get(b"WinDivertSend\0")?),
                shutdown: std::mem::transmute::<*const c_void, Shutdown>(get(b"WinDivertShutdown\0")?),
                close: std::mem::transmute::<*const c_void, Close>(get(b"WinDivertClose\0")?),
            })
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// The protocol and local port of a raw IPv4 or IPv6 packet.
fn local_port(packet: &[u8], outbound: bool) -> Option<(u8, u16)> {
    let (proto, header) = match packet.first()? >> 4 {
        4 => (*packet.get(9)?, usize::from(packet.first()? & 0x0f) * 4),
        6 => (*packet.get(6)?, 40),
        _ => return None,
    };
    if proto != 6 && proto != 17 {
        return None;
    }
    let at = header + if outbound { 0 } else { 2 };
    Some((proto, u16::from_be_bytes(packet.get(at..at + 2)?.try_into().ok()?)))
}

#[derive(Default, Clone)]
struct Ports {
    tcp: HashSet<u16>,
    udp: HashSet<u16>,
}

impl Ports {
    fn of(pids: &[u32]) -> Self {
        let (tcp, udp) = win::ports_of(pids);
        Self { tcp: tcp.into_iter().collect(), udp: udp.into_iter().collect() }
    }

    fn has(&self, proto: u8, port: u16) -> bool {
        if proto == 6 { self.tcp.contains(&port) } else { self.udp.contains(&port) }
    }
}

/// What the packet thread does, kept up to date by the engine.
#[derive(Default, Clone)]
struct Plan {
    whole_pc: bool,
    freeze: Option<(Ports, Direction)>,
    lag: Option<(Ports, Shape)>,
}

impl Plan {
    fn capturing(&self) -> bool {
        self.whole_pc || self.freeze.is_some() || self.lag.is_some()
    }
}

enum Verdict {
    Pass,
    Drop,
    Hold,
}

fn verdict(plan: &Plan, packet: &[u8], outbound: bool) -> Verdict {
    if plan.whole_pc {
        return Verdict::Drop;
    }
    let Some((proto, port)) = local_port(packet, outbound) else { return Verdict::Pass };
    if let Some((ports, direction)) = &plan.freeze {
        let blocked = match direction {
            Direction::In => !outbound,
            Direction::Out => outbound,
            Direction::Both => true,
        };
        if blocked && ports.has(proto, port) {
            return Verdict::Drop;
        }
    }
    match &plan.lag {
        Some((ports, shape)) if ports.has(proto, port) && (outbound || shape.delays_inbound()) => Verdict::Hold,
        _ => Verdict::Pass,
    }
}

/// Held packets, in the order they go out.
#[derive(Default)]
struct Held {
    out: VecDeque<(Instant, Vec<u8>, Address)>,
    inbound: VecDeque<(Instant, Vec<u8>, Address)>,
}

impl Held {
    fn next_due(&self) -> Option<Instant> {
        [self.out.front(), self.inbound.front()].into_iter().flatten().map(|(due, _, _)| *due).min()
    }
}

/// One open WinDivert handle and the two threads that serve it.
struct Diverter {
    handle: win::HANDLE,
    whole_pc: bool,
    stop: Arc<AtomicBool>,
}

struct Shared {
    plan: RwLock<Plan>,
    schedule: Mutex<Option<Schedule>>,
    held: Mutex<Held>,
    wake: Condvar,
    counts: (AtomicU64, AtomicU64),
}

fn open(whole_pc: bool) -> Result<win::HANDLE, String> {
    let dll = dll()?;
    let filter: &[u8] = if whole_pc { b"!loopback\0" } else { b"!loopback and (tcp or udp)\0" };
    // SAFETY: the filter is NUL-terminated; layer 0 is the network layer.
    let handle = unsafe { (dll.open)(filter.as_ptr(), 0, 0, 0) };
    if handle != -1 {
        return Ok(handle);
    }
    // SAFETY: no preconditions.
    Err(match unsafe { win::GetLastError() } {
        5 => "the Lag tab needs hyprclick running as administrator".to_string(),
        2 => "WinDivert64.sys is missing, keep it next to hyprclick.exe".to_string(),
        577 | 1275 => "Windows blocked WinDivert's driver (antivirus or Secure Boot policy)".to_string(),
        e => format!("WinDivert couldn't start (error {e})"),
    })
}

fn send(handle: win::HANDLE, packet: &[u8], addr: &Address) {
    if let Ok(dll) = dll() {
        // SAFETY: the packet slice and its length go together; addr came from WinDivertRecv.
        unsafe { (dll.send)(handle, packet.as_ptr().cast(), packet.len() as u32, std::ptr::null_mut(), addr) };
    }
}

impl Diverter {
    fn start(shared: Arc<Shared>, whole_pc: bool) -> Result<Self, String> {
        let handle = open(whole_pc)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (recv_shared, recv_stop) = (shared.clone(), stop.clone());
        std::thread::spawn(move || receive(handle, recv_shared, recv_stop));
        let send_stop = stop.clone();
        std::thread::spawn(move || release(handle, shared, send_stop));
        Ok(Self { handle, whole_pc, stop })
    }

    /// Stop capturing. Held packets still go out, then the handle closes.
    fn stop(self, shared: &Shared) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(dll) = dll() {
            // SAFETY: shutting down a handle we opened; the threads see the stop flag.
            unsafe { (dll.shutdown)(self.handle, 1) }; // WINDIVERT_SHUTDOWN_RECV
        }
        shared.wake.notify_all();
    }
}

fn receive(handle: win::HANDLE, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let Ok(dll) = dll() else { return };
    let mut buf = vec![0u8; MTU];
    loop {
        let mut len = 0u32;
        let mut addr = Address { timestamp: 0, flags: 0, reserved: 0, data: [0; 64] };
        // SAFETY: buf, len and addr are valid for writing for the call.
        let ok = unsafe { (dll.recv)(handle, buf.as_mut_ptr().cast(), buf.len() as u32, &mut len, &mut addr) };
        if ok == 0 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            continue;
        }
        let packet = &buf[..len as usize];
        let outbound = addr.outbound();
        let plan = shared.plan.read().unwrap().clone();
        match verdict(&plan, packet, outbound) {
            Verdict::Pass => send(handle, packet, &addr),
            Verdict::Drop => {}
            Verdict::Hold => {
                let due = match shared.schedule.lock().unwrap().as_mut() {
                    Some(schedule) => schedule.due(Instant::now(), !outbound),
                    None => Instant::now(),
                };
                let mut held = shared.held.lock().unwrap();
                if outbound {
                    held.out.push_back((due, packet.to_vec(), addr));
                    shared.counts.0.fetch_add(1, Ordering::Relaxed);
                } else {
                    held.inbound.push_back((due, packet.to_vec(), addr));
                    shared.counts.1.fetch_add(1, Ordering::Relaxed);
                }
                shared.wake.notify_all();
            }
        }
    }
}

/// Send held packets when their time comes, and everything once stopped.
fn release(handle: win::HANDLE, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let mut held = shared.held.lock().unwrap();
    loop {
        let stopping = stop.load(Ordering::Relaxed);
        let now = Instant::now();
        let lists = &mut *held;
        for list in [&mut lists.out, &mut lists.inbound] {
            while list.front().is_some_and(|(due, _, _)| stopping || *due <= now) {
                if let Some((_, packet, addr)) = list.pop_front() {
                    send(handle, &packet, &addr);
                }
            }
        }
        if stopping {
            drop(held);
            if let Ok(dll) = dll() {
                // SAFETY: both threads are done with the handle: receive returns on the stop flag.
                unsafe { (dll.close)(handle) };
            }
            return;
        }
        let wait = held.next_due().map_or(Duration::from_millis(200), |due| due.saturating_duration_since(now));
        held = shared.wake.wait_timeout(held, wait).unwrap().0;
    }
}

/// Fake lag on one app, until it's turned off.
struct Lag {
    shape: Shape,
    target: Target,
    names: Vec<String>,
    pids: Vec<u32>,
}

struct Engine {
    shared: Arc<Shared>,
    diverter: Option<Diverter>,
    active: Option<Active>,
    freeze_pids: Vec<u32>,
    lag: Option<Lag>,
    refreshed: Instant,
}

/// The processes an app target stands for: an exact exe name, or part of
/// exactly one, or a pid.
fn resolve(target: &Target) -> Result<(Vec<String>, Vec<u32>), String> {
    let all = win::processes();
    let pick = |want: &dyn Fn(&str) -> bool| -> Vec<(u32, String)> { all.iter().filter(|(_, n)| want(n)).cloned().collect() };
    let found = match target {
        Target::Network => return Err("this only works on one app, not the whole PC".to_string()),
        Target::Pid(pid) => all.iter().filter(|(p, _)| p == pid).cloned().collect(),
        Target::App(name) => {
            let exact = pick(&|n| n.eq_ignore_ascii_case(name) || n.eq_ignore_ascii_case(&format!("{name}.exe")));
            if exact.is_empty() {
                let lower = name.to_lowercase();
                let close = pick(&|n| n.to_lowercase().contains(&lower));
                let names: HashSet<String> = close.iter().map(|(_, n)| n.to_lowercase()).collect();
                if names.len() > 1 {
                    let mut list: Vec<String> = names.into_iter().collect();
                    list.sort();
                    return Err(format!("\"{name}\" matches several apps: {}", list.join(", ")));
                }
                close
            } else {
                exact
            }
        }
    };
    if found.is_empty() {
        return Err(match target {
            Target::Pid(pid) => format!("process {pid} not found"),
            _ => format!("no running app matches \"{}\"", target.encode().trim_start_matches("app:")),
        });
    }
    let mut names: Vec<String> = found.iter().map(|(_, n)| n.clone()).collect();
    names.sort();
    names.dedup();
    Ok((names, found.into_iter().map(|(p, _)| p).collect()))
}

impl Engine {
    fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                plan: RwLock::new(Plan::default()),
                schedule: Mutex::new(None),
                held: Mutex::new(Held::default()),
                wake: Condvar::new(),
                counts: (AtomicU64::new(0), AtomicU64::new(0)),
            }),
            diverter: None,
            active: None,
            freeze_pids: Vec::new(),
            lag: None,
            refreshed: Instant::now(),
        }
    }

    /// Point the packet thread at the current freeze and fake lag, opening or
    /// closing WinDivert as needed.
    fn apply(&mut self) -> Result<(), String> {
        let plan = Plan {
            whole_pc: self.active.as_ref().is_some_and(|a| a.network),
            freeze: self.active.as_ref().filter(|a| !a.network).map(|a| (Ports::of(&self.freeze_pids), a.direction)),
            lag: self.lag.as_ref().map(|l| (Ports::of(&l.pids), l.shape)),
        };
        let reopen = self.diverter.as_ref().is_none_or(|d| d.whole_pc != plan.whole_pc);
        if plan.capturing() && reopen {
            let next = Diverter::start(self.shared.clone(), plan.whole_pc)?;
            if let Some(old) = self.diverter.replace(next) {
                old.stop(&self.shared);
            }
        }
        let capturing = plan.capturing();
        *self.shared.plan.write().unwrap() = plan;
        if !capturing && let Some(old) = self.diverter.take() {
            old.stop(&self.shared);
        }
        self.refreshed = Instant::now();
        Ok(())
    }

    fn freeze(&mut self, f: &FreezeRequest, length: Duration) -> Result<(), String> {
        let whole_pc = f.target == Target::Network;
        let (names, pids) = if whole_pc { (Vec::new(), Vec::new()) } else { resolve(&f.target)? };
        let previous = (self.active.take(), std::mem::take(&mut self.freeze_pids));
        self.active = Some(Active {
            mode: f.mode,
            direction: if whole_pc { Direction::Both } else { f.direction },
            cgroups: names,
            network: whole_pc,
            deadline: Instant::now() + length,
        });
        self.freeze_pids = pids;
        if let Err(e) = self.apply() {
            (self.active, self.freeze_pids) = previous;
            return Err(e);
        }
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        self.active = None;
        self.freeze_pids.clear();
        self.apply()
    }

    fn lag_on(&mut self, shape: Shape, target: &Target) -> Result<(), String> {
        shape.check()?;
        let (names, pids) = resolve(target)?;
        let fresh = self.lag.is_none();
        let previous = self.lag.replace(Lag { shape, target: target.clone(), names, pids });
        *self.shared.schedule.lock().unwrap() = Some(Schedule::new(shape, seed()));
        if fresh {
            self.shared.counts.0.store(0, Ordering::Relaxed);
            self.shared.counts.1.store(0, Ordering::Relaxed);
        }
        if let Err(e) = self.apply() {
            self.lag = previous;
            return Err(e);
        }
        Ok(())
    }

    fn lag_off(&mut self) -> Result<(), String> {
        self.lag = None;
        self.apply()?;
        // Anything still held goes out now.
        let mut held = self.shared.held.lock().unwrap();
        let lists = &mut *held;
        for list in [&mut lists.out, &mut lists.inbound] {
            for entry in list.iter_mut() {
                entry.0 = Instant::now();
            }
        }
        drop(held);
        self.shared.wake.notify_all();
        Ok(())
    }

    /// The same status line as lagswitchd, so the Lag tab reads it the same way.
    fn report(&self) -> String {
        let freeze = match &self.active {
            None => "idle".to_string(),
            Some(a) => format!(
                "frozen {} {} {} {}",
                a.mode.name(),
                a.direction.name(),
                state::millis_until(a.deadline, Instant::now()),
                if a.network { "network".to_string() } else { a.cgroups.join(",") }
            ),
        };
        match &self.lag {
            Some(l) => format!(
                "{freeze} lag {} {} style={} jitter={} out={} in={}",
                l.shape.ms,
                l.names.join(","),
                l.shape.style.name(),
                l.shape.jitter,
                self.shared.counts.0.load(Ordering::Relaxed),
                self.shared.counts.1.load(Ordering::Relaxed)
            ),
            None => freeze,
        }
    }

    fn handle(&mut self, request: &Request) -> Result<String, String> {
        match request {
            Request::Lag { toggle: true, .. } if self.lag.is_some() => self.lag_off()?,
            Request::Lag { shape, target, .. } => self.lag_on(*shape, target)?,
            Request::LagOff => self.lag_off()?,
            _ => match state::decide(self.active.as_ref(), request) {
                Decision::Report => {}
                Decision::Release => self.release()?,
                Decision::Freeze(length) => match request {
                    Request::Freeze(f) => self.freeze(f, length)?,
                    _ => {}
                },
            },
        }
        Ok(self.report())
    }

    /// The timer, and following the apps' ports and restarts.
    fn tick(&mut self) {
        if self.active.as_ref().is_some_and(|a| Instant::now() >= a.deadline) {
            let _ = self.release();
        }
        if self.refreshed.elapsed() < REFRESH_EVERY || !self.shared.plan.read().unwrap().capturing() {
            return;
        }
        if let Some(a) = &self.active
            && !a.network
            && let Some(name) = a.cgroups.first()
            && let Ok((_, pids)) = resolve(&Target::App(name.clone()))
        {
            self.freeze_pids = pids;
        }
        if let Some(lag) = &mut self.lag
            && let Ok((names, pids)) = resolve(&lag.target)
        {
            lag.names = names;
            lag.pids = pids;
        }
        let _ = self.apply();
    }
}

fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
}

fn engine() -> &'static Mutex<Engine> {
    static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();
    ENGINE.get_or_init(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(TICK);
                engine().lock().unwrap().tick();
            }
        });
        Mutex::new(Engine::new())
    })
}

/// lagswitch's commands, answered by the engine in this process.
pub struct Local;

impl Backend for Local {
    fn ask(&self, request: &Request) -> Result<String, String> {
        engine().lock().unwrap().handle(request)
    }

    /// Programs with a window or a network socket, one row each. Ones with a
    /// window come first and show as apps.
    fn list(&self, filter: Option<&str>) -> Result<String, String> {
        let windowed: HashSet<String> = win::clients().into_iter().map(|c| c.class.to_lowercase()).collect();
        let networked = win::networked_pids();
        let mut by_name: Vec<(String, u32, usize)> = Vec::new();
        for (pid, name) in win::processes() {
            // The idle and System processes, and background ones with nothing to freeze.
            if pid == 0 || pid == 4 || !(networked.contains(&pid) || windowed.contains(&name.to_lowercase())) {
                continue;
            }
            match by_name.iter_mut().find(|(n, _, _)| n.eq_ignore_ascii_case(&name)) {
                Some(entry) => entry.2 += 1,
                None => by_name.push((name, pid, 1)),
            }
        }
        let filter = filter.map(str::to_lowercase);
        by_name.retain(|(n, _, _)| filter.as_ref().is_none_or(|f| n.to_lowercase().contains(f)));
        by_name.sort_by_key(|(n, _, _)| (!windowed.contains(&n.to_lowercase()), n.to_lowercase()));
        let rows: Vec<String> = by_name
            .into_iter()
            .map(|(name, pid, count)| {
                let app = if windowed.contains(&name.to_lowercase()) { name.as_str() } else { "-" };
                format!("{name}\t{pid}\t{app}\t{count}\tok\t{name}")
            })
            .collect();
        Ok(rows.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4_udp(src: u16, dst: u16) -> Vec<u8> {
        let mut p = vec![0x45, 0, 0, 28, 0, 0, 0, 0, 64, 17, 0, 0, 10, 0, 0, 1, 1, 1, 1, 1];
        p.extend(src.to_be_bytes());
        p.extend(dst.to_be_bytes());
        p.extend([0, 8, 0, 0]);
        p
    }

    #[test]
    fn finds_the_local_port() {
        let packet = ipv4_udp(50153, 53);
        assert_eq!(local_port(&packet, true), Some((17, 50153)));
        assert_eq!(local_port(&packet, false), Some((17, 53)));
        assert_eq!(local_port(&packet[..21], true), None);
        assert_eq!(std::mem::size_of::<Address>(), 80);
    }

    #[test]
    fn freezes_and_holds_only_the_app() {
        let app = Ports { tcp: HashSet::new(), udp: [50153].into_iter().collect() };
        let shape = Shape { ms: 200, style: lagswitch::shape::Style::Choke, jitter: 0 };
        let plan = Plan { whole_pc: false, freeze: None, lag: Some((app.clone(), shape)) };
        assert!(matches!(verdict(&plan, &ipv4_udp(50153, 53), true), Verdict::Hold));
        assert!(matches!(verdict(&plan, &ipv4_udp(53, 50153), false), Verdict::Pass), "choke leaves incoming alone");
        assert!(matches!(verdict(&plan, &ipv4_udp(40000, 53), true), Verdict::Pass), "other apps pass");
        let frozen = Plan { whole_pc: false, freeze: Some((app, Direction::Out)), lag: None };
        assert!(matches!(verdict(&frozen, &ipv4_udp(50153, 53), true), Verdict::Drop));
        assert!(matches!(verdict(&frozen, &ipv4_udp(53, 50153), false), Verdict::Pass));
        let all = Plan { whole_pc: true, ..Plan::default() };
        assert!(matches!(verdict(&all, &ipv4_udp(1, 2), false), Verdict::Drop));
    }
}
