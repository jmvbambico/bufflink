//! `src/freebuff/settings.rs` — pin the active model in freebuff's
//! `settings.json`.
//!
//! freebuff 0.2.x reads the active model from the `freebuffModel` key (and the
//! picker-row key from `freebuffModelKey`) in `~/.config/manicode/settings.json`
//! at startup and on catalog refresh; there is no boot model picker to
//! arrow-walk. To honor `BLINK_MODEL`, bufflink must therefore write those keys
//! *before* the freebuff process starts, and put the user's originals back when
//! it is done so an interactive freebuff session is unaffected.
//!
//! Rules of the road (see AGENTS.md): never touch state we did not create. We
//! read-modify-write a `serde_json::Value` object so keys bufflink does not own
//! survive untouched, copy the original file to
//! `settings.json.blink-backup` before the first write of each run (refreshed,
//! so it always reflects the value actually being replaced), and never create a
//! fresh settings file that would discard user state — a missing or unparseable
//! file is a loud error, not something to paper over.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

/// File name of freebuff's settings, relative to the manicode directory.
pub const SETTINGS_FILE: &str = "settings.json";
/// Suffix of the one-time backup taken before bufflink first rewrites settings.
pub const BACKUP_SUFFIX: &str = ".blink-backup";
/// Settings key holding the active model id.
const SETTINGS_MODEL: &str = "freebuffModel";
/// Settings key holding the picker-row key for the active model.
const SETTINGS_MODEL_KEY: &str = "freebuffModelKey";

/// Path to freebuff's settings file inside `manicode_dir`.
pub fn settings_path(manicode_dir: &Path) -> PathBuf {
    manicode_dir.join(SETTINGS_FILE)
}

/// Path of the one-time backup of the original settings file.
pub fn backup_path(settings_path: &Path) -> PathBuf {
    let mut s = settings_path.as_os_str().to_os_string();
    s.push(BACKUP_SUFFIX);
    PathBuf::from(s)
}

/// Validate a `BLINK_MODEL` value before writing it to settings, so a typo or
/// shell mishap never reaches freebuff's config. Accepts a provider-prefixed id
/// (`z-ai/glm-5.3-flash`), a bare id (`glm-5.3-flash`), or a picker key
/// (`m-79293c5b7d`): lowercase ASCII plus digits, with `.`, `_`, `-`, `/`
/// separators, starting alphanumeric, 2..=128 bytes. Deliberately syntactic —
/// catalog membership is freebuff's call, and an unknown-but-plausible id must
/// still be written so the running model can be checked against it later.
pub fn validate_model_id(id: &str) -> Result<()> {
    let bytes = id.as_bytes();
    if bytes.len() < 2 || bytes.len() > 128 {
        bail!("model id must be 2..=128 bytes, got {}", bytes.len());
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        bail!("model id must start with a lowercase letter or digit");
    }
    if !bytes.iter().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-' | b'/')
    }) {
        bail!("model id may only contain lowercase letters, digits and . _ - /");
    }
    Ok(())
}

/// Validate the picker-row key form freebuff itself writes:
/// `/^[a-z0-9][a-z0-9-]{1,63}$/`. Used only when bufflink supplies a key.
fn validate_model_key(key: &str) -> Result<()> {
    let bytes = key.as_bytes();
    if bytes.len() < 2 || bytes.len() > 64 {
        bail!("model key must be 2..=64 bytes, got {}", bytes.len());
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        bail!("model key must start with a lowercase letter or digit");
    }
    if !bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
    {
        bail!("model key may only contain lowercase letters, digits and -");
    }
    Ok(())
}

/// A pin on freebuff's `settings.json` that remembers the user's original
/// `freebuffModel` / `freebuffModelKey` and restores them on drop-in-time.
pub struct SettingsPin {
    path: PathBuf,
    backup_path: PathBuf,
    original_model: Option<Value>,
    original_key: Option<Value>,
    applied: bool,
}

impl SettingsPin {
    /// Create a pin targeting `settings_path`. No I/O until [`pin`] is called.
    pub fn new(settings_path: PathBuf) -> Self {
        let backup_path = backup_path(&settings_path);
        Self {
            path: settings_path,
            backup_path,
            original_model: None,
            original_key: None,
            applied: false,
        }
    }

    /// Path of the settings file this pin manages.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path of the last-resort backup holding the user's pre-pin values. Named
    /// in every error so a failed restore tells the user exactly which file
    /// holds their original model.
    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    /// Whether a pin is currently applied.
    pub fn is_applied(&self) -> bool {
        self.applied
    }

