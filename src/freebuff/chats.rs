//! Helpers for locating freebuff chat directories and files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Get the manicode config directory.
/// Uses `$BLINK_MANICODE_DIR` if set, else `$HOME/.config/manicode`.
pub fn manicode_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BLINK_MANICODE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("manicode")
}

/// Get the chats directory for a given working directory.
/// Path: `<manicode_dir>/projects/<cwd file_name>/chats`
pub fn chats_dir(cwd: &Path) -> PathBuf {
    let project_name = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    manicode_dir()
        .join("projects")
        .join(project_name)
        .join("chats")
}

/// Snapshot the set of entry names in a directory.
/// Returns an empty set if the directory doesn't exist.
pub fn snapshot(dir: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                set.insert(name.to_string());
            }
        }
    }
    Ok(set)
}

/// Find the newest chat directory not in `before`.
/// Returns the path to the newest entry (lexicographically greatest) not in `before`.
pub fn newest_new_chat(dir: &Path, before: &BTreeSet<String>) -> std::io::Result<Option<PathBuf>> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    // Sort lexicographically (ISO timestamps sort correctly)
    entries.sort();

    // Find the greatest entry not in `before`
    for name in entries.into_iter().rev() {
        if !before.contains(&name) {
            return Ok(Some(dir.join(name)));
        }
    }
    Ok(None)
}

/// Filename constants for chat artifacts.
pub const CHAT_MESSAGES: &str = "chat-messages.json";
pub const LOG: &str = "log.jsonl";

/// Parse the freebuff instance owner file.
/// Returns (instance_id, pid) if the file exists and is valid.
pub fn instance_owner(manicode: &Path) -> Option<(String, u32)> {
    let path = manicode.join("freebuff-instance-owner.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;

    let instance_id = value.get("instanceId")?.as_str()?.to_string();
    let pid = value.get("pid")?.as_u64()? as u32;

    Some((instance_id, pid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn temp_dir() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("bufflink-test-{}-{}", std::process::id(), id))
    }

    #[test]
    fn manicode_dir_from_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let original = std::env::var("BLINK_MANICODE_DIR").ok();
        std::env::set_var("BLINK_MANICODE_DIR", "/custom/path");
        assert_eq!(manicode_dir(), PathBuf::from("/custom/path"));
        match original {
            Some(v) => std::env::set_var("BLINK_MANICODE_DIR", v),
            None => std::env::remove_var("BLINK_MANICODE_DIR"),
        }
    }

    #[test]
    fn manicode_dir_default() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let original = std::env::var("BLINK_MANICODE_DIR").ok();
        std::env::remove_var("BLINK_MANICODE_DIR");
        let home = std::env::var("HOME").unwrap();
        let expected = PathBuf::from(home).join(".config").join("manicode");
        assert_eq!(manicode_dir(), expected);
        match original {
            Some(v) => std::env::set_var("BLINK_MANICODE_DIR", v),
            None => std::env::remove_var("BLINK_MANICODE_DIR"),
        }
    }

    #[test]
    fn chats_dir_constructs_path() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let original = std::env::var("BLINK_MANICODE_DIR").ok();
        std::env::remove_var("BLINK_MANICODE_DIR");
        let cwd = PathBuf::from("/home/user/my-project");
        let home = std::env::var("HOME").unwrap();
        let expected = PathBuf::from(home)
            .join(".config")
            .join("manicode")
            .join("projects")
            .join("my-project")
            .join("chats");
        assert_eq!(chats_dir(&cwd), expected);
        match original {
            Some(v) => std::env::set_var("BLINK_MANICODE_DIR", v),
            None => std::env::remove_var("BLINK_MANICODE_DIR"),
        }
    }

    #[test]
    fn snapshot_empty_for_missing_dir() {
        let dir = temp_dir().join("nonexistent");
        let snap = snapshot(&dir).unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn snapshot_lists_entries() {
        let base = temp_dir();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        fs::write(base.join("2026-01-01T00-00-00.000Z"), "").unwrap();
        fs::write(base.join("2026-01-02T00-00-00.000Z"), "").unwrap();
        fs::write(base.join("2026-01-03T00-00-00.000Z"), "").unwrap();

        let snap = snapshot(&base).unwrap();
        assert_eq!(snap.len(), 3);
        assert!(snap.contains("2026-01-01T00-00-00.000Z"));
        assert!(snap.contains("2026-01-02T00-00-00.000Z"));
        assert!(snap.contains("2026-01-03T00-00-00.000Z"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn newest_new_chat_finds_newest_not_in_before() {
        let base = temp_dir();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        fs::write(base.join("2026-01-01T00-00-00.000Z"), "").unwrap();
        fs::write(base.join("2026-01-02T00-00-00.000Z"), "").unwrap();
        fs::write(base.join("2026-01-03T00-00-00.000Z"), "").unwrap();

        let before: BTreeSet<String> = ["2026-01-01T00-00-00.000Z".to_string()].into();
        let newest = newest_new_chat(&base, &before).unwrap();
        assert_eq!(newest, Some(base.join("2026-01-03T00-00-00.000Z")));

        let before2: BTreeSet<String> = [
            "2026-01-01T00-00-00.000Z".to_string(),
            "2026-01-02T00-00-00.000Z".to_string(),
            "2026-01-03T00-00-00.000Z".to_string(),
        ]
        .into();
        let newest2 = newest_new_chat(&base, &before2).unwrap();
        assert_eq!(newest2, None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn instance_owner_parses_valid_file() {
        let base = temp_dir();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let owner_file = base.join("freebuff-instance-owner.json");
        fs::write(&owner_file, r#"{"instanceId": "abc-123", "pid": 12345}"#).unwrap();

        let result = instance_owner(&base);
        assert_eq!(result, Some(("abc-123".to_string(), 12345)));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn instance_owner_returns_none_for_missing_file() {
        let base = temp_dir();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let result = instance_owner(&base);
        assert_eq!(result, None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn instance_owner_returns_none_for_invalid_json() {
        let base = temp_dir();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let owner_file = base.join("freebuff-instance-owner.json");
        fs::write(&owner_file, "not json").unwrap();

        let result = instance_owner(&base);
        assert_eq!(result, None);

        let _ = fs::remove_dir_all(&base);
    }
}
