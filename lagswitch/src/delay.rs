//! Fake lag: nft hands the app's packets to a netfilter queue, and this lets
//! each one through later. Two styles:
//!
//! - Steady: half the delay on the way out and half on the way in, so the round
//!   trip a game measures grows by the full amount.
//! - Choke: outgoing packets are held for up to the limit and then sent in one
//!   burst, again and again, so others see you freeze and jump. Incoming
//!   packets aren't touched, so your own game stays smooth.
//!
//! Jitter shortens each hold, or the steady delay for a quarter second at a
//! time, by a random part of up to that percentage.
//!
//! The rules use `queue flags bypass`, so with nobody listening (the daemon
//! died) packets pass straight through and never get stuck.
//!
//! Message layouts come from linux/netfilter/nfnetlink.h and nfnetlink_queue.h.

use std::collections::VecDeque;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use crate::rules::QUEUE;

/// The longest delay the daemon accepts.
pub const MAX_MS: u64 = 5000;

const SUBSYS_QUEUE: u16 = 3;
const MSG_PACKET: u16 = 0;
const MSG_VERDICT: u16 = 1;
const MSG_CONFIG: u16 = 2;
const PACKET_HDR: u16 = 1;
const VERDICT_HDR: u16 = 2;
const CFG_CMD: u16 = 1;
const CFG_PARAMS: u16 = 2;
const CFG_QUEUE_MAXLEN: u16 = 3;
const CFG_MASK: u16 = 4;
const CFG_FLAGS: u16 = 5;
const CMD_BIND: u8 = 1;
const COPY_META: u8 = 1;
/// Over the queue limit, packets pass instead of being dropped.
const FLAG_FAIL_OPEN: u32 = 1;
const NF_ACCEPT: u32 = 1;
const HOOK_LOCAL_IN: u8 = 1;
const NLMSG_ERROR: u16 = 2;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
/// Attribute types carry two flag bits on top.
const NLA_TYPE_MASK: u16 = 0x3fff;
const HEADER: usize = 20;
const MAX_QUEUED: u32 = 8192;

/// One netlink message for the queue subsystem, with its attributes.
fn message(kind: u16, flags: u16, seq: u32, attrs: &[(u16, &[u8])]) -> Vec<u8> {
    let mut m = Vec::with_capacity(64);
    m.extend_from_slice(&0u32.to_ne_bytes()); // length, filled in below
    m.extend_from_slice(&((SUBSYS_QUEUE << 8) | kind).to_ne_bytes());
    m.extend_from_slice(&(NLM_F_REQUEST | flags).to_ne_bytes());
    m.extend_from_slice(&seq.to_ne_bytes());
    m.extend_from_slice(&0u32.to_ne_bytes());
    // nfgenmsg: family, version, then the queue number in network order.
    m.extend_from_slice(&[libc::AF_UNSPEC as u8, 0]);
    m.extend_from_slice(&QUEUE.to_be_bytes());
    for (kind, data) in attrs {
        m.extend_from_slice(&((4 + data.len()) as u16).to_ne_bytes());
        m.extend_from_slice(&kind.to_ne_bytes());
        m.extend_from_slice(data);
        while m.len() % 4 != 0 {
            m.push(0);
        }
    }
    let len = m.len() as u32;
    m[..4].copy_from_slice(&len.to_ne_bytes());
    m
}

fn verdict(id: u32) -> Vec<u8> {
    let mut hdr = [0u8; 8];
    hdr[..4].copy_from_slice(&NF_ACCEPT.to_be_bytes());
    hdr[4..].copy_from_slice(&id.to_be_bytes());
    message(MSG_VERDICT, 0, 0, &[(VERDICT_HDR, &hdr)])
}

