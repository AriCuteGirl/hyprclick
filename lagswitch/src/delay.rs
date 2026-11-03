//! Fake lag: nft hands the app's packets to a netfilter queue, and this lets
//! each one through after a fixed delay. Half the delay goes on the way out and
//! half on the way in, so the round trip a game measures grows by the full amount.
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

pub struct Delay {
    fd: OwnedFd,
    to_out: Duration,
    to_in: Duration,
    /// Packets in arrival order with the time each may go. A direction always
    /// adds the same delay, so each list stays sorted.
    out: VecDeque<(Instant, u32)>,
    inbound: VecDeque<(Instant, u32)>,
    seq: u32,
}

impl Delay {
    /// Take over the queue. Nothing is queued until the nft rules point at it.
    pub fn open(ms: u64) -> Result<Self, String> {
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
        let mut delay = Self {
            fd,
            to_out: Duration::ZERO,
            to_in: Duration::ZERO,
            out: VecDeque::new(),
            inbound: VecDeque::new(),
            seq: 0,
        };
        delay.set_ms(ms);
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

    /// Change the delay. Packets already waiting keep the time they were given.
    pub fn set_ms(&mut self, ms: u64) {
        let ms = ms.min(MAX_MS);
        self.to_out = Duration::from_millis(ms / 2);
        self.to_in = Duration::from_millis(ms - ms / 2);
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
                    if inbound {
                        self.inbound.push_back((now + self.to_in, id));
                    } else {
                        self.out.push_back((now + self.to_out, id));
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

    #[test]
    fn ignores_truncated_input() {
        let buf = kernel_packet(5, 3);
        for cut in 0..buf.len() {
            let _ = parse(&buf[..cut]);
        }
        assert!(parse(&buf[..buf.len() - 4]).is_empty());
    }
}
