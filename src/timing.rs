use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SHOW_TIMING: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(enabled: bool) {
    SHOW_TIMING.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    SHOW_TIMING.load(Ordering::Relaxed)
}

pub fn elapsed_text(elapsed: Duration) -> String {
    format!("[{}ms]", elapsed.as_millis())
}

pub fn elapsed_suffix(elapsed: Duration) -> String {
    if enabled() {
        format!(" {}", elapsed_text(elapsed))
    } else {
        String::new()
    }
}