#[derive(Debug, PartialEq)]
enum Event {
    Packet { id: u32, inbound: bool },
    /// The kernel's answer to a request: 0, or a negative errno.
    Ack { seq: u32, error: i32 },
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn packet(msg: &[u8]) -> Option<Event> {
    let mut at = HEADER;
    while at + 4 <= msg.len() {
        let len = usize::from(u16_at(msg, at)?);
        if len < 4 || at + len > msg.len() {
            return None;
        }
        if u16_at(msg, at + 2)? & NLA_TYPE_MASK == PACKET_HDR {
            let data = msg.get(at + 4..at + len)?;
            let id = u32::from_be_bytes(data.get(..4)?.try_into().ok()?);
            return Some(Event::Packet { id, inbound: *data.get(6)? == HOOK_LOCAL_IN });
        }
        at += (len + 3) & !3;
    }
    None
}

/// Everything in one buffer from recv.
fn parse(buf: &[u8]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut at = 0;
    while let (Some(len), Some(kind)) = (u32_at(buf, at), u16_at(buf, at + 4)) {
        let len = len as usize;
        let Some(msg) = buf.get(at..at + len).filter(|_| len >= 16) else { break };
        if kind == NLMSG_ERROR {
            if let (Some(seq), Some(error)) = (u32_at(msg, 8), u32_at(msg, 16)) {
                events.push(Event::Ack { seq, error: error as i32 });
            }
        } else if kind == (SUBSYS_QUEUE << 8) | MSG_PACKET
            && let Some(event) = packet(msg)
        {
            events.push(event);
        }
        at += (len + 3) & !3;
    }
    events
}

fn os_error(what: &str) -> String {
    format!("{what}: {}", io::Error::last_os_error())
}

fn set_int(fd: RawFd, level: libc::c_int, name: libc::c_int, value: libc::c_int) -> bool {
    // SAFETY: value lives for the call and its size is passed with it.
    unsafe {
        libc::setsockopt(fd, level, name, (&raw const value).cast(), mem::size_of::<libc::c_int>() as libc::socklen_t)
            == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Steady,
    Choke,
}

impl Style {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "steady" => Ok(Self::Steady),
            "choke" => Ok(Self::Choke),
            _ => Err(format!("style must be steady or choke, not \"{text}\"")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Choke => "choke",
        }
    }
}

/// How fake lag treats packets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// Steady: added ping. Choke: the longest hold.
    pub ms: u64,
    pub style: Style,
    /// Percent, 0 to 100.
    pub jitter: u8,
}

impl Shape {
    pub fn check(&self) -> Result<(), String> {
        if self.ms == 0 || self.ms > MAX_MS {
            return Err(format!("fake lag must be 1 to {MAX_MS} ms"));
        }
        if self.jitter > 100 {
            return Err("jitter must be 0 to 100 percent".to_string());
        }
        Ok(())
    }

    /// Choke leaves incoming packets alone, so they needn't be queued.
    pub fn delays_inbound(&self) -> bool {
        self.style == Style::Steady
    }
}

/// When each packet may go. No I/O, so it can be tested.
struct Schedule {
    shape: Shape,
    rng: u64,
    /// The latest time given out per direction, so packets never overtake.
    last_out: Option<Instant>,
    last_in: Option<Instant>,
    /// Choke: when the current hold ends.
    choke_until: Option<Instant>,
    /// Steady: the share of the delay in use, and when to pick a new one.
    steady_share: f64,
    next_pick: Option<Instant>,
}

/// Steady jitter picks a new delay this often. Per packet it would barely show,
/// because a packet can't overtake the ones before it.
const PICK_EVERY: Duration = Duration::from_millis(250);

impl Schedule {
    fn new(shape: Shape, seed: u64) -> Self {
        Self {
            shape,
            rng: seed | 1,
            last_out: None,
            last_in: None,
            choke_until: None,
            steady_share: 1.0,
            next_pick: None,
        }
    }

