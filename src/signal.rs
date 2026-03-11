use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Result, WaxError};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static CRITICAL_SECTION: AtomicBool = AtomicBool::new(false);

pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

pub fn check_cancelled() -> Result<()> {
    if is_shutdown_requested() && !is_in_critical_section() {
        Err(WaxError::Interrupted)
    } else {
        Ok(())
    }
}

pub fn enter_critical_section() {
    CRITICAL_SECTION.store(true, Ordering::SeqCst);
}

pub fn leave_critical_section() {
    CRITICAL_SECTION.store(false, Ordering::SeqCst);
}

pub fn is_in_critical_section() -> bool {
    CRITICAL_SECTION.load(Ordering::SeqCst)
}

pub struct CriticalSection;

impl CriticalSection {
    pub fn new() -> Self {
        enter_critical_section();
        CriticalSection
    }
}

impl Drop for CriticalSection {
    fn drop(&mut self) {
        leave_critical_section();
    }
}

pub fn install_handler() {
    let _ = ctrlc::set_handler(move || {
        if is_in_critical_section() {
            eprintln!("\nfinishing current operation, please wait...");
            request_shutdown();
        } else if is_shutdown_requested() {
            eprintln!("\nforce quitting");
            std::process::exit(130);
        } else {
            eprintln!("\ninterrupted, cleaning up...");
            request_shutdown();
        }
    });
}
