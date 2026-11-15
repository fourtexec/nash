//! Thin wrappers around OS syscalls, using [`rustix`] where possible so the
//! vast majority of the code is `unsafe`-free.  Non-Linux targets (macOS)
//! fall back to `libc`.

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use rustix::fd::{BorrowedFd, IntoRawFd};
use rustix::fs::{Access, Mode, OFlags};



// Constants and signal constants re-exported at sys module level (libc is
// cross-platform, so no cfg gate needed).
pub use libc::{
    EINTR, EAGAIN, EWOULDBLOCK, EISDIR, ENOSPC, EDQUOT, EIO, ENODEV,
    R_OK, W_OK, X_OK, F_OK,
    POLLIN, POLLOUT, POLLERR, POLLHUP, POLLNVAL,
    RLIMIT_AS, RLIMIT_CORE, RLIMIT_DATA, RLIMIT_FSIZE, RLIMIT_MEMLOCK,
    RLIMIT_NOFILE, RLIMIT_NPROC, RLIMIT_RSS, RLIMIT_STACK, RLIMIT_CPU,
    RLIM_INFINITY,
    WUNTRACED, WNOHANG,
    STDIN_FILENO, STDOUT_FILENO, STDERR_FILENO,
    BRKINT, ICRNL, INPCK, ISTRIP, IXON, CS8,
    sigaction, sigemptyset, sigaddset, sigdelset, sigismember,
    SIG_DFL, SIG_IGN, SIG_ERR,
    SIGHUP, SIGINT, SIGQUIT, SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE,
    SIGKILL, SIGUSR1, SIGSEGV, SIGUSR2, SIGPIPE, SIGALRM, SIGTERM, SIGCHLD,
    SIGCONT, SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU, SIGURG, SIGXCPU, SIGXFSZ,
    SIGVTALRM, SIGPROF, SIGWINCH, SIGIO, SIGSYS,
    ECHO, ICANON, ISIG, IEXTEN, VMIN, VTIME,
    TCSANOW, TCSADRAIN, TCSAFLUSH,
    rlimit, rlim_t,
    WCONTINUED, ENOENT, ENOEXEC,
    F_GETFD,
    getpwnam, getpwuid, gethostname,
    rusage, RUSAGE_SELF, RUSAGE_CHILDREN,
    c_char, c_int, c_uint, c_void,
};



/// Errno value.
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

pub fn strerror(e: i32) -> String {
    let s = std::io::Error::from_raw_os_error(e).to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

pub fn last_error() -> String {
    strerror(errno())
}

/// Write `b` to `fd`, retrying on `EINTR`.
pub fn write_fd(fd: i32, mut b: &[u8]) -> bool {
    // Use rustix File for safe write (no unsafe in this function body).
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| -> std::io::Result<()> {
        while !b.is_empty() {
            match file.write(b) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => b = &b[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })();
    // Prevent the File from closing the fd on drop (caller owns the fd).
    std::mem::forget(file);
    result.is_ok()
}

#[macro_export]
macro_rules! out {
    ($($a:tt)*) => { $crate::sys::write_fd(1, format!($($a)*).as_bytes()) };
}

#[macro_export]
macro_rules! err {
    ($($a:tt)*) => {{ $crate::sys::write_fd(2, format!($($a)*).as_bytes()); }};
}

/// Print an error message prefixed with the shell name.
#[macro_export]
macro_rules! sherr {
    ($($a:tt)*) => {{ $crate::sys::write_fd(2, format!("nash: {}\n", format!($($a)*)).as_bytes()); }};
}

/// Create a pipe. Returns `(read_fd, write_fd)`.
pub fn pipe() -> Option<(i32, i32)> {
    match rustix::pipe::pipe() {
        Ok((r, w)) => Some((r.into_raw_fd(), w.into_raw_fd())),
        Err(_) => None,
    }
}

/// Close `fd`.
pub fn close(fd: i32) {
    // Use rustix File to close safely.
    let _file = unsafe { File::from_raw_fd(fd) };
    // File::drop closes the fd — that's what we want.
}

/// Duplicate `fd` to `b`.
pub fn dup2(a: i32, b: i32) -> i32 {
    // rustix doesn't expose dup2 directly in 0.38 stable API; use libc.
    unsafe { libc::dup2(a, b) }
}

/// Duplicate `fd` to a descriptor >= `min_fd` with close-on-exec set.
pub fn save_fd(fd: i32) -> i32 {
    // fcntl_dupfd_cloexec may not exist in this rustix version; use libc.
    unsafe {
        libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10)
    }
}

/// Open `path` with `flags` and `mode`. Returns the fd or `-1`.
pub fn open(path: &str, flags: i32, mode: u32) -> i32 {
    let c = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return -1,
    };
    let oflags = OFlags::from_bits_truncate(flags as u32);
    let mode = {
        #[cfg(target_os = "macos")]
        let m = Mode::from_bits_truncate(mode as u16);
        #[cfg(not(target_os = "macos"))]
        let m = Mode::from_bits_truncate(mode as u32);
        m
    };
    match rustix::fs::openat(rustix::fs::CWD, c.as_c_str(), oflags, mode) {
        Ok(f) => f.into_raw_fd(),
        Err(_) => -1,
    }
}