    /// A random number in [0, 1), from xorshift64.
    fn random(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 1, less a random part of up to the jitter percentage.
    fn share(&mut self) -> f64 {
        1.0 - f64::from(self.shape.jitter) / 100.0 * self.random()
    }

    fn millis(ms: u64, share: f64) -> Duration {
        Duration::from_secs_f64(ms as f64 * share / 1000.0)
    }

    fn due(&mut self, now: Instant, inbound: bool) -> Instant {
        let ms = self.shape.ms;
        let due = match self.shape.style {
            Style::Choke if inbound => now,
            Style::Choke => match self.choke_until {
                Some(until) if until > now => until,
                _ => {
                    let until = now + Self::millis(ms, self.share());
                    self.choke_until = Some(until);
                    until
                }
            },
            Style::Steady => {
                if self.next_pick.is_none_or(|at| now >= at) {
                    self.steady_share = self.share();
                    self.next_pick = Some(now + PICK_EVERY);
                }
                now + Self::millis(if inbound { ms - ms / 2 } else { ms / 2 }, self.steady_share)
            }
        };
        let last = if inbound { &mut self.last_in } else { &mut self.last_out };
        let due = last.map_or(due, |l| due.max(l));
        *last = Some(due);
        due
    }
}

pub struct Delay {
    fd: OwnedFd,
    schedule: Schedule,
    /// Packets in arrival order with the time each may go. Each list stays
    /// sorted, because the schedule never lets a packet overtake another.
    out: VecDeque<(Instant, u32)>,
    inbound: VecDeque<(Instant, u32)>,
    /// Packets delayed so far, out and in.
    pub counts: (u64, u64),
    seq: u32,
}

impl Delay {
    /// Take over the queue. Nothing is queued until the nft rules point at it.
    pub fn open(shape: Shape) -> Result<Self, String> {
        // SAFETY: plain socket call; the descriptor is checked before OwnedFd takes it.
        let raw = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_NETFILTER) };
        if raw < 0 {
            return Err(os_error("can't open a netfilter socket"));
        }
        // SAFETY: raw is a new descriptor nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: an all-zero sockaddr_nl is valid, and family is set before use.
        let mut kernel: libc::sockaddr_nl = unsafe { mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let size = mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        // SAFETY: kernel is a valid sockaddr_nl of the given size. Port 0 is the kernel.
        unsafe {
            if libc::bind(raw, (&raw const kernel).cast(), size) != 0 {
                return Err(os_error("can't bind the netfilter socket"));
            }
            if libc::connect(raw, (&raw const kernel).cast(), size) != 0 {
                return Err(os_error("can't connect the netfilter socket"));
            }
        }
        // Room for bursts, and no error if it ever overflows anyway.
        set_int(raw, libc::SOL_SOCKET, libc::SO_RCVBUFFORCE, 8 << 20);
        set_int(raw, libc::SOL_NETLINK, libc::NETLINK_NO_ENOBUFS, 1);
        let timeout = libc::timeval { tv_sec: 1, tv_usec: 0 };
        // SAFETY: timeout lives for the call and its size is passed with it.
        unsafe {
            libc::setsockopt(raw, libc::SOL_SOCKET, libc::SO_RCVTIMEO, (&raw const timeout).cast(),
                mem::size_of::<libc::timeval>() as libc::socklen_t);
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64);
        let mut delay = Self {
            fd,
            schedule: Schedule::new(shape, seed),
            out: VecDeque::new(),
            inbound: VecDeque::new(),
            counts: (0, 0),
            seq: 0,
        };
        delay.configure(&[(CFG_CMD, &[CMD_BIND, 0, 0, 0])])?;
        let mut params = [0u8; 5];
        params[4] = COPY_META; // only the packet ID, never its contents
        delay.configure(&[(CFG_PARAMS, &params)])?;
        delay.configure(&[
            (CFG_QUEUE_MAXLEN, &MAX_QUEUED.to_be_bytes()),
            (CFG_MASK, &FLAG_FAIL_OPEN.to_be_bytes()),
            (CFG_FLAGS, &FLAG_FAIL_OPEN.to_be_bytes()),
        ])?;
        // SAFETY: fcntl on a descriptor we own.
        if unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
            return Err(os_error("can't set up the netfilter socket"));
        }
        Ok(delay)
    }

    fn send(&self, msg: &[u8]) -> io::Result<()> {
        // SAFETY: msg is valid for reads of its length.
        let sent = unsafe { libc::send(self.fd.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0) };
        if sent < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Send a config message and wait for the kernel to accept it.
    fn configure(&mut self, attrs: &[(u16, &[u8])]) -> Result<(), String> {
        self.seq += 1;
        let seq = self.seq;
        self.send(&message(MSG_CONFIG, NLM_F_ACK, seq, attrs))
            .map_err(|e| format!("can't set up the packet queue: {e}"))?;
        let mut buf = vec![0u8; 8192];
        loop {
            // SAFETY: buf is valid for writes of its length.
            let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                return Err(os_error("no answer while setting up the packet queue"));
            }
            for event in parse(&buf[..n as usize]) {
                match event {
                    Event::Ack { seq: s, error: 0 } if s == seq => return Ok(()),
                    Event::Ack { seq: s, error } if s == seq => {
                        let e = io::Error::from_raw_os_error(-error);
                        return Err(match -error {
                            libc::EBUSY | libc::EPERM => format!("packet queue {QUEUE} is already in use: {e}"),
                            _ => format!("can't set up the packet queue: {e}"),
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    /// Change how packets are delayed. Packets already waiting keep their time.
    pub fn set_shape(&mut self, shape: Shape) {
        self.schedule.shape = shape;
        self.schedule.choke_until = None;
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Take in every packet the kernel has handed over so far.
    pub fn read(&mut self) -> Result<(), String> {
        let mut buf = vec![0u8; 65536];
        loop {
            // SAFETY: buf is valid for writes of its length.
            let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                return match e.kind() {
                    io::ErrorKind::WouldBlock => Ok(()),
                    io::ErrorKind::Interrupted => continue,
                    _ => Err(format!("packet queue: {e}")),
                };
            }
            let now = Instant::now();
            for event in parse(&buf[..n as usize]) {
                if let Event::Packet { id, inbound } = event {
                    let due = self.schedule.due(now, inbound);
                    if inbound {
                        self.inbound.push_back((due, id));
                        self.counts.1 += 1;
                    } else {
                        self.out.push_back((due, id));
                        self.counts.0 += 1;
                    }
                }
            }
        }
    }

    pub fn next_due(&self) -> Option<Instant> {
        [self.out.front(), self.inbound.front()].into_iter().flatten().map(|(due, _)| *due).min()
    }

    /// Let through every packet whose time has come, or all of them.
    fn pass(&mut self, until: Option<Instant>) -> Result<(), String> {
        let mut first_error = None;
        for list in [&mut self.out, &mut self.inbound] {
            while let Some(&(due, id)) = list.front() {
                if until.is_some_and(|now| due > now) {
                    break;
                }
                list.pop_front();
                let msg = verdict(id);
                // SAFETY: msg is valid for reads of its length.
                let sent = unsafe { libc::send(self.fd.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0) };
                if sent < 0 && first_error.is_none() {
                    first_error = Some(os_error("can't pass a packet on"));
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn pass_due(&mut self, now: Instant) -> Result<(), String> {
        self.pass(Some(now))
    }
}

impl Drop for Delay {
    /// Closing the socket drops whatever the kernel still holds for us, including
    /// packets handed over but not read yet, so read those and pass everything first.
    fn drop(&mut self) {
        let _ = self.read();
        let _ = self.pass(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel_packet(id: u32, hook: u8) -> Vec<u8> {
        let mut hdr = [0u8; 7];
        hdr[..4].copy_from_slice(&id.to_be_bytes());
        hdr[4..6].copy_from_slice(&0x0800u16.to_be_bytes());
        hdr[6] = hook;
        // A mark attribute first, so the parser has to walk past it.
        message(MSG_PACKET, 0, 0, &[(3, &7u32.to_be_bytes()), (PACKET_HDR, &hdr)])
    }

    #[test]
    fn messages_are_aligned_and_sized() {
        let m = message(MSG_CONFIG, NLM_F_ACK, 9, &[(CFG_PARAMS, &[0, 0, 0, 0, COPY_META])]);
        assert_eq!(m.len() % 4, 0);
        assert_eq!(u32_at(&m, 0), Some(m.len() as u32));
        assert_eq!(u16_at(&m, 4), Some((3 << 8) | 2));
        assert_eq!(u16_at(&m, 6), Some(NLM_F_REQUEST | NLM_F_ACK));
        assert_eq!(u32_at(&m, 8), Some(9));
        assert_eq!(&m[18..20], &QUEUE.to_be_bytes());
        assert_eq!(u16_at(&m, 20), Some(9), "attribute length counts its header but not padding");
    }

    #[test]
    fn verdict_accepts_by_id() {
        let m = verdict(0x0102_0304);
        assert_eq!(u16_at(&m, 4), Some((3 << 8) | 1));
        assert_eq!(u16_at(&m, 22), Some(VERDICT_HDR));
        assert_eq!(&m[24..28], &1u32.to_be_bytes());
        assert_eq!(&m[28..32], &[1, 2, 3, 4]);
    }

    #[test]
    fn reads_packets_in_both_directions() {
        let mut buf = kernel_packet(5, 3);
        buf.extend(kernel_packet(6, HOOK_LOCAL_IN));
        assert_eq!(parse(&buf), [Event::Packet { id: 5, inbound: false }, Event::Packet { id: 6, inbound: true }]);
    }

    #[test]
    fn reads_acks_and_errors() {
        let mut m = vec![0u8; 36];
        m[..4].copy_from_slice(&36u32.to_ne_bytes());
        m[4..6].copy_from_slice(&NLMSG_ERROR.to_ne_bytes());
        m[8..12].copy_from_slice(&4u32.to_ne_bytes());
        m[16..20].copy_from_slice(&(-libc::EBUSY).to_ne_bytes());
        assert_eq!(parse(&m), [Event::Ack { seq: 4, error: -libc::EBUSY }]);
    }

    fn shape(ms: u64, style: Style, jitter: u8) -> Shape {
        Shape { ms, style, jitter }
    }

    #[test]
    fn steady_splits_the_delay_both_ways() {
        let now = Instant::now();
        let mut s = Schedule::new(shape(301, Style::Steady, 0), 1);
        assert_eq!(s.due(now, false) - now, Duration::from_millis(150));
        assert_eq!(s.due(now, true) - now, Duration::from_millis(151));
    }

    #[test]
    fn choke_holds_outgoing_then_sends_them_together() {
        let now = Instant::now();
        let mut s = Schedule::new(shape(400, Style::Choke, 0), 1);
        let first = s.due(now, false);
        assert_eq!(first - now, Duration::from_millis(400));
        // Everything sent during the hold goes out with the first packet.
        assert_eq!(s.due(now + Duration::from_millis(250), false), first);
        // After the burst, the next packet starts a new hold.
        let later = now + Duration::from_millis(410);
        assert_eq!(s.due(later, false) - later, Duration::from_millis(400));
        // Incoming packets pass straight away.
        assert_eq!(s.due(later, true), later);
    }

    #[test]
    fn jitter_only_shortens_and_never_reorders() {
        let now = Instant::now();
        let mut s = Schedule::new(shape(400, Style::Steady, 100), 7);
        let mut last = now;
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..200 {
            let at = now + Duration::from_millis(i * 20);
            let due = s.due(at, false);
            assert!(due >= last, "packets kept in order");
            assert!(due - at <= Duration::from_millis(200));
            seen.insert((due - at).as_millis() / 20);
            last = due;
        }
        assert!(seen.len() > 3, "delays vary: {seen:?}");
        assert!(shape(400, Style::Choke, 101).check().is_err());
        assert!(shape(0, Style::Choke, 0).check().is_err());
        assert!(!shape(400, Style::Choke, 0).delays_inbound());
    }

    #[test]
    fn ignores_truncated_input() {
        let buf = kernel_packet(5, 3);
        for cut in 0..buf.len() {
            let _ = parse(&buf[..cut]);
        }
        assert!(parse(&buf[..buf.len() - 4]).is_empty());
    }
}
