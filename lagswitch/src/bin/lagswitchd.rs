//! The root daemon: owns the rules and the timer, so a freeze ends on time even
//! if every client dies. Run by the systemd unit in dist/.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::ExitCode;
use std::ptr;
use std::time::{Duration, Instant};

use lagswitch::proto::{FreezeRequest, Request, SOCKET, Target};
use lagswitch::state::{self, Active, Decision};
use lagswitch::delay::{self, Delay};
use lagswitch::rules::{Direction, Freeze};
use lagswitch::{cgroup, network, nft};

/// Removes the rules however the daemon ends, including by panic.
struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Err(e) = network::release_all() {
            eprintln!("lagswitchd: cleanup failed: {e}");
        }
    }
}

fn signal_fd() -> Result<OwnedFd, String> {
    // SAFETY: the set is initialised by sigemptyset before any other use, and
    // signalfd returns a new descriptor that OwnedFd takes over.
    unsafe {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        libc::sigemptyset(set.as_mut_ptr());
        let mut set = set.assume_init();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut set, signal);
        }
        if libc::sigprocmask(libc::SIG_BLOCK, &set, ptr::null_mut()) != 0 {
            return Err("can't block signals".to_string());
        }
        let fd = libc::signalfd(-1, &set, libc::SFD_CLOEXEC);
        if fd < 0 {
            return Err("can't create signalfd".to_string());
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

fn listen(uid: u32) -> Result<UnixListener, String> {
    let _ = fs::remove_file(SOCKET);
    // Never readable by anyone else, not even between bind and chmod.
    // SAFETY: umask has no preconditions.
    let old = unsafe { libc::umask(0o177) };
    let listener = UnixListener::bind(SOCKET).map_err(|e| format!("can't bind {SOCKET}: {e}"));
    // SAFETY: as above.
    unsafe { libc::umask(old) };
    let listener = listener?;
    std::os::unix::fs::chown(SOCKET, Some(uid), None).map_err(|e| format!("can't chown {SOCKET}: {e}"))?;
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("can't chmod {SOCKET}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| format!("can't set up {SOCKET}: {e}"))?;
    Ok(listener)
}

fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid for writing and describe the same buffer.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

/// Fake lag on one app, which lasts until it's turned off.
struct Lag {
    ms: u64,
    cgroups: Vec<String>,
    queue: Delay,
}

struct Daemon {
    uid: u32,
    active: Option<Active>,
    lag: Option<Lag>,
}

impl Daemon {
    fn resolve(&self, target: &Target) -> Result<Vec<String>, String> {
        let cgroups = match target {
            Target::Network => return Err("this only works on one app, not the whole PC".to_string()),
            Target::App(name) => cgroup::resolve_app(name, self.uid)?,
            Target::Pid(pid) => vec![cgroup::of_pid(*pid)?],
        };
        for path in &cgroups {
            cgroup::check_owner(path, self.uid)?;
            cgroup::check_target(path)?;
        }
        Ok(cgroups)
    }

    /// Load the table for this freeze plus the current fake lag.
    fn apply(&self, active: Option<&Active>) -> Result<(), String> {
        let freeze = match active {
            None => Freeze::Off,
            Some(a) if a.network => Freeze::WholePc,
            Some(a) => Freeze::Apps(&a.cgroups, a.direction),
        };
        nft::apply(freeze, self.lag.as_ref().map_or(&[], |l| l.cgroups.as_slice()))
    }

    fn freeze(&mut self, f: &FreezeRequest, length: Duration) -> Result<(), String> {
        let network_off = self.active.as_ref().is_some_and(|a| a.network);
        let whole_pc = f.target == Target::Network;
        let (direction, cgroups) =
            if whole_pc { (Direction::Both, Vec::new()) } else { (f.direction, self.resolve(&f.target)?) };
        let next = Active { mode: f.mode, direction, cgroups, network: whole_pc, deadline: Instant::now() + length };
        self.apply(Some(&next))?;
        let routing = match (whole_pc, network_off) {
            (true, false) => network::off(),
            (false, true) => network::restore(),
            _ => Ok(()),
        };
        if let Err(e) = routing {
            let _ = network::restore();
            let _ = self.apply(None);
            self.active = None;
            return Err(e);
        }
        self.active = Some(next);
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        let network = network::restore();
        if self.apply(None).is_err() {
            // Ending the freeze matters more than keeping fake lag on.
            self.lag = None;
            nft::release()?;
        }
        network?;
        self.active = None;
        Ok(())
    }

    fn lag_on(&mut self, ms: u64, target: &Target) -> Result<(), String> {
        if ms == 0 || ms > delay::MAX_MS {
            return Err(format!("fake lag must be 1 to {} ms", delay::MAX_MS));
        }
        let cgroups = self.resolve(target)?;
        let previous = self.lag.as_ref().map(|l| (l.ms, l.cgroups.clone()));
        match &mut self.lag {
            Some(lag) => {
                lag.queue.set_ms(ms);
                lag.ms = ms;
                lag.cgroups = cgroups;
            }
            None => self.lag = Some(Lag { ms, cgroups, queue: Delay::open(ms)? }),
        }
        if let Err(e) = self.apply(self.active.as_ref()) {
            match (previous, &mut self.lag) {
                (Some((ms, cgroups)), Some(lag)) => {
                    lag.queue.set_ms(ms);
                    lag.ms = ms;
                    lag.cgroups = cgroups;
                }
                _ => self.lag = None,
            }
            return Err(e);
        }
        Ok(())
    }

    fn lag_off(&mut self) -> Result<(), String> {
        let Some(lag) = self.lag.take() else { return Ok(()) };
        if let Err(e) = self.apply(self.active.as_ref()) {
            self.lag = Some(lag);
            return Err(e);
        }
        // Dropping the queue lets through whatever was still waiting.
        drop(lag);
        Ok(())
    }

    fn report(&self) -> String {
        let freeze = match &self.active {
            None => "ok idle".to_string(),
            Some(a) => format!(
                "ok frozen {} {} {} {}",
                a.mode.name(),
                a.direction.name(),
                state::millis_until(a.deadline, Instant::now()),
                if a.network { "network".to_string() } else { a.cgroups.join(",") }
            ),
        };
        match &self.lag {
            Some(lag) => format!("{freeze} lag {} {}", lag.ms, lag.cgroups.join(",")),
            None => freeze,
        }
    }

    fn handle(&mut self, line: &str) -> String {
        let result = Request::parse(line).and_then(|request| {
            match &request {
                Request::Lag { toggle: true, .. } if self.lag.is_some() => return self.lag_off(),
                Request::Lag { ms, target, .. } => return self.lag_on(*ms, target),
                Request::LagOff => return self.lag_off(),
                _ => {}
            }
            match state::decide(self.active.as_ref(), &request) {
                Decision::Report => Ok(()),
                Decision::Release => self.release(),
                Decision::Freeze(length) => match &request {
                    Request::Freeze(f) => self.freeze(f, length),
                    _ => Ok(()),
                },
            }
        });
        match result {
            Ok(()) => self.report(),
            Err(e) => format!("err {e}"),
        }
    }

    fn serve(&mut self, stream: UnixStream) -> std::io::Result<()> {
        if !peer_uid(&stream).is_some_and(|uid| uid == self.uid || uid == 0) {
            return Ok(());
        }
        // A stuck client must never hold up the timer for long.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        stream.set_write_timeout(Some(Duration::from_millis(200)))?;
        let mut line = String::new();
        BufReader::new(Read::take(&stream, 4096)).read_line(&mut line)?;
        let reply = self.handle(&line);
        (&stream).write_all(format!("{reply}\n").as_bytes())
    }

    /// Runs until a signal arrives.
    fn run(&mut self, listener: &UnixListener, signals: &OwnedFd) -> Result<(), String> {
        loop {
            let now = Instant::now();
            let next = [self.active.as_ref().map(|a| a.deadline), self.lag.as_ref().and_then(|l| l.queue.next_due())]
                .into_iter()
                .flatten()
                .min();
            let timeout = next.map_or(-1, |due| state::millis_until(due, now).min(i32::MAX as u64) as i32);
            let mut fds = vec![
                libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: signals.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            if let Some(lag) = &self.lag {
                fds.push(libc::pollfd { fd: lag.queue.fd(), events: libc::POLLIN, revents: 0 });
            }
            // SAFETY: fds is valid for the call and its length is passed with it.
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("poll failed: {e}"));
            }
            if fds[1].revents != 0 {
                return Ok(());
            }
            if let Some(lag) = &mut self.lag {
                if fds.get(2).is_some_and(|fd| fd.revents != 0)
                    && let Err(e) = lag.queue.read()
                {
                    eprintln!("lagswitchd: {e}");
                }
                if let Err(e) = lag.queue.pass_due(Instant::now()) {
                    eprintln!("lagswitchd: {e}");
                }
            }
            if fds[0].revents != 0
                && let Ok((stream, _)) = listener.accept()
                && let Err(e) = self.serve(stream)
            {
                eprintln!("lagswitchd: client error: {e}");
            }
            if self.active.as_ref().is_some_and(|a| Instant::now() >= a.deadline)
                && let Err(e) = self.release()
            {
                // Keep trying: the failsafe must not give up.
                eprintln!("lagswitchd: release failed, retrying: {e}");
                if let Some(a) = &mut self.active {
                    a.deadline = Instant::now() + Duration::from_secs(1);
                }
            }
        }
    }
}

/// A user name's uid, from the system's user database.
fn uid_of(name: &str) -> Result<u32, String> {
    let c_name = std::ffi::CString::new(name).map_err(|_| format!("bad user name \"{name}\""))?;
    // SAFETY: an all-zero passwd is valid storage for getpwnam_r to fill.
    let mut entry: libc::passwd = unsafe { mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut found: *mut libc::passwd = ptr::null_mut();
    // SAFETY: every pointer is valid for the call, and buf's length is passed with it.
    let rc = unsafe { libc::getpwnam_r(c_name.as_ptr(), &mut entry, buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() {
        return Err(format!("no user named \"{name}\""));
    }
    Ok(entry.pw_uid)
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut uid = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--release" => return network::release_all(),
            "--uid" => {
                let value = args.next().ok_or("--uid needs a value")?;
                uid = Some(value.parse().map_err(|_| format!("bad uid \"{value}\""))?);
            }
            "--user" => uid = Some(uid_of(&args.next().ok_or("--user needs a name")?)?),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let uid = uid.ok_or("--uid or --user is required")?;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("not root".to_string());
    }
    // Whatever a crash or power cut left behind.
    network::release_all()?;
    let cleanup = Cleanup;
    let signals = signal_fd()?;
    let listener = listen(uid)?;
    let result = Daemon { uid, active: None, lag: None }.run(&listener, &signals);
    let _ = fs::remove_file(SOCKET);
    drop(cleanup);
    result
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lagswitchd: {e}");
            ExitCode::FAILURE
        }
    }
}