/// Return `true` if `fd` refers to a terminal.
pub fn isatty(fd: i32) -> bool {
    unsafe {
        rustix::termios::isatty(BorrowedFd::borrow_raw(fd))
    }
}

/// Read all bytes from `fd` until EOF or error.
pub fn read_all(fd: i32) -> Vec<u8> {
    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    std::mem::forget(file);
    out
}

/// Read one byte from `fd`. `None` on EOF; `Err(errno)` on error (EINTR reported as Err).
pub fn read_byte(fd: i32) -> Result<Option<u8>, i32> {
    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut b = [0u8; 1];
    match file.read(&mut b) {
        Ok(0) => {
            std::mem::forget(file);
            Ok(None)
        }
        Ok(_) => {
            std::mem::forget(file);
            Ok(Some(b[0]))
        }
        Err(e) => {
            std::mem::forget(file);
            let errno = e.raw_os_error().unwrap_or(0);
            Err(errno)
        }
    }
}

/// Decode a `waitpid` status.
pub enum WaitStatus {
    Exited(i32),
    Signaled(i32),
    Stopped,
    Continued,
}

pub fn decode_status(st: i32) -> WaitStatus {
    if libc::WIFEXITED(st) {
        WaitStatus::Exited(libc::WEXITSTATUS(st))
    } else if libc::WIFSIGNALED(st) {
        WaitStatus::Signaled(libc::WTERMSIG(st))
    } else if libc::WIFSTOPPED(st) {
        WaitStatus::Stopped
    } else {
        WaitStatus::Continued
    }
}

/// Terminal size in `(columns, rows)`.
pub fn term_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return (ws.ws_col as usize, ws.ws_row.max(1) as usize);
        }
    }
    (80, 24)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn strftime(fmt: &str, t: i64) -> String {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&tt, &mut tm);
        let cfmt = match CString::new(fmt) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        let mut buf = vec![0u8; 256];
        let n = libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            cfmt.as_ptr(),
            &tm,
        );
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

pub fn home_of(user: &str) -> Option<String> {
    let c = match CString::new(user) {
        Ok(c) => c,
        Err(_) => return None,
    };
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

pub fn current_user() -> (String, Option<String>) {
    unsafe {
        let pw = libc::getpwuid(libc::geteuid());
        if pw.is_null() {
            return (
                std::env::var("USER").unwrap_or_else(|_| "user".into()),
                None,
            );
        }
        (
            std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned(),
            Some(
                std::ffi::CStr::from_ptr((*pw).pw_dir)
                    .to_string_lossy()
                    .into_owned(),
            ),
        )
    }
}

pub fn hostname() -> String {
    let mut buf = vec![0u8; 256];
    unsafe {
        if libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) != 0 {
            return "localhost".into();
        }
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

pub fn is_executable(path: &str) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        _ => return false,
    }
    rustix::fs::access(path, Access::EXEC_OK).is_ok()
}

