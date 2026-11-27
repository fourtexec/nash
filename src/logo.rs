//! Terminal texted logo for `nash`, rendered on interactive startup and via
//! `nash --logo` (including from `~/.bashrc` when configured by `nash install`).

use crate::shell::Shell;
use crate::sys;

/// Check locale environment variables (`LC_ALL`, `LC_CTYPE`, `LANG`) for UTF-8.
pub fn env_is_utf8() -> bool {
    for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return has_utf8(&v);
            }
        }
    }
    false
}

fn has_utf8(v: &str) -> bool {
    let b = v.as_bytes();
    b.windows(4).any(|w| w.eq_ignore_ascii_case(b"utf8"))
        || b.windows(5).any(|w| w.eq_ignore_ascii_case(b"utf-8"))
}

fn shell_is_utf8(sh: &mut Shell) -> bool {
    for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(v) = sh.get(k) {
            if !v.is_empty() {
                return has_utf8(&v);
            }
        }
    }
    false
}

/// Render the texted `nash` logo banner.
/// Uses crisp Unicode block art on UTF-8 locales and a clean ASCII fallback
/// otherwise, with optional ANSI color styling (matching the cyan-emerald mark
/// and bright white wordmark).
pub fn render(version: &str, utf8: bool, color: bool) -> String {
    let ver = format!("v{}", version);
    if utf8 {
        if color {
            format!(
                "\n\
                 \x1b[1;36m  ▄▄   \x1b[1;97m  ███╗   ██╗ █████╗ ███████╗██╗  ██╗\x1b[0m\n\
                 \x1b[1;36m   ▀██▄\x1b[1;97m  ████╗  ██║██╔══██╗██╔════╝██║  ██║\x1b[0m\n\
                 \x1b[1;32m     ▀█▌\x1b[1;97m ██╔██╗ ██║███████║███████╗███████║\x1b[0m\n\
                 \x1b[1;32m   ▄██▀\x1b[1;97m  ██║╚██╗██║██╔══██║╚════██║██╔══██║\x1b[0m\n\
                 \x1b[1;36m  ▀▀   \x1b[1;97m  ██║ ╚████║██║  ██║███████║██║  ██║  \x1b[1;36m▄▄▄▄\x1b[0m\n\
                 \x1b[36m         ╚═╝  ╚═══╝╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝  \x1b[90m{}\x1b[0m\n\n",
                ver
            )
        } else {
            format!(
                "\n\
                 \x20\x20▄▄     ███╗   ██╗ █████╗ ███████╗██╗  ██╗\n\
                 \x20\x20 ▀██▄  ████╗  ██║██╔══██╗██╔════╝██║  ██║\n\
                 \x20\x20   ▀█▌ ██╔██╗ ██║███████║███████╗███████║\n\
                 \x20\x20 ▄██▀  ██║╚██╗██║██╔══██║╚════██║██╔══██║\n\
                 \x20\x20▀▀     ██║ ╚████║██║  ██║███████║██║  ██║  ▄▄▄▄\n\
                 \x20\x20       ╚═╝  ╚═══╝╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝  {}\n\n",
                ver
            )
        }
    } else if color {
        format!(
            "\n\
             \x1b[1;36m  \\\\   \x1b[1;97m  _   _    _    ____  _   _\x1b[0m\n\
             \x1b[1;36m   \\\\  \x1b[1;97m | \\ | |  / \\  / ___|| | | |\x1b[0m\n\
             \x1b[1;32m   //  \x1b[1;97m |  \\| | / _ \\ \\___ \\| |_| |\x1b[0m\n\
             \x1b[1;32m  //   \x1b[1;97m | |\\  |/ ___ \\ ___) |  _  |\x1b[0m\n\
             \x1b[36m        \x1b[1;97m|_| \\_/_/   \\_\\____/|_| |_|  \x1b[1;36m___  \x1b[90m{}\x1b[0m\n\n",
            ver
        )
    } else {
        format!(
            "\n\
             \x20\x20\\\\     _   _    _    ____  _   _\n\
             \x20\x20 \\\\   | \\ | |  / \\  / ___|| | | |\n\
             \x20\x20 //   |  \\| | / _ \\ \\___ \\| |_| |\n\
             \x20\x20//    | |\\  |/ ___ \\ ___) |  _  |\n\
             \x20\x20      |_| \\_/_/   \\_\\____/|_| |_|  ___  {}\n\n",
            ver
        )
    }
}

/// Print the startup logo for an interactive terminal session unless disabled
/// via `NASH_NO_LOGO=1`, `NASH_LOGO=0`, or `TERM=dumb`.
pub fn print_startup(sh: &mut Shell) {
    if !sh.interactive || !sys::isatty(2) {
        return;
    }
    if sh.get("TERM").as_deref() == Some("dumb") {
        return;
    }
    if sh.get("NASH_NO_LOGO").as_deref() == Some("1") || sh.get("NASH_LOGO").as_deref() == Some("0") {
        return;
    }
    let color = sh.get("NO_COLOR").map(|v| v.is_empty()).unwrap_or(true);
    let utf8 = shell_is_utf8(sh);
    let banner = render(crate::version(), utf8, color);
    sys::write_fd(2, banner.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_utf8_and_ascii_with_and_without_color() {
        let u_col = render("0.3.1", true, true);
        assert!(u_col.contains("\x1b[1;36m"));
        assert!(u_col.contains("v0.3.1"));
        assert!(u_col.contains("███╗"));

        let u_plain = render("0.3.1", true, false);
        assert!(!u_plain.contains('\x1b'));
        assert!(u_plain.contains("v0.3.1"));

        let a_col = render("0.3.1", false, true);
        assert!(a_col.contains("\x1b[1;36m"));
        assert!(a_col.contains("v0.3.1"));

        let a_plain = render("0.3.1", false, false);
        assert!(!a_plain.contains('\x1b'));
        assert!(a_plain.contains("____"));
        assert!(a_plain.contains("v0.3.1"));
    }
}
