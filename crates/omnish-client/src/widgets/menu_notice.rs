// crates/omnish-client/src/widgets/menu_notice.rs
//
// Transient notices shown in place of the menu title (issue #693).
//
// A notice replaces the breadcrumb line with a colored bar, holds for
// `HOLD`, then fades its background into the terminal background over
// `FADE` before the breadcrumb comes back. Menu callbacks post notices with
// `notify()`; `run_menu` picks them up and drives the animation from its
// key-read loop. Output printed directly by a callback is overwritten by the
// next menu redraw, which is why save errors used to go unseen.
//
// Daemon `NoticePush` messages (e.g. client deploy or plugin install results)
// are routed here with `notify_push()` while a menu is open, instead of
// waiting for the shell loop that only runs after the menu and chat close.
// A pushed notice the menu could not show before closing is handed back
// through `take_unshown_pushes()`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::display::{BOLD, RESET};

/// How long a notice stays at full color.
pub const HOLD: Duration = Duration::from_millis(2500);
/// How long the background takes to fade out after `HOLD`.
pub const FADE: Duration = Duration::from_millis(1000);
/// Number of fade frames (each one a repaint of the title line).
pub const FADE_STEPS: u32 = 10;

/// Severity of a notice; picks its background color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Error,
    Info,
}

impl NoticeLevel {
    fn base_rgb(self) -> (u8, u8, u8) {
        match self {
            NoticeLevel::Error => (176, 36, 36),
            NoticeLevel::Info => (32, 96, 176),
        }
    }

    /// 16-color background used when the terminal has no 256-color support.
    fn basic_bg(self) -> &'static str {
        match self {
            NoticeLevel::Error => "\x1b[41m",
            NoticeLevel::Info => "\x1b[44m",
        }
    }
}

struct Pending {
    level: NoticeLevel,
    text: String,
    /// Came from a daemon push rather than a menu callback.
    pushed: bool,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// Pushed notices posted while a menu was open but never shown by it.
static UNSHOWN_PUSHES: Mutex<Vec<(NoticeLevel, String)>> = Mutex::new(Vec::new());

fn post(level: NoticeLevel, text: String, pushed: bool) {
    // Notices are single-line; fold any newlines a message carries.
    let text = text.replace(['\r', '\n'], " ");
    let mut pending = PENDING.lock().unwrap();
    // A newer notice replaces an older one; keep a replaced push so it is
    // still shown the usual way once the menu closes.
    if let Some(old) = pending.take() {
        if old.pushed {
            UNSHOWN_PUSHES.lock().unwrap().push((old.level, old.text));
        }
    }
    *pending = Some(Pending { level, text, pushed });
}

/// Show `text` in the title line of the running menu. A newer notice
/// replaces an older one. Safe to call when no menu is open: `run_menu`
/// clears stale notices when it starts.
pub fn notify(level: NoticeLevel, text: impl Into<String>) {
    post(level, text.into(), false);
}

/// Serializes tests that touch the global notice state.
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// True while a menu is open (and will show routed notices).
pub fn menu_open() -> bool {
    MENU_OPEN.load(Ordering::SeqCst)
}

/// Route a daemon notice to the open menu. Returns false (and does
/// nothing) when no menu is open, so the caller shows it the usual way.
pub fn notify_push(level: NoticeLevel, text: impl Into<String>) -> bool {
    if !MENU_OPEN.load(Ordering::SeqCst) {
        return false;
    }
    post(level, text.into(), true);
    true
}

/// Pushed notices a menu received but closed before showing (or that a
/// newer notice replaced). The shell loop shows them as ordinary notices.
pub fn take_unshown_pushes() -> Vec<(NoticeLevel, String)> {
    std::mem::take(&mut *UNSHOWN_PUSHES.lock().unwrap())
}

/// Take the notice posted since the last call, if any.
pub(super) fn take_pending() -> Option<(NoticeLevel, String)> {
    PENDING.lock().unwrap().take().map(|p| (p.level, p.text))
}

/// Marks a menu as open for its lifetime, so daemon notices are routed to
/// it. On drop, a pushed notice still pending goes to `take_unshown_pushes`.
pub(super) struct MenuOpenGuard;

impl MenuOpenGuard {
    pub fn new() -> Self {
        // Drop anything a previous menu left behind.
        let _ = take_pending();
        MENU_OPEN.store(true, Ordering::SeqCst);
        MenuOpenGuard
    }
}

impl Drop for MenuOpenGuard {
    fn drop(&mut self) {
        MENU_OPEN.store(false, Ordering::SeqCst);
        if let Some(p) = PENDING.lock().unwrap().take() {
            if p.pushed {
                UNSHOWN_PUSHES.lock().unwrap().push((p.level, p.text));
            }
        }
    }
}

/// How the terminal can render background colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColorMode {
    /// 24-bit `ESC[48;2;r;g;bm`: smooth fade.
    TrueColor,
    /// xterm 256-color cube: the fade snaps to a few cube steps.
    Ansi256,
    /// 16 colors only: no fade, the bar disappears when the time is up.
    Basic,
}