pub fn access(path: &str, mode: i32) -> bool {
    let mut am = Access::empty();
    if mode & libc::R_OK != 0 {
        am |= Access::READ_OK;
    }
    if mode & libc::W_OK != 0 {
        am |= Access::WRITE_OK;
    }
    if mode & libc::X_OK != 0 {
        am |= Access::EXEC_OK;
    }
    // F_OK: just check if the file exists
    if mode & libc::F_OK != 0 {
        if std::fs::metadata(path).is_err() {
            return false;
        }
    }
    rustix::fs::access(path, am).is_ok()
}

pub fn is_dir(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

// ---------- signals ----------

pub const SIGNALS: &[(&str, i32)] = &[
    ("HUP", libc::SIGHUP as i32),
    ("INT", libc::SIGINT as i32),
    ("QUIT", libc::SIGQUIT as i32),
    ("ILL", libc::SIGILL as i32),
    ("TRAP", libc::SIGTRAP as i32),
    ("ABRT", libc::SIGABRT as i32),
    ("BUS", libc::SIGBUS as i32),
    ("FPE", libc::SIGFPE as i32),
    ("KILL", libc::SIGKILL as i32),
    ("USR1", libc::SIGUSR1 as i32),
    ("SEGV", libc::SIGSEGV as i32),
    ("USR2", libc::SIGUSR2 as i32),
    ("PIPE", libc::SIGPIPE as i32),
    ("ALRM", libc::SIGALRM as i32),
    ("TERM", libc::SIGTERM as i32),
    ("CHLD", libc::SIGCHLD as i32),
    ("CONT", libc::SIGCONT as i32),
    ("STOP", libc::SIGSTOP as i32),
    ("TSTP", libc::SIGTSTP as i32),
    ("TTIN", libc::SIGTTIN as i32),
    ("TTOU", libc::SIGTTOU as i32),
    ("URG", libc::SIGURG as i32),
    ("XCPU", libc::SIGXCPU as i32),
    ("XFSZ", libc::SIGXFSZ as i32),
    ("VTALRM", libc::SIGVTALRM as i32),
    ("PROF", libc::SIGPROF as i32),
    ("WINCH", libc::SIGWINCH as i32),
    ("IO", libc::SIGIO as i32),
    ("SYS", libc::SIGSYS as i32),
];

pub fn sig_name(n: i32) -> String {
    for (name, num) in SIGNALS {
        if *num == n {
            return name.to_string();
        }
    }
    n.to_string()
}

pub fn sig_number(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return if (0..65).contains(&n) { Some(n) } else { None };
    }
    let up = s.to_ascii_uppercase();
    let up = up.strip_prefix("SIG").unwrap_or(&up);
    if up == "EXIT" {
        return Some(0);
    }
    SIGNALS.iter().find(|(n, _)| *n == up).map(|(_, n)| *n)
}

pub fn sig_description(sig: i32) -> &'static str {
    match sig {
        libc::SIGHUP => "Hangup",
        libc::SIGQUIT => "Quit",
        libc::SIGILL => "Illegal instruction",
        libc::SIGABRT => "Aborted",
        libc::SIGBUS => "Bus error",
        libc::SIGFPE => "Floating point exception",
        libc::SIGKILL => "Killed",
        libc::SIGSEGV => "Segmentation fault",
        libc::SIGTERM => "Terminated",
        libc::SIGALRM => "Alarm clock",
        libc::SIGUSR1 => "User defined signal 1",
        libc::SIGUSR2 => "User defined signal 2",
        _ => "",
    }
}

#[allow(clippy::declare_interior_mutable_const)]
const FALSE: AtomicBool = AtomicBool::new(false);
pub static PENDING: [AtomicBool; 65] = [FALSE; 65];
pub static ANY_PENDING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    if (0..65).contains(&sig) {
        PENDING[sig as usize].store(true, Ordering::SeqCst);
        ANY_PENDING.store(true, Ordering::SeqCst);
    }
}

