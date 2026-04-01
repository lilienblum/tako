use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::ProgressBar;

use super::PHASE_PB;

/// When true, the terminal cursor stays hidden even when `show_cursor()` is
/// called. Only explicit `restore_cursor()` (process exit) or interactive
/// prompts (which use crossterm directly) will show the cursor.
static CURSOR_GLOBALLY_HIDDEN: AtomicBool = AtomicBool::new(false);

/// When true, pretty interactive mode keeps terminal control characters
/// (notably Ctrl-C) from being echoed as `^C` while preserving normal input
/// echo for prompts.
pub(super) static CONTROL_ECHO_GLOBALLY_SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Currently visible progress bar, used to clear spinner state on process-wide Ctrl-C.
pub(super) static ACTIVE_PROGRESS_BAR: Mutex<Option<ProgressBar>> = Mutex::new(None);

/// Hide the cursor for the entire process lifetime. Individual text-input
/// prompts temporarily show it via crossterm; all other code paths keep it
/// hidden. Call `restore_cursor()` on exit to bring it back.
pub fn set_cursor_globally_hidden() {
    CURSOR_GLOBALLY_HIDDEN.store(true, Ordering::Relaxed);
    CONTROL_ECHO_GLOBALLY_SUPPRESSED.store(true, Ordering::Relaxed);
    apply_termios_echo_mode(false, true);
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Hide);
}

/// Check whether the cursor is currently globally hidden.
pub fn is_cursor_globally_hidden() -> bool {
    CURSOR_GLOBALLY_HIDDEN.load(Ordering::Relaxed)
}

/// Restore the cursor on process exit / interrupt. Only emits the escape
/// sequence when the cursor was actually globally hidden.
pub fn restore_cursor() {
    if CONTROL_ECHO_GLOBALLY_SUPPRESSED.swap(false, Ordering::Relaxed) {
        apply_termios_echo_mode(false, false);
    }
    if CURSOR_GLOBALLY_HIDDEN.load(Ordering::Relaxed) {
        CURSOR_GLOBALLY_HIDDEN.store(false, Ordering::Relaxed);
        let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);
    }
}

/// Hide cursor and suppress keyboard echo while keeping signal handling
/// (Ctrl+C etc.) intact. Call `show_cursor()` to restore.
pub fn hide_cursor() {
    suppress_echo(true);
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Hide);
}

/// Restore keyboard echo and show the cursor — unless the cursor is globally
/// hidden for this process (pretty interactive mode).
pub fn show_cursor() {
    if !CURSOR_GLOBALLY_HIDDEN.load(Ordering::Relaxed) {
        let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);
    }
    suppress_echo(false);
}

pub(super) fn register_active_progress_bar(pb: &ProgressBar) {
    *ACTIVE_PROGRESS_BAR.lock().unwrap() = Some(pb.clone());
}

pub(super) fn clear_active_progress_bar() {
    ACTIVE_PROGRESS_BAR.lock().unwrap().take();
}

/// Toggle the terminal ECHO flag without touching ISIG, so Ctrl+C still
/// generates SIGINT.
pub fn suppress_echo(suppress: bool) {
    apply_termios_echo_mode(
        suppress,
        CONTROL_ECHO_GLOBALLY_SUPPRESSED.load(Ordering::Relaxed),
    );
}

#[cfg(unix)]
fn set_termios_echo_flags(
    termios: &mut libc::termios,
    suppress_keyboard_echo: bool,
    suppress_control_echo: bool,
) {
    if suppress_keyboard_echo {
        termios.c_lflag &= !(libc::ECHO | libc::ECHOCTL | libc::ICANON);
    } else {
        termios.c_lflag |= libc::ECHO | libc::ICANON;
        if suppress_control_echo {
            termios.c_lflag &= !libc::ECHOCTL;
        } else {
            termios.c_lflag |= libc::ECHOCTL;
        }
    }
}

fn apply_termios_echo_mode(suppress_keyboard_echo: bool, suppress_control_echo: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = std::io::stdin().as_raw_fd();
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut termios) != 0 {
                return;
            }
            set_termios_echo_flags(&mut termios, suppress_keyboard_echo, suppress_control_echo);
            let _ = libc::tcsetattr(fd, libc::TCSANOW, &termios);
        }
    }
}

/// Clears any live spinner or progress bar state before printing interrupt
/// output, so cancellation renders on a clean line.
pub fn clear_interrupt_output() {
    let active = ACTIVE_PROGRESS_BAR.lock().unwrap().take();
    let phase = PHASE_PB.lock().unwrap().take();

    if let Some(pb) = active {
        pb.finish_and_clear();
    }
    if let Some(pb) = phase {
        pb.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn set_termios_echo_flags_restores_input_echo_without_control_echo() {
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        set_termios_echo_flags(&mut termios, false, true);
        assert_ne!(termios.c_lflag & libc::ECHO, 0);
        assert_ne!(termios.c_lflag & libc::ICANON, 0);
        assert_eq!(termios.c_lflag & libc::ECHOCTL, 0);
    }

    #[cfg(unix)]
    #[test]
    fn set_termios_echo_flags_restores_control_echo_when_not_suppressed() {
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        set_termios_echo_flags(&mut termios, false, false);
        assert_ne!(termios.c_lflag & libc::ECHO, 0);
        assert_ne!(termios.c_lflag & libc::ICANON, 0);
        assert_ne!(termios.c_lflag & libc::ECHOCTL, 0);
    }

    #[cfg(unix)]
    #[test]
    fn set_termios_echo_flags_suppresses_all_echo_while_cursor_is_hidden() {
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        termios.c_lflag = libc::ECHO | libc::ECHOCTL | libc::ICANON;
        set_termios_echo_flags(&mut termios, true, true);
        assert_eq!(termios.c_lflag & libc::ECHO, 0);
        assert_eq!(termios.c_lflag & libc::ICANON, 0);
        assert_eq!(termios.c_lflag & libc::ECHOCTL, 0);
    }

    #[test]
    fn clear_interrupt_output_clears_registered_progress_state() {
        let pb = ProgressBar::new_spinner();
        register_active_progress_bar(&pb);
        *PHASE_PB.lock().unwrap() = Some(pb);
        clear_interrupt_output();
        assert!(ACTIVE_PROGRESS_BAR.lock().unwrap().is_none());
        assert!(PHASE_PB.lock().unwrap().is_none());
    }
}
