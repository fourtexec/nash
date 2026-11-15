//! Thin wrappers around libc.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};

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

pub fn write_fd(fd: i32, mut b: &[u8]) -> bool {
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const libc::c_void, b.len()) };
        if n < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            return false;
        }
        b = &b[n as usize..];
    }
    true
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

pub fn pipe() -> Option<(i32, i32)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        None
    } else {
        Some((fds[0], fds[1]))
    }
}

pub fn close(fd: i32) {
    unsafe {
        libc::close(fd);
    }
}

pub fn dup2(a: i32, b: i32) -> i32 {
    unsafe { libc::dup2(a, b) }
}

/// Duplicate `fd` to a descriptor >= 10 with close-on-exec set.
pub fn save_fd(fd: i32) -> i32 {
    unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) }
}

pub fn open(path: &str, flags: i32, mode: u32) -> i32 {
    match CString::new(path) {
        Ok(c) => loop {
            let fd = unsafe { libc::open(c.as_ptr(), flags, mode as libc::c_uint) };
            if fd < 0 && errno() == libc::EINTR {
                continue;
            }
            return fd;
        },
        Err(_) => -1,
    }
}

pub fn isatty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

pub fn read_all(fd: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            break;
        }
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}

/// Read one byte; None on EOF or error (errno EINTR is reported as Err).
pub fn read_byte(fd: i32) -> Result<Option<u8>, i32> {
    let mut b = [0u8; 1];
    let n = unsafe { libc::read(fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
    if n < 0 {
        Err(errno())
    } else if n == 0 {
        Ok(None)
    } else {
        Ok(Some(b[0]))
    }
}

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
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
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
        let n = libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), cfmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

pub fn home_of(user: &str) -> Option<String> {
    let c = CString::new(user).ok()?;
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
            return (std::env::var("USER").unwrap_or_else(|_| "user".into()), None);
        }
        (
            std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned(),
            Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned()),
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
    match CString::new(path) {
        Ok(c) => unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 },
        Err(_) => false,
    }
}

pub fn access(path: &str, mode: i32) -> bool {
    match CString::new(path) {
        Ok(c) => unsafe { libc::access(c.as_ptr(), mode) == 0 },
        Err(_) => false,
    }
}

pub fn is_dir(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

// ---------- signals ----------

pub const SIGNALS: &[(&str, i32)] = &[
    ("HUP", libc::SIGHUP),
    ("INT", libc::SIGINT),
    ("QUIT", libc::SIGQUIT),
    ("ILL", libc::SIGILL),
    ("TRAP", libc::SIGTRAP),
    ("ABRT", libc::SIGABRT),
    ("BUS", libc::SIGBUS),
    ("FPE", libc::SIGFPE),
    ("KILL", libc::SIGKILL),
    ("USR1", libc::SIGUSR1),
    ("SEGV", libc::SIGSEGV),
    ("USR2", libc::SIGUSR2),
    ("PIPE", libc::SIGPIPE),
    ("ALRM", libc::SIGALRM),
    ("TERM", libc::SIGTERM),
    ("CHLD", libc::SIGCHLD),
    ("CONT", libc::SIGCONT),
    ("STOP", libc::SIGSTOP),
    ("TSTP", libc::SIGTSTP),
    ("TTIN", libc::SIGTTIN),
    ("TTOU", libc::SIGTTOU),
    ("URG", libc::SIGURG),
    ("XCPU", libc::SIGXCPU),
    ("XFSZ", libc::SIGXFSZ),
    ("VTALRM", libc::SIGVTALRM),
    ("PROF", libc::SIGPROF),
    ("WINCH", libc::SIGWINCH),
    ("IO", libc::SIGIO),
    ("SYS", libc::SIGSYS),
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

/// Install the flag-setting handler for `sig` (without SA_RESTART so blocking
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