/// Install the flag-setting handler for `sig` (without `SA_RESTART` so blocking
/// reads are interrupted).
pub fn catch_signal(sig: i32) {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

pub fn ignore_signal(sig: i32) {
    unsafe {
        libc::signal(sig, libc::SIG_IGN);
    }
}

pub fn default_signal(sig: i32) {
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
    }
}

pub fn take_pending(sig: i32) -> bool {
    PENDING[sig as usize].swap(false, Ordering::SeqCst)
}

/// Set the file mode creation mask.
pub fn umask(mask: libc::mode_t) -> libc::mode_t {
    unsafe { libc::umask(mask) }
}

/// Send a signal to a process or process group.
pub fn kill(pid: i32, sig: i32) -> i32 {
    unsafe { libc::kill(pid, sig) }
}

/// Set the foreground process group of the terminal.
pub fn tcsetpgrp(fd: i32, pgid: i32) -> i32 {
    unsafe { libc::tcsetpgrp(fd, pgid) }
}

/// Wait for a child process to change state. Returns the pid, or None on error.
pub fn waitpid(pid: i32, opts: i32) -> Option<i32> {
    let mut st: libc::c_int = 0;
    let r = unsafe { libc::waitpid(pid, &mut st, opts) };
    if r < 0 { None } else { Some(r) }
}

/// Wait for a child process and return its exit status, or None on error.
pub fn waitpid_status(pid: i32, opts: i32) -> Option<i32> {
    let mut st: libc::c_int = 0;
    let r = unsafe { libc::waitpid(pid, &mut st, opts) };
    if r < 0 { None } else { Some(st) }
}

/// Wait for any child process and return (pid, status), or None on error/no child.
pub fn waitpid_any(opts: i32) -> Option<(i32, i32)> {
    let mut st: libc::c_int = 0;
    let r = unsafe { libc::waitpid(-1, &mut st, opts) };
    if r < 0 { None } else { Some((r, st)) }
}

/// Return the process ID of the calling process.
pub fn getpid() -> i32 {
    unsafe { libc::getpid() }
}

/// Return the effective user ID.
pub fn geteuid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

/// Return the effective group ID.
pub fn getegid() -> libc::gid_t {
    unsafe { libc::getegid() }
}

/// Return the real user ID.
pub fn getppid() -> libc::pid_t {
    unsafe { libc::getppid() }
}

/// Create a new process.
pub fn fork() -> i32 {
    unsafe { libc::fork() }
}

/// Set the process group ID of a process.
pub fn setpgid(pid: i32, pgid: i32) -> i32 {
    unsafe { libc::setpgid(pid, pgid) }
}

/// Get resource limits.
pub fn getrlimit(resource: libc::c_int) -> Option<libc::rlimit> {
    let mut rl: libc::rlimit = unsafe { std::mem::zeroed() };
    match unsafe { libc::getrlimit(resource, &mut rl) } {
        0 => Some(rl),
        _ => None,
    }
}

/// Set resource limits.
pub fn setrlimit(resource: libc::c_int, rl: &libc::rlimit) -> i32 {
    unsafe { libc::setrlimit(resource, rl) }
}

/// Get terminal attributes.
pub fn tcgetattr(fd: i32) -> Option<libc::termios> {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    match unsafe { libc::tcgetattr(fd, &mut t) } {
        0 => Some(t),
        _ => None,
    }
}

/// Set terminal attributes.
pub fn tcsetattr(fd: i32, actions: i32, t: &libc::termios) -> i32 {
    unsafe { libc::tcsetattr(fd, actions, t) }
}

/// Terminate the process immediately.
pub fn _exit(status: i32) -> ! {
    unsafe { libc::_exit(status) }
}

/// Poll file descriptors (wrapper around libc::poll).
pub fn poll(fds: &mut [libc::pollfd], timeout: i32) -> i32 {
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::c_uint, timeout) }
}

/// Fcntl operation F_GETFD.
pub fn fcntl_getfd(fd: i32) -> i32 {
    unsafe { libc::fcntl(fd, libc::F_GETFD) }
}
