use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use lagswitch::cgroup;
use lagswitch::cli::{self, Backend};
use lagswitch::proto::{Request, SOCKET};

/// The root daemon, over its socket.
struct Daemon;

impl Backend for Daemon {
    fn ask(&self, request: &Request) -> Result<String, String> {
        let mut stream = UnixStream::connect(SOCKET).map_err(|e| match e.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
                "the daemon isn't running, start it with: sudo systemctl start lagswitchd".to_string()
            }
            ErrorKind::PermissionDenied => format!("no permission to use {SOCKET}"),
            _ => format!("can't reach the daemon: {e}"),
        })?;
        let timeout = Some(Duration::from_secs(3));
        let io_error = |e: std::io::Error| format!("can't talk to the daemon: {e}");
        stream.set_read_timeout(timeout).map_err(io_error)?;
        stream.set_write_timeout(timeout).map_err(io_error)?;
        writeln!(stream, "{}", request.encode()).map_err(io_error)?;
        let mut reply = String::new();
        BufReader::new(&stream).read_line(&mut reply).map_err(io_error)?;
        match reply.trim_end().strip_prefix("ok") {
            Some(text) => Ok(text.trim_start().to_string()),
            None => Err(reply.trim_end().trim_start_matches("err ").to_string()),
        }
    }

    /// One row per cgroup.
    fn list(&self, filter: Option<&str>) -> Result<String, String> {
        // SAFETY: geteuid has no preconditions.
        let mut entries = cgroup::entries(unsafe { libc::geteuid() })?;
        if let Some(text) = filter {
            entries = cgroup::filter(entries, text);
        }
        let rows: Vec<String> = entries
            .into_iter()
            .map(|e| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    e.name,
                    e.pid,
                    e.app_id.as_deref().unwrap_or("-"),
                    e.processes,
                    if e.freezable { "ok" } else { "shared" },
                    e.cgroup
                )
            })
            .collect();
        Ok(rows.join("\n"))
    }
}

/// Run from a keybind, nobody sees stderr, so binds pass --notify to show
/// errors (and what a key turned on or off) on the desktop too.
fn notify(text: &str) {
    let _ = std::process::Command::new("notify-send")
        // Short-lived, and replacing the last one where the daemon supports it.
        .args(["-a", "lagswitch", "-t", "2500", "-h", "string:x-canonical-private-synchronous:lagswitch", "lagswitch", text])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let notify_errors = args.first().map(String::as_str) == Some("--notify");
    if notify_errors {
        args.remove(0);
    }
    match cli::run(args, &Daemon) {
        Ok(outcome) => {
            if !outcome.output.is_empty() {
                println!("{}", outcome.output);
            }
            if let Some(notice) = outcome.notice.filter(|_| notify_errors) {
                notify(&notice);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lagswitch: {e}");
            if notify_errors {
                notify(&e);
            }
            ExitCode::FAILURE
        }
    }
}