/// Pick the color mode from `$COLORTERM` and `$TERM`.
pub(super) fn color_mode_from(colorterm: Option<&str>, term: Option<&str>) -> ColorMode {
    let colorterm = colorterm.unwrap_or("").to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorMode::TrueColor;
    }
    if term.unwrap_or("").contains("256color") {
        return ColorMode::Ansi256;
    }
    ColorMode::Basic
}

/// Background assumed when the terminal does not answer the OSC 11 query.
const DEFAULT_TERM_BG: (u8, u8, u8) = (0, 0, 0);

/// An active notice in the title line.
pub(super) struct Notice {
    level: NoticeLevel,
    text: String,
    start: Instant,
    mode: ColorMode,
    term_bg: (u8, u8, u8),
}

impl Notice {
    /// A notice starting now, using the terminal's color mode and background.
    pub fn new(level: NoticeLevel, text: String) -> Self {
        let mode = color_mode_from(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        );
        // Only a fade needs the real background.
        let term_bg = if mode == ColorMode::Basic {
            DEFAULT_TERM_BG
        } else {
            terminal_bg().unwrap_or(DEFAULT_TERM_BG)
        };
        Self::with(level, text, Instant::now(), mode, term_bg)
    }

    fn with(level: NoticeLevel, text: String, start: Instant, mode: ColorMode, term_bg: (u8, u8, u8)) -> Self {
        Self { level, text, start, mode, term_bg }
    }

    /// Frame at `now`: 0 while holding, 1..=FADE_STEPS while fading, None
    /// once finished.
    fn frame(&self, now: Instant) -> Option<u32> {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed < HOLD {
            return Some(0);
        }
        let fading = elapsed - HOLD;
        if fading >= FADE {
            return None;
        }
        let step = (fading.as_millis() * FADE_STEPS as u128 / FADE.as_millis()) as u32 + 1;
        Some(step.min(FADE_STEPS))
    }

    /// Time from `now` until the next frame, for the menu's poll timeout.
    pub fn next_change_in(&self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed < HOLD {
            return HOLD - elapsed;
        }
        let step_len = FADE / FADE_STEPS;
        let fading = elapsed - HOLD;
        let done = fading.as_nanos() / step_len.as_nanos();
        let next = step_len * (done as u32 + 1);
        next.saturating_sub(fading).max(Duration::from_millis(1))
    }

    /// The title line for `now` (starts with `\r`, clears to end of line),
    /// or None once finished and the breadcrumb should be drawn instead.
    pub fn line(&self, now: Instant, cols: u16) -> Option<String> {
        let frame = self.frame(now)?;
        let bg = match self.mode {
            ColorMode::Basic => self.level.basic_bg().to_string(),
            ColorMode::TrueColor | ColorMode::Ansi256 => {
                let t = frame as f32 / FADE_STEPS as f32;
                let rgb = blend(self.level.base_rgb(), self.term_bg, t);
                if self.mode == ColorMode::TrueColor {
                    format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
                } else {
                    format!("\x1b[48;5;{}m", rgb_to_256(rgb))
                }
            }
        };
        // One column of padding on each side; the erase fills the rest of the
        // row with the background color.
        let text = crate::display::truncate_cols(&self.text, (cols as usize).saturating_sub(2));
        Some(format!("\r{bg}\x1b[97m{BOLD} {text} \x1b[K{RESET}"))
    }
}

