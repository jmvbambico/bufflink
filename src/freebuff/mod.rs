//! `src/freebuff/` — everything that knows what freebuff's TUI looks like
//! (prompt detection, output extraction, approval prompts). Isolate the
//! fragile knowledge here so a TUI change is a one-module fix.

pub mod chats;
pub mod log;
pub mod screen;
pub mod transcript;

/// Command to exit freebuff cleanly.
pub const EXIT_COMMAND: &str = "/exit";

/// Key to cancel generation (Escape).
pub const CANCEL_KEY: &str = "\x1b";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_exist() {
        assert_eq!(EXIT_COMMAND, "/exit");
        assert_eq!(CANCEL_KEY, "\x1b");
    }
}
