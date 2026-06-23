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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TIMING_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_set_and_get_enabled() {
        let _guard = TIMING_LOCK.lock().unwrap();

        set_enabled(true);
        assert!(enabled());

        set_enabled(false);
        assert!(!enabled());
    }

    #[test]
    fn test_elapsed_suffix_when_enabled() {
        let _guard = TIMING_LOCK.lock().unwrap();

        set_enabled(true);
        let duration = Duration::from_millis(1500);
        assert_eq!(elapsed_suffix(duration), " [1500ms]");
    }

    #[test]
    fn test_elapsed_suffix_when_disabled() {
        let _guard = TIMING_LOCK.lock().unwrap();

        set_enabled(false);
        let duration = Duration::from_millis(1500);
        assert_eq!(elapsed_suffix(duration), "");
    }
}