/// Linear blend from `a` (t = 0) to `b` (t = 1).
fn blend(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Nearest color in the xterm 6x6x6 cube (indices 16..=231).
fn rgb_to_256(rgb: (u8, u8, u8)) -> u8 {
    let q = |c: u8| ((c as u16 * 5 + 127) / 255) as u8;
    16 + 36 * q(rgb.0) + 6 * q(rgb.1) + q(rgb.2)
}

/// Parse an OSC 11 reply: `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` ended by BEL or
/// `ESC \`. Components may have 1 to 4 hex digits.
fn parse_osc11(reply: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(reply).ok()?;
    let rest = &s[s.find("rgb:")? + 4..];
    let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
    let mut parts = rest[..end].split('/');
    let mut channel = || -> Option<u8> {
        let hex = parts.next()?;
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len() as u32)) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    let rgb = (channel()?, channel()?, channel()?);
    Some(rgb)
}

/// How long to wait for the terminal to answer the background query.
const OSC11_TIMEOUT: Duration = Duration::from_millis(200);

/// The terminal background, queried once per process with OSC 11 (tmux
/// answers for its pane). None when the terminal does not reply in time.
fn terminal_bg() -> Option<(u8, u8, u8)> {
    static BG: OnceLock<Option<(u8, u8, u8)>> = OnceLock::new();
    *BG.get_or_init(query_terminal_bg)
}