    /// Write `freebuffModel = model_id` (and `freebuffModelKey` when given, else
    /// remove any stale key so it cannot override the requested model on a
    /// catalog refresh). Refreshes the backup to the value being replaced on the
    /// first pin of a run, then read-modify-writes preserving every other key.
    /// Fails loudly if the file is missing or unparseable — never creates a
    /// fresh one.
    pub fn pin(&mut self, model_id: &str, model_key: Option<&str>) -> Result<()> {
        validate_model_id(model_id).with_context(|| format!("invalid BLINK_MODEL '{model_id}'"))?;
        if let Some(key) = model_key {
            validate_model_key(key).with_context(|| format!("invalid freebuffModelKey '{key}'"))?;
        }

        let raw = fs::read(&self.path).with_context(|| {
            format!(
                "freebuff settings not found at {}; run freebuff once to create it before pinning BLINK_MODEL",
                self.path.display()
            )
        })?;
        let parsed: Value = serde_json::from_slice(&raw).with_context(|| {
            format!(
                "freebuff settings at {} is not valid JSON",
                self.path.display()
            )
        })?;
        let mut obj = match parsed {
            Value::Object(map) => map,
            _ => bail!(
                "freebuff settings at {} is not a JSON object",
                self.path.display()
            ),
        };

        if !self.applied {
            // Refresh the backup to the value actually being replaced right now.
            // Writing it only when ABSENT left a STALE backup: a previous run's
            // copy (model A) survived, the user later selected model B, a restore
            // failed and the process died before any retry — B lived only in
            // memory while the durable copy still said A. Re-writing it on every
            // capture keeps the on-disk artefact correct even if we never get to
            // restore. (Within a run, only the first pin captures: `applied`
            // guards it so a later pin does not record our own pinned value.)
            atomic_write(&self.backup_path, &raw).with_context(|| {
                format!(
                    "failed to back up freebuff settings to {}",
                    self.backup_path.display()
                )
            })?;
            self.original_model = obj.get(SETTINGS_MODEL).cloned();
            self.original_key = obj.get(SETTINGS_MODEL_KEY).cloned();
        }

        obj.insert(
            SETTINGS_MODEL.to_string(),
            Value::String(model_id.to_string()),
        );
        match model_key {
            Some(key) => {
                obj.insert(
                    SETTINGS_MODEL_KEY.to_string(),
                    Value::String(key.to_string()),
                );
            }
            None => {
                obj.remove(SETTINGS_MODEL_KEY);
            }
        }

        let out = serde_json::to_vec_pretty(&Value::Object(obj))
            .context("failed to serialize pinned freebuff settings")?;
        atomic_write(&self.path, &out).with_context(|| {
            format!(
                "failed to write pinned freebuff settings to {}",
                self.path.display()
            )
        })?;
        self.applied = true;
        Ok(())
    }

    /// Restore the user's original `freebuffModel` / `freebuffModelKey`, leaving
    /// everything else — including keys freebuff changed mid-session — intact.
    /// A no-op when nothing was pinned or the file has since vanished.
    ///
    /// On failure the pin is RETAINED (`applied` stays true, the original values
    /// are kept) so a later shutdown path can retry; a non-object or unreadable
    /// file is an error, never a silent success. Every error names the
    /// `settings.json.blink-backup` last-resort copy.
    pub fn restore(&mut self) -> Result<()> {
        if !self.applied {
            return Ok(());
        }
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.applied = false;
                return Ok(());
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "failed to read freebuff settings at {} during restore: the user's original model was NOT restored (backup at {})",
                        self.path.display(),
                        self.backup_path.display()
                    )
                })
            }
        };
        let mut obj = match serde_json::from_slice::<Value>(&raw) {
            Ok(Value::Object(map)) => map,
            // Valid JSON but not an object: the original model was NOT restored.
            // This is an error, never a silent success — and the pin is kept
            // (`applied` stays true) so a later shutdown can retry.
            Ok(_) => bail!(
                "freebuff settings at {} is not a JSON object during restore: the user's original model was NOT restored. \
                 Retry at shutdown, or restore it manually from the backup at {}",
                self.path.display(),
                self.backup_path.display()
            ),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "freebuff settings at {} is not valid JSON during restore: the user's original model was NOT restored (backup at {})",
                        self.path.display(),
                        self.backup_path.display()
                    )
                })
            }
        };

        restore_field(&mut obj, SETTINGS_MODEL, self.original_model.clone());
        restore_field(&mut obj, SETTINGS_MODEL_KEY, self.original_key.clone());

        let out = serde_json::to_vec_pretty(&Value::Object(obj))
            .context("failed to serialize restored freebuff settings")?;
        atomic_write(&self.path, &out).with_context(|| {
            format!(
                "failed to restore freebuff settings to {}: the user's original model was NOT restored (backup at {})",
                self.path.display(),
                self.backup_path.display()
            )
        })?;
        self.applied = false;
        Ok(())
    }
}

