use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

pub struct Peer {
    pub credentials: libc::ucred,
    pub process: Arc<OwnedFd>,
}

impl Peer {
    pub fn read(stream: &UnixStream) -> Result<Self, String> {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::from_mut(&mut credentials).cast(),
                &mut length,
            )
        } != 0
            || length as usize != std::mem::size_of_val(&credentials)
            || credentials.pid <= 0
        {
            return Err("cannot establish the request's Unix peer identity".into());
        }
        let mut descriptor: libc::c_int = -1;
        length = std::mem::size_of_val(&descriptor) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERPIDFD,
                std::ptr::from_mut(&mut descriptor).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(format!(
                "kernel peer pidfd is required: {}",
                io::Error::last_os_error()
            ));
        }
        if descriptor < 0 {
            return Err("kernel returned an invalid peer pidfd".into());
        }
        let process = Arc::new(unsafe { OwnedFd::from_raw_fd(descriptor) });
        if length as usize != std::mem::size_of_val(&descriptor) {
            return Err("kernel returned a malformed peer pidfd".into());
        }
        ensure_alive(&process)?;
        Ok(Self {
            credentials,
            process,
        })
    }
}

pub fn ensure_alive(process: &OwnedFd) -> Result<(), String> {
    let mut event = libc::pollfd {
        fd: process.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        match unsafe { libc::poll(&mut event, 1, 0) } {
            0 => return Ok(()),
            value if value > 0 => {
                return Err(if event.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                    "requesting process has exited".into()
                } else {
                    "requesting process identity is unavailable".into()
                });
            }
            _ => {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(error.to_string());
                }
            }
        }
    }
}

pub fn with_stopped_peer<T>(
    peer: &Peer,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    ensure_alive(&peer.process)?;
    let pid = peer.credentials.pid;
    trace(libc::PTRACE_SEIZE as u32, pid, 0)?;
    let mut guard = TraceGuard {
        pid,
        attached: true,
        stopped: false,
        signal: 0,
    };
    guard.stop()?;
    let result = ensure_alive(&peer.process).and_then(|_| {
        let mut tasks =
            std::fs::read_dir(format!("/proc/{pid}/task")).map_err(|error| error.to_string())?;
        if tasks
            .next()
            .transpose()
            .map_err(|error| error.to_string())?
            .is_none()
            || tasks
                .next()
                .transpose()
                .map_err(|error| error.to_string())?
                .is_some()
        {
            return Err("attachment requires a single-threaded child before exec".into());
        }
        operation()
    });
    match (result, guard.detach()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(detach)) => {
            Err(format!("{error}; releasing process attachment: {detach}"))
        }
    }
}

struct TraceGuard {
    pid: i32,
    attached: bool,
    stopped: bool,
    signal: i32,
}

impl TraceGuard {
    fn stop(&mut self) -> Result<(), String> {
        trace(libc::PTRACE_INTERRUPT as u32, self.pid, 0)?;
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::__WALL) };
            if result == self.pid {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error.to_string());
            }
        }
        if !libc::WIFSTOPPED(status) {
            self.attached = false;
            return Err("child exited before process attachment".into());
        }
        self.stopped = true;
        self.signal = match (status >> 16, libc::WSTOPSIG(status)) {
            (libc::PTRACE_EVENT_STOP, libc::SIGTRAP) => 0,
            (_, signal) => signal,
        };
        Ok(())
    }

    fn detach(&mut self) -> Result<(), String> {
        if !self.attached {
            return Ok(());
        }
        if !self.stopped {
            self.stop()?;
        }
        trace(libc::PTRACE_DETACH as u32, self.pid, self.signal)?;
        self.attached = false;
        Ok(())
    }
}

impl Drop for TraceGuard {
    fn drop(&mut self) {
        if let Err(error) = self.detach() {
            eprintln!("child detachment failed before request-thread exit: {error}");
        }
    }
}

fn trace(request: u32, pid: i32, signal: i32) -> Result<(), String> {
    if unsafe {
        libc::ptrace(
            request as _,
            pid,
            std::ptr::null_mut::<libc::c_void>(),
            signal as usize as *mut libc::c_void,
        )
    } < 0
    {
        return Err(format!(
            "process attachment: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}
