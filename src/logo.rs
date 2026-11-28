//! Terminal logo for `nash`, generated directly from `assets/nash-logo-text.jpg`
//! and rendered on interactive startup and via `nash --logo` (including from
//! `~/.bashrc` when configured by `nashed install`).

use crate::shell::Shell;
use crate::sys;
use std::fmt::Write as _;

#[allow(dead_code)]
mod asset {
    include!(concat!(env!("OUT_DIR"), "/logo_asset.rs"));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorMode {
    None,
    Ansi256,
    TrueColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageProto {
    None,
    Kitty,
    ITerm2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb(u8, u8, u8);

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

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return false;
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

fn detect_color_mode(
    color: bool,
    term_program: Option<&str>,
    colorterm: Option<&str>,
) -> ColorMode {
    if !color {
        return ColorMode::None;
    }
    if let Some(ct) = colorterm {
        if contains_ignore_ascii_case(ct, "truecolor") || contains_ignore_ascii_case(ct, "24bit") {
            return ColorMode::TrueColor;
        }
    }
    if matches!(term_program, Some("Apple_Terminal")) {
        return ColorMode::Ansi256;
    }
    ColorMode::TrueColor
}

fn detect_image_proto(
    term: Option<&str>,
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
    kitty_window_id: Option<&str>,
    in_mux: bool,
) -> ImageProto {
    if in_mux {
        return ImageProto::None;
    }
    if kitty_window_id.map(|v| !v.is_empty()).unwrap_or(false) {
        return ImageProto::Kitty;
    }
    if let Some(t) = term {
        if contains_ignore_ascii_case(t, "kitty") || contains_ignore_ascii_case(t, "ghostty") {
            return ImageProto::Kitty;
        }
    }
    if let Some(tp) = term_program {
        if tp.eq_ignore_ascii_case("ghostty") || tp.eq_ignore_ascii_case("wezterm") {
            return ImageProto::Kitty;
        }
        if tp.eq_ignore_ascii_case("iTerm.app")
            || tp.eq_ignore_ascii_case("Tabby")
            || tp.eq_ignore_ascii_case("Hyper")
            || tp.eq_ignore_ascii_case("mintty")
        {
            return ImageProto::ITerm2;
        }
    }
    if let Some(lt) = lc_terminal {
        if lt.eq_ignore_ascii_case("iTerm2") {
            return ImageProto::ITerm2;
        }
    }
    ImageProto::None
}

fn rgb_to_ansi256(Rgb(r, g, b): Rgb) -> u8 {
    // Check if close to greyscale ramp (232..=255) or pure black/white.
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 12 {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + (((r as u16 - 8) * 24) / 247) as u8;
    }
    let ri = ((r as u16 * 5 + 127) / 255) as u8;
    let gi = ((g as u16 * 5 + 127) / 255) as u8;
    let bi = ((b as u16 * 5 + 127) / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}

fn sample_grid(tw: usize, th: usize) -> Vec<Option<Rgb>> {
    let sw = asset::GRID_W;
    let sh = asset::GRID_H;
    let mut out = vec![None; tw * th];
    if tw == sw && th == sh {
        for i in 0..(sw * sh) {
            let o = i * 4;
            if asset::PIXELS[o + 3] >= 128 {
                out[i] = Some(Rgb(
                    asset::PIXELS[o],
                    asset::PIXELS[o + 1],
                    asset::PIXELS[o + 2],
                ));
            }
        }
        return out;
    }

    for y in 0..th {
        let sy0 = (y * sh) / th;
        let sy1 = (((y + 1) * sh) / th).max(sy0 + 1).min(sh);
        for x in 0..tw {
            let sx0 = (x * sw) / tw;
            let sx1 = (((x + 1) * sw) / tw).max(sx0 + 1).min(sw);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            let mut sa = 0u32;
            let mut total = 0u32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let o = (sy * sw + sx) * 4;
                    let a = asset::PIXELS[o + 3] as u32;
                    if a > 0 {
                        sr += asset::PIXELS[o] as u32 * a;
                        sg += asset::PIXELS[o + 1] as u32 * a;
                        sb += asset::PIXELS[o + 2] as u32 * a;
                        sa += a;
                    }
                    total += 255;
                }
            }
            if sa > 0 && sa * 100 >= total * 42 {
                out[y * tw + x] = Some(Rgb(
                    ((sr + sa / 2) / sa).min(255) as u8,
                    ((sg + sa / 2) / sa).min(255) as u8,
                    ((sb + sa / 2) / sa).min(255) as u8,
                ));
            }
        }
    }
    out
}

fn push_fg(out: &mut String, mode: ColorMode, rgb: Rgb) {
    match mode {
        ColorMode::TrueColor => {
            let _ = write!(out, "\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2);
        }
        ColorMode::Ansi256 => {
            let _ = write!(out, "\x1b[38;5;{}m", rgb_to_ansi256(rgb));
        }
        ColorMode::None => {}
    }
}

fn push_bg(out: &mut String, mode: ColorMode, rgb: Rgb) {
    match mode {
        ColorMode::TrueColor => {
            let _ = write!(out, "\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2);
        }
        ColorMode::Ansi256 => {
            let _ = write!(out, "\x1b[48;5;{}m", rgb_to_ansi256(rgb));
        }
        ColorMode::None => {}
    }
}

fn render_blocks(version: &str, max_cols: usize, mode: ColorMode) -> String {
    let margin = 2usize;
    let avail = max_cols.saturating_sub(margin * 2);
    let tw = avail.clamp(28, asset::GRID_W);
    let raw_h = ((tw * asset::GRID_H + asset::GRID_W / 2) / asset::GRID_W).max(8);
    let th = if raw_h % 2 == 0 { raw_h } else { raw_h + 1 };

    let grid = sample_grid(tw, th);
    let mut out = String::with_capacity(tw * th * 14);
    out.push('\n');

    for y in (0..th).step_by(2) {
        // Trim trailing transparent columns so we never emit dangling spaces.
        let mut row_end = 0usize;
        for x in 0..tw {
            let top = grid[y * tw + x];
            let bot = grid[(y + 1) * tw + x];
            if top.is_some() || bot.is_some() {
                row_end = x + 1;
            }
        }
        if row_end == 0 {
            continue;
        }
        for _ in 0..margin {
            out.push(' ');
        }

        let mut cur_fg: Option<Rgb> = None;
        let mut cur_bg: Option<Rgb> = None;

        for x in 0..row_end {
            let top = grid[y * tw + x];
            let bot = grid[(y + 1) * tw + x];
            let (want_fg, want_bg, ch) = match (top, bot) {
                (None, None) => (None, None, ' '),
                (Some(t), None) => (Some(t), None, '▀'),
                (None, Some(b)) => (Some(b), None, '▄'),
                (Some(t), Some(b)) if t == b => (Some(t), None, '█'),
                (Some(t), Some(b)) => (Some(t), Some(b), '▀'),
            };

            if mode != ColorMode::None {
                if want_bg.is_none() && cur_bg.is_some() {
                    out.push_str("\x1b[49m");
                    cur_bg = None;
                }
                if let Some(fg) = want_fg {
                    if cur_fg != Some(fg) {
                        push_fg(&mut out, mode, fg);
                        cur_fg = Some(fg);
                    }
                }
                if let Some(bg) = want_bg {
                    if cur_bg != Some(bg) {
                        push_bg(&mut out, mode, bg);
                        cur_bg = Some(bg);
                    }
                }
            }
            out.push(ch);
        }

        if mode != ColorMode::None && (cur_fg.is_some() || cur_bg.is_some()) {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }

    let ver = format_version_label(version);
    let pad = (margin + tw).saturating_sub(ver.len()).max(margin);
    for _ in 0..pad {
        out.push(' ');
    }
    if mode != ColorMode::None {
        let _ = writeln!(out, "\x1b[90m{}\x1b[0m\n", ver);
    } else {
        let _ = writeln!(out, "{}\n", ver);
    }
    out
}

fn format_version_label(version: &str) -> String {
    if version.as_bytes().first().map(|b| b.is_ascii_digit()).unwrap_or(false) {
        format!("v{}", version)
    } else {
        version.to_string()
    }
}

fn render_ascii(version: &str, color: bool) -> String {
    let ver = format_version_label(version);
    if color {
        format!(
            "\n\
             \x1b[1;36m  |\\   |  \x1b[1;97m               _     \x1b[0m\n\
             \x1b[1;36m  | \\  \\  \x1b[1;97m _ __   __ _ ___| |__  \x1b[0m\n\
             \x1b[1;32m  |  \\  > \x1b[1;97m| '_ \\ / _` / __| '_ \\ \x1b[0m\n\
             \x1b[1;32m  |   \\/  \x1b[1;97m| | | | (_| \\__ \\ | | |\x1b[0m\n\
             \x1b[1;36m  |       \x1b[1;97m|_| |_|\\__,_|___/_| |_| \x1b[1;36m___  \x1b[90m{}\x1b[0m\n\n",
            ver
        )
    } else {
        format!(
            "\n\
             \x20\x20|\\   |                 _     \n\
             \x20\x20| \\  \\   _ __   __ _ ___| |__  \n\
             \x20\x20|  \\  > | '_ \\ / _` / __| '_ \\ \n\
             \x20\x20|   \\/  | | | | (_| \\__ \\ | | |\n\
             \x20\x20|       |_| |_|\\__,_|___/_| |_| ___  {}\n\n",
            ver
        )
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((data.len() + 2) / 3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn render_kitty_png(version: &str, max_cols: usize) -> String {
    let cols = max_cols.saturating_sub(4).clamp(24, 54);
    let b64 = base64_encode(asset::PNG);
    let bytes = b64.as_bytes();
    let mut out = String::with_capacity(b64.len() + 256);
    out.push_str("\n  ");

    let chunk_size = 4096usize;
    let mut pos = 0usize;
    let mut first = true;
    while pos < bytes.len() {
        let end = (pos + chunk_size).min(bytes.len());
        let more = if end < bytes.len() { 1 } else { 0 };
        let slice = &b64[pos..end];
        if first {
            // q=2 silences all terminal replies so stdin is never polluted.
            let _ = write!(
                out,
                "\x1b_Ga=T,f=100,c={},q=2,m={};{}\x1b\\",
                cols, more, slice
            );
            first = false;
        } else {
            let _ = write!(out, "\x1b_Gm={};{}\x1b\\", more, slice);
        }
        pos = end;
    }

    let ver = format_version_label(version);
    let pad = (2 + cols).saturating_sub(ver.len()).max(2);
    out.push('\n');
    for _ in 0..pad {
        out.push(' ');
    }
    let _ = writeln!(out, "\x1b[90m{}\x1b[0m\n", ver);
    out
}

fn render_iterm2_png(version: &str, max_cols: usize) -> String {
    let cols = max_cols.saturating_sub(4).clamp(24, 54);
    let b64 = base64_encode(asset::PNG);
    let mut out = String::with_capacity(b64.len() + 192);
    let _ = write!(
        out,
        "\n  \x1b]1337;File=name=bmFzaC1sb2dvLXRleHQucG5n;size={};width={};preserveAspectRatio=1;inline=1:{}\x07\n",
        asset::PNG.len(),
        cols,
        b64
    );
    let ver = format_version_label(version);
    let pad = (2 + cols).saturating_sub(ver.len()).max(2);
    for _ in 0..pad {
        out.push(' ');
    }
    let _ = writeln!(out, "\x1b[90m{}\x1b[0m\n", ver);
    out
}

/// Render the `nash` logo banner from `assets/nash-logo-text.jpg`.
/// Uses Unicode half-block pixel art on UTF-8 locales and a clean ASCII
/// fallback otherwise, with optional ANSI truecolor styling.
#[allow(dead_code)]
pub fn render(version: &str, utf8: bool, color: bool) -> String {
    if utf8 {
        let mode = if color {
            ColorMode::TrueColor
        } else {
            ColorMode::None
        };
        render_blocks(version, 80, mode)
    } else {
        render_ascii(version, color)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_for_terminal(
    version: &str,
    utf8: bool,
    color: bool,
    cols: usize,
    is_tty: bool,
    logo_mode: Option<&str>,
    term: Option<&str>,
    term_program: Option<&str>,
    colorterm: Option<&str>,
    lc_terminal: Option<&str>,
    kitty_window_id: Option<&str>,
    in_mux: bool,
) -> String {
    if logo_mode.map(|m| m.eq_ignore_ascii_case("text")).unwrap_or(false) || !utf8 {
        return render_ascii(version, color);
    }

    let force_blocks = logo_mode
        .map(|m| m.eq_ignore_ascii_case("blocks"))
        .unwrap_or(false);

    if is_tty && color && !force_blocks {
        match detect_image_proto(term, term_program, lc_terminal, kitty_window_id, in_mux) {
            ImageProto::Kitty => return render_kitty_png(version, cols),
            ImageProto::ITerm2 => return render_iterm2_png(version, cols),
            ImageProto::None => {}
        }
    }

    let color_mode = detect_color_mode(color, term_program, colorterm);
    render_blocks(version, cols, color_mode)
}

/// Print the `nash` logo for `nash --logo` to stdout.
pub fn print_cli(version: &str) {
    let color = std::env::var_os("NO_COLOR")
        .map(|v| v.is_empty())
        .unwrap_or(true);
    let utf8 = env_is_utf8();
    let is_tty = sys::isatty(1);
    let (cols, _) = sys::term_size();
    let logo_mode = std::env::var("NASH_LOGO_MODE").ok();
    let term = std::env::var("TERM").ok();
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let colorterm = std::env::var("COLORTERM").ok();
    let lc_terminal = std::env::var("LC_TERMINAL").ok();
    let kitty_window_id = std::env::var("KITTY_WINDOW_ID").ok();
    let in_mux = std::env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false)
        || std::env::var("STY").map(|v| !v.is_empty()).unwrap_or(false);

    let banner = render_for_terminal(
        version,
        utf8,
        color,
        cols,
        is_tty,
        logo_mode.as_deref(),
        term.as_deref(),
        term_program.as_deref(),
        colorterm.as_deref(),
        lc_terminal.as_deref(),
        kitty_window_id.as_deref(),
        in_mux,
    );
    sys::write_fd(1, banner.as_bytes());
}

/// Print the startup logo for an interactive terminal session unless disabled
/// via `NASH_NO_LOGO=1`, `NASH_LOGO=0`, or `TERM=dumb`.
pub fn print_startup(sh: &mut Shell) {
    if !sh.interactive || !sys::isatty(2) {
        return;
    }
    let term = sh.get("TERM");
    if term.as_deref() == Some("dumb") {
        return;
    }
    if sh.get("NASH_NO_LOGO").as_deref() == Some("1") || sh.get("NASH_LOGO").as_deref() == Some("0")
    {
        return;
    }
    let color = sh.get("NO_COLOR").map(|v| v.is_empty()).unwrap_or(true);
    let utf8 = shell_is_utf8(sh);
    let (cols, _) = sys::term_size();
    let logo_mode = sh.get("NASH_LOGO_MODE");
    let term_program = sh.get("TERM_PROGRAM");
    let colorterm = sh.get("COLORTERM");
    let lc_terminal = sh.get("LC_TERMINAL");
    let kitty_window_id = sh.get("KITTY_WINDOW_ID");
    let in_mux = sh.get("TMUX").map(|v| !v.is_empty()).unwrap_or(false)
        || sh.get("STY").map(|v| !v.is_empty()).unwrap_or(false);

    let banner = render_for_terminal(
        crate::version(),
        utf8,
        color,
        cols,
        true,
        logo_mode.as_deref(),
        term.as_deref(),
        term_program.as_deref(),
        colorterm.as_deref(),
        lc_terminal.as_deref(),
        kitty_window_id.as_deref(),
        in_mux,
    );
    sys::write_fd(2, banner.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                i += 2;
                while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1;
                continue;
            }
            if let Some(ch) = s[i..].chars().next() {
                out.push(ch);
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        out
    }

    #[test]
    fn embedded_asset_decodes_cleanly_from_jpg() {
        assert_eq!(asset::GRID_W, 64);
        assert_eq!(asset::GRID_H, 18);
        assert_eq!(asset::PIXELS.len(), asset::GRID_W * asset::GRID_H * 4);
        // Valid PNG signature + IHDR dimensions 256x72
        assert_eq!(&asset::PNG[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&asset::PNG[12..16], b"IHDR");
        let w = u32::from_be_bytes([asset::PNG[16], asset::PNG[17], asset::PNG[18], asset::PNG[19]]);
        let h = u32::from_be_bytes([asset::PNG[20], asset::PNG[21], asset::PNG[22], asset::PNG[23]]);
        assert_eq!((w, h), (256, 72));

        // Verify logo contains bright white wordmark pixels, cyan/green N mark pixels, and transparent background
        let mut white = 0usize;
        let mut cyan_green = 0usize;
        let mut transparent = 0usize;
        for px in asset::PIXELS.chunks_exact(4) {
            if px[3] == 0 {
                transparent += 1;
            } else if px[0] > 200 && px[1] > 200 && px[2] > 200 {
                white += 1;
            } else if px[1] > 140 && px[0] < 100 {
                cyan_green += 1;
            }
        }
        assert!(white > 80, "expected white wordmark pixels, got {}", white);
        assert!(
            cyan_green > 80,
            "expected cyan/emerald N mark pixels, got {}",
            cyan_green
        );
        assert!(
            transparent > 200,
            "expected transparent background pixels, got {}",
            transparent
        );
    }

    #[test]
    fn renders_utf8_and_ascii_with_and_without_color() {
        let u_col = render("1.0.1", true, true);
        assert!(u_col.contains("\x1b[38;2;"));
        assert!(u_col.contains("v1.0.1"));
        assert!(u_col.contains('▀') || u_col.contains('▄') || u_col.contains('█'));

        let u_plain = render("1.0.1", true, false);
        assert!(!u_plain.contains('\x1b'));
        assert!(u_plain.contains("v1.0.1"));
        assert!(u_plain.contains('▀') || u_plain.contains('█'));

        let a_col = render("1.0.1", false, true);
        assert!(a_col.contains("\x1b[1;36m"));
        assert!(a_col.contains("v1.0.1"));

        let a_plain = render("1.0.1", false, false);
        assert!(!a_plain.contains('\x1b'));
        assert!(a_plain.contains("___"));
        assert!(a_plain.contains("v1.0.1"));

        let dev_blocks = render("dev43", true, false);
        assert!(dev_blocks.contains("dev43"));
        assert!(!dev_blocks.contains("vdev43"));
    }

    #[test]
    fn narrow_terminal_fits_within_column_budget() {
        let narrow = render_blocks("1.0.1", 44, ColorMode::TrueColor);
        for line in strip_ansi(&narrow).lines() {
            assert!(
                line.chars().count() <= 44,
                "line exceeded 44 cols ({}): {:?}",
                line.chars().count(),
                line
            );
        }
    }

    #[test]
    fn apple_terminal_uses_ansi256_and_protocols_detect_cleanly() {
        let mac = render_for_terminal(
            "1.0.1",
            true,
            true,
            80,
            true,
            None,
            Some("xterm-256color"),
            Some("Apple_Terminal"),
            None,
            None,
            None,
            false,
        );
        assert!(mac.contains("\x1b[38;5;"));
        assert!(!mac.contains("\x1b[38;2;"));

        let kitty = render_for_terminal(
            "1.0.1",
            true,
            true,
            100,
            true,
            None,
            Some("xterm-kitty"),
            None,
            Some("truecolor"),
            None,
            Some("1"),
            false,
        );
        assert!(kitty.contains("\x1b_Ga=T,f=100,"));
        assert!(kitty.contains(",q=2,"));
        assert!(kitty.contains("m=0;"));
        assert!(kitty.contains("v1.0.1"));

        let iterm = render_for_terminal(
            "1.0.1",
            true,
            true,
            100,
            true,
            None,
            Some("xterm-256color"),
            Some("iTerm.app"),
            Some("truecolor"),
            Some("iTerm2"),
            None,
            false,
        );
        assert!(iterm.contains("\x1b]1337;File="));
        assert!(iterm.contains("inline=1:"));
        assert!(iterm.contains("v1.0.1"));

        // Inside tmux, inline image protocols fall back to half-block rendering.
        let tmux_kitty = render_for_terminal(
            "1.0.1",
            true,
            true,
            100,
            true,
            None,
            Some("xterm-kitty"),
            None,
            Some("truecolor"),
            None,
            Some("1"),
            true,
        );
        assert!(!tmux_kitty.contains("\x1b_G"));
        assert!(tmux_kitty.contains("\x1b[38;2;"));
    }
}