fn restore_field(obj: &mut Map<String, Value>, key: &str, original: Option<Value>) {
    match original {
        Some(value) => {
            obj.insert(key.to_string(), value);
        }
        None => {
            obj.remove(key);
        }
    }
}

/// Write `bytes` to `path` atomically: a sibling temp file, fsync, then rename
/// over the target. A failed write leaves the original file untouched.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| SETTINGS_FILE.to_string());
    let tmp = dir.join(format!(".{file_name}.blink-tmp-{}", std::process::id()));

    let attempt = || -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    };
    if let Err(e) = attempt() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// RAII temp directory, mirroring the driver's test helper (no tempfile dep).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "bufflink-settings-test-{}-{}",
                std::process::id(),
                unique
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_settings(initial: Option<Value>) -> (TempDir, PathBuf) {
        let dir = TempDir::new();
        let path = settings_path(dir.path());
        if let Some(value) = initial {
            fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        }
        (dir, path)
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn validate_model_id_accepts_ids_and_rejects_junk() {
        for ok in [
            "z-ai/glm-5.3-flash",
            "deepseek/deepseek-v4-flash-fast",
            "mimo/mimo-v2.5",
            "glm-5.3-flash",
            "m-79293c5b7d",
            "ab",
        ] {
            assert!(validate_model_id(ok).is_ok(), "should accept {ok}");
        }
        for bad in ["", "a", "GLM 5.3 Flash", "GLM-5.3", "-glm", "glm 5", ".glm"] {
            assert!(validate_model_id(bad).is_err(), "should reject {bad:?}");
        }
        assert!(validate_model_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn pin_writes_model_preserves_other_keys_and_clears_stale_key() {
        let (_dir, path) = temp_settings(Some(json!({
            "mode": "DEFAULT",
            "adsEnabled": true,
            "freebuffModel": "z-ai/glm-5.3-flash",
            "freebuffModelKey": "m-00032eaeec",
            "futureKey": {"nested": [1, 2, 3]}
        })));

        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", None).unwrap();

        let written = read_json(&path);
        assert_eq!(written["freebuffModel"], "mimo/mimo-v2.5");
        assert!(written.get("freebuffModelKey").is_none());
        assert_eq!(written["mode"], "DEFAULT");
        assert_eq!(written["adsEnabled"], true);
        assert_eq!(written["futureKey"], json!({"nested": [1, 2, 3]}));
        assert!(pin.is_applied());
    }

    #[test]
    fn pin_writes_key_when_provided() {
        let (_dir, path) = temp_settings(Some(json!({ "freebuffModel": "old/model" })));
        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", Some("m-00032eaeec")).unwrap();
        let written = read_json(&path);
        assert_eq!(written["freebuffModel"], "mimo/mimo-v2.5");
        assert_eq!(written["freebuffModelKey"], "m-00032eaeec");
    }

    #[test]
    fn pin_backs_up_original_once() {
        let original = json!({
            "freebuffModel": "z-ai/glm-5.3-flash",
            "freebuffModelKey": "m-00032eaeec",
            "mode": "DEFAULT"
        });
        let (_dir, path) = temp_settings(Some(original.clone()));
        let backup = backup_path(&path);

        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", None).unwrap();
        assert_eq!(read_json(&backup), original);

        // A second pin must not clobber the one-time backup.
        let mut aside = original.clone();
        aside["freebuffModel"] = json!("changed/later");
        fs::write(&backup, serde_json::to_vec_pretty(&aside).unwrap()).unwrap();
        pin.pin("deepseek/deepseek-v4-pro", None).unwrap();
        assert_eq!(read_json(&backup), aside);
    }

    /// The backup is REFRESHED on the first pin of each run so it always
    /// reflects the value actually being replaced. A stale backup left by an
    /// earlier run would otherwise hold an older model while the user's current
    /// choice lived only in memory — so a restore failure plus an immediate
    /// process exit would lose it.
    #[test]
    fn backup_is_refreshed_to_the_value_being_replaced() {
        let (_dir, path) = temp_settings(Some(json!({
            "freebuffModel": "mimo/mimo-v2.5",
            "mode": "DEFAULT"
        })));
        let backup = backup_path(&path);

        // A previous run left a STALE backup describing an older model.
        fs::write(
            &backup,
            serde_json::to_vec_pretty(&json!({ "freebuffModel": "old/model" })).unwrap(),
        )
        .unwrap();

        // A fresh run replaces the user's CURRENT model...
        let mut pin = SettingsPin::new(path.clone());
        pin.pin("z-ai/glm-5.3-flash", None).unwrap();
        // ...and the durable backup now describes exactly what this run replaced.
        assert_eq!(read_json(&backup)["freebuffModel"], "mimo/mimo-v2.5");
    }

    #[test]
    fn restore_puts_originals_back_and_keeps_mid_session_changes() {
        let (_dir, path) = temp_settings(Some(json!({
            "freebuffModel": "z-ai/glm-5.3-flash",
            "freebuffModelKey": "m-00032eaeec",
            "mode": "DEFAULT"
        })));

        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", None).unwrap();

        // Simulate freebuff rewriting settings mid-session.
        let mut mid = read_json(&path);
        mid["mode"] = json!("AGENT");
        mid["hasSubmittedFirstPrompt"] = json!(true);
        fs::write(&path, serde_json::to_vec_pretty(&mid).unwrap()).unwrap();

        pin.restore().unwrap();
        let restored = read_json(&path);
        assert_eq!(restored["freebuffModel"], "z-ai/glm-5.3-flash");
        assert_eq!(restored["freebuffModelKey"], "m-00032eaeec");
        assert_eq!(restored["mode"], "AGENT");
        assert_eq!(restored["hasSubmittedFirstPrompt"], true);
        assert!(!pin.is_applied());
    }

    #[test]
    fn restore_removes_keys_absent_originally() {
        let (_dir, path) = temp_settings(Some(json!({ "mode": "DEFAULT" })));
        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", Some("m-00032eaeec")).unwrap();
        pin.restore().unwrap();
        let restored = read_json(&path);
        assert!(restored.get("freebuffModel").is_none());
        assert!(restored.get("freebuffModelKey").is_none());
        assert_eq!(restored["mode"], "DEFAULT");
    }

    #[test]
    fn pin_missing_settings_is_a_loud_error() {
        let dir = TempDir::new();
        let path = settings_path(dir.path());
        let mut pin = SettingsPin::new(path.clone());
        let err = pin.pin("mimo/mimo-v2.5", None).unwrap_err();
        assert!(format!("{err:#}").contains("settings not found"));
        assert!(!pin.is_applied());
        assert!(!path.exists(), "must not create a fresh settings file");
    }

    #[test]
    fn pin_unparseable_settings_is_a_loud_error() {
        let dir = TempDir::new();
        let path = settings_path(dir.path());
        fs::write(&path, b"{ not json").unwrap();
        let mut pin = SettingsPin::new(path.clone());
        let err = pin.pin("mimo/mimo-v2.5", None).unwrap_err();
        assert!(format!("{err:#}").contains("not valid JSON"));
        assert!(!pin.is_applied());
        assert_eq!(fs::read(&path).unwrap(), b"{ not json");
    }

    #[test]
    fn pin_non_object_settings_is_a_loud_error() {
        let (_dir, path) = temp_settings(Some(json!([1, 2, 3])));
        let mut pin = SettingsPin::new(path.clone());
        let err = pin.pin("mimo/mimo-v2.5", None).unwrap_err();
        assert!(format!("{err:#}").contains("not a JSON object"));
    }

    /// A valid-JSON-but-non-object settings file must be an ERROR (never a
    /// silent success), and the pin — the only holder of the user's original
    /// model — must survive for a later retry.
    #[test]
    fn restore_non_object_settings_is_an_error_and_keeps_the_pin() {
        let (_dir, path) = temp_settings(Some(json!({
            "freebuffModel": "orig/model",
            "freebuffModelKey": "m-orig",
            "mode": "DEFAULT"
        })));
        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", None).unwrap();
        assert!(pin.is_applied());

        fs::write(&path, b"[1,2,3]").unwrap();
        let err = pin.restore().unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not a JSON object"), "unexpected: {msg}");
        assert!(msg.contains("blink-backup"), "must name the backup: {msg}");
        // Retained, not discarded: a later shutdown can retry.
        assert!(pin.is_applied());

        // Once the file is an object again, the retry restores the original.
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({ "mode": "DEFAULT" })).unwrap(),
        )
        .unwrap();
        pin.restore().unwrap();
        let restored = read_json(&path);
        assert_eq!(restored["freebuffModel"], "orig/model");
        assert_eq!(restored["freebuffModelKey"], "m-orig");
        assert!(!pin.is_applied());
    }

    #[test]
    fn restore_before_pin_is_a_noop() {
        let (_dir, path) = temp_settings(Some(json!({ "freebuffModel": "keep/me" })));
        let mut pin = SettingsPin::new(path.clone());
        pin.restore().unwrap();
        assert_eq!(read_json(&path)["freebuffModel"], "keep/me");
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let (_dir, path) = temp_settings(Some(json!({ "freebuffModel": "x/y" })));
        let mut pin = SettingsPin::new(path.clone());
        pin.pin("mimo/mimo-v2.5", None).unwrap();
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("blink-tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }
}