fn query_terminal_bg() -> Option<(u8, u8, u8)> {
    use std::os::unix::io::AsRawFd;
    let stdin_fd = std::io::stdin().as_raw_fd();
    if unsafe { libc::isatty(stdin_fd) } != 1 {
        return None;
    }
    super::common::write_stdout(b"\x1b]11;?\x1b\\");
    let deadline = Instant::now() + OSC11_TIMEOUT;
    let mut reply: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) } <= 0 {
            return None;
        }
        if nix::unistd::read(stdin_fd, &mut byte) != Ok(1) {
            return None;
        }
        reply.push(byte[0]);
        let terminated = byte[0] == 0x07 || reply.ends_with(b"\x1b\\");
        if terminated && reply.starts_with(b"\x1b]") {
            return parse_osc11(&reply);
        }
        if reply.len() > 64 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(mode: ColorMode, start: Instant) -> Notice {
        Notice::with(NoticeLevel::Error, "Failed to save".into(), start, mode, (255, 255, 255))
    }

    #[test]
    fn color_mode_detection() {
        assert_eq!(color_mode_from(Some("truecolor"), Some("xterm")), ColorMode::TrueColor);
        assert_eq!(color_mode_from(Some("24bit"), None), ColorMode::TrueColor);
        assert_eq!(color_mode_from(None, Some("tmux-256color")), ColorMode::Ansi256);
        assert_eq!(color_mode_from(Some(""), Some("xterm")), ColorMode::Basic);
        assert_eq!(color_mode_from(None, None), ColorMode::Basic);
    }

    #[test]
    fn frames_hold_then_fade_then_finish() {
        let t0 = Instant::now();
        let n = notice(ColorMode::TrueColor, t0);
        assert_eq!(n.frame(t0), Some(0));
        assert_eq!(n.frame(t0 + HOLD - Duration::from_millis(1)), Some(0));
        assert_eq!(n.frame(t0 + HOLD), Some(1));
        assert_eq!(n.frame(t0 + HOLD + FADE / 2), Some(6));
        assert_eq!(n.frame(t0 + HOLD + FADE - Duration::from_millis(1)), Some(FADE_STEPS));
        assert_eq!(n.frame(t0 + HOLD + FADE), None);
        assert!(n.line(t0 + HOLD + FADE, 80).is_none());
    }

    #[test]
    fn next_change_points_at_frame_boundaries() {
        let t0 = Instant::now();
        let n = notice(ColorMode::TrueColor, t0);
        assert_eq!(n.next_change_in(t0), HOLD);
        let step = FADE / FADE_STEPS;
        assert_eq!(n.next_change_in(t0 + HOLD), step);
        assert_eq!(n.next_change_in(t0 + HOLD + step / 2), step / 2);
    }

    #[test]
    fn truecolor_fades_from_level_color_to_terminal_bg() {
        let t0 = Instant::now();
        let n = notice(ColorMode::TrueColor, t0);
        let first = n.line(t0, 80).unwrap();
        assert!(first.contains("\x1b[48;2;176;36;36m"), "{first:?}");
        assert!(first.contains("Failed to save"));
        assert!(first.starts_with('\r') && first.ends_with(RESET));
        let last = n.line(t0 + HOLD + FADE - Duration::from_millis(1), 80).unwrap();
        assert!(last.contains("\x1b[48;2;255;255;255m"), "last frame reaches the background: {last:?}");
    }

    #[test]
    fn ansi256_and_basic_modes() {
        let t0 = Instant::now();
        let n = notice(ColorMode::Ansi256, t0);
        assert!(n.line(t0, 80).unwrap().contains(&format!("\x1b[48;5;{}m", rgb_to_256((176, 36, 36)))));
        let n = notice(ColorMode::Basic, t0);
        // No fade: the same 16-color bar until the end.
        assert!(n.line(t0, 80).unwrap().contains("\x1b[41m"));
        assert!(n.line(t0 + HOLD + FADE / 2, 80).unwrap().contains("\x1b[41m"));
    }

    #[test]
    fn long_text_is_truncated_to_width() {
        let t0 = Instant::now();
        let n = Notice::with(NoticeLevel::Info, "数".repeat(50), t0, ColorMode::Basic, (0, 0, 0));
        let line = n.line(t0, 20).unwrap();
        assert!(crate::display::display_width(&line) <= 20, "{line:?}");
        assert!(line.contains('…'));
    }

    #[test]
    fn rgb_to_256_cube() {
        assert_eq!(rgb_to_256((0, 0, 0)), 16);
        assert_eq!(rgb_to_256((255, 255, 255)), 231);
        assert_eq!(rgb_to_256((255, 0, 0)), 196);
    }

    #[test]
    fn blend_endpoints() {
        assert_eq!(blend((200, 0, 0), (0, 0, 200), 0.0), (200, 0, 0));
        assert_eq!(blend((200, 0, 0), (0, 0, 200), 1.0), (0, 0, 200));
        assert_eq!(blend((200, 0, 0), (0, 0, 200), 0.5), (100, 0, 100));
    }

    #[test]
    fn osc11_reply_parsing() {
        assert_eq!(parse_osc11(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"), Some((255, 255, 255)));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:0000/0000/0000\x07"), Some((0, 0, 0)));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:1e1e/2020/2828\x07"), Some((30, 32, 40)));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:f/8/0\x07"), Some((255, 136, 0)));
        assert_eq!(parse_osc11(b"\x1b]11;?\x07"), None);
        assert_eq!(parse_osc11(b"garbage"), None);
    }

    // Global-state behavior in one test, serialized with TEST_LOCK.
    #[test]
    fn pending_notices_and_push_routing() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_pending();
        let _ = take_unshown_pushes();
        notify(NoticeLevel::Info, "first");
        notify(NoticeLevel::Error, "line one\nline two");
        assert_eq!(take_pending(), Some((NoticeLevel::Error, "line one line two".into())));
        assert_eq!(take_pending(), None);

        // No menu open: pushes are left to the caller.
        assert!(!notify_push(NoticeLevel::Info, "deployed"));
        assert_eq!(take_pending(), None);

        {
            let _guard = MenuOpenGuard::new();
            assert!(notify_push(NoticeLevel::Info, "deployed"));
            assert_eq!(take_pending(), Some((NoticeLevel::Info, "deployed".into())));
            // A push replaced by a newer notice is not lost.
            assert!(notify_push(NoticeLevel::Error, "install failed"));
            notify(NoticeLevel::Error, "Failed to save");
            // A push still pending when the menu closes is not lost either.
            assert!(notify_push(NoticeLevel::Info, "late"));
        }
        assert!(!notify_push(NoticeLevel::Info, "after close"));
        assert_eq!(
            take_unshown_pushes(),
            vec![(NoticeLevel::Error, "install failed".into()), (NoticeLevel::Info, "late".into())]
        );
        assert!(take_unshown_pushes().is_empty());
    }
}
