//! The panic policy every PSoXide guest build shares.
//!
//! A shipping guest has nowhere to print a panic message, so it is built with
//! `-Cpanic=immediate-abort`: every panic, bounds check and overflow check is a
//! bare `break` at the faulting instruction. That keeps `core::fmt`, the
//! panic `Location` records and their file names out of the image (about
//! 12.8 KB on Quake, 26.6 KB on the 2026-09-23 Half-Life stack). psx-rt's
//! exception handler records Cause, EPC and BadVAddr (see `fault`), so a
//! fault is still located. Set `PSOXIDE_PANIC_MESSAGES=1` to build with the
//! formatted panic instead, to read a message while chasing a fault.

/// Environment variable that restores the formatted panic.
pub const MESSAGES_ENV: &str = "PSOXIDE_PANIC_MESSAGES";

/// True when the value asks for messages: set, not empty, not `0`.
pub fn wants_messages(value: Option<&str>) -> bool {
    matches!(value, Some(v) if !v.is_empty() && v != "0")
}

/// The rustflags for the chosen policy. Empty when messages are wanted.
pub fn rustflags(messages: bool) -> &'static [&'static str] {
    if messages {
        &[]
    } else {
        &["-Zunstable-options", "-Cpanic=immediate-abort"]
    }
}

/// The policy from this process's environment.
pub fn rustflags_from_env() -> &'static [&'static str] {
    rustflags(wants_messages(std::env::var(MESSAGES_ENV).ok().as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_is_the_default_and_messages_restore_the_panic() {
        assert_eq!(
            rustflags(false),
            ["-Zunstable-options", "-Cpanic=immediate-abort"]
        );
        assert!(rustflags(true).is_empty());
        assert!(!wants_messages(None));
        assert!(!wants_messages(Some("")));
        assert!(!wants_messages(Some("0")));
        assert!(wants_messages(Some("1")));
    }
}
