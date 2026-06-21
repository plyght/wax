use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SHOW_TIMING: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(enabled: bool) {
    SHOW_TIMING.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    SHOW_TIMING.load(Ordering::Relaxed)
}

pub fn elapsed_suffix(elapsed: Duration) -> String {
    if enabled() {
        format!(" [{}ms]", elapsed.as_millis())
    } else {
        String::new()
    }
}
