use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{Result, WaxError};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static CRITICAL_SECTION: AtomicBool = AtomicBool::new(false);

static CURRENT_OP: OnceLock<Mutex<String>> = OnceLock::new();

fn current_op_mutex() -> &'static Mutex<String> {
    CURRENT_OP.get_or_init(|| Mutex::new(String::new()))
}

pub fn set_current_op(op: impl Into<String>) {
    if let Ok(mut guard) = current_op_mutex().lock() {
        *guard = op.into();
    }
}

pub fn clear_current_op() {
    if let Ok(mut guard) = current_op_mutex().lock() {
        guard.clear();
    }
}

fn get_current_op() -> String {
    current_op_mutex()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

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
        let op = get_current_op();
        if is_in_critical_section() {
            if op.is_empty() {
                eprintln!("\nfinishing current operation, please wait...");
            } else {
                eprintln!("\nfinishing {} — do not interrupt, cleaning up when done...", op);
            }
            request_shutdown();
        } else if is_shutdown_requested() {
            eprintln!("\nforce quitting");
            std::process::exit(130);
        } else {
            if op.is_empty() {
                eprintln!("\ninterrupted, cleaning up temp files...");
            } else {
                eprintln!("\ninterrupted while {} — cleaning up temp files...", op);
            }
            request_shutdown();
        }
    });
}
