use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::model::{API_KEY_SCOPE, AuthMode, AuthStore, GrokAuth, lookup_auth};

const PROVIDER_STORE_VERSION: u8 = 2;

/// On-disk provider credential store.  The legacy scope map is retained as
/// an implementation detail during the v2 transition so custom OAuth scopes
/// cannot be lost, while the canonical xAI credential always lives at
/// `providers.xai.credentials.xai`.
#[derive(serde::Serialize, serde::Deserialize)]
struct ProviderAuthStore {
    version: u8,
    providers: ProviderEntries,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProviderEntries {
    /// xAI is optional so a v2 store containing only a third-party provider
    /// remains readable by legacy xAI callers (as an empty xAI scope map).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    xai: Option<XaiProvider>,
    /// Provider namespaces unknown to the legacy xAI reader.  Flattening is
    /// lossless: AuthManager writes must never erase OpenAI/OpenRouter keys.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct XaiProvider {
    credentials: XaiCredentials,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct XaiCredentials {
    xai: GrokAuth,
    /// Keeps current callers' scope-based contract lossless until they move
    /// to provider credentials directly.  `xai` remains authoritative for
    /// a newly written single-credential store.
    #[serde(default, skip_serializing_if = "AuthStore::is_empty")]
    scopes: AuthStore,
}

impl ProviderAuthStore {
    fn from_legacy(scopes: AuthStore) -> std::io::Result<Self> {
        let credential = scopes
            .get(super::model::LEGACY_SCOPE)
            .or_else(|| scopes.get(API_KEY_SCOPE))
            .or_else(|| scopes.values().next())
            .cloned()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty auth store")
            })?;
        Ok(Self {
            version: PROVIDER_STORE_VERSION,
            providers: ProviderEntries {
                xai: Some(XaiProvider {
                    credentials: XaiCredentials {
                        xai: credential,
                        scopes,
                    },
                }),
                other: serde_json::Map::new(),
            },
        })
    }

    fn into_legacy(self) -> std::io::Result<AuthStore> {
        if self.version != PROVIDER_STORE_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported auth store version",
            ));
        }
        let Some(xai) = self.providers.xai else {
            return Ok(AuthStore::new());
        };
        let credentials = xai.credentials;
        let scope = if credentials.xai.auth_mode == AuthMode::ApiKey {
            API_KEY_SCOPE
        } else {
            super::model::LEGACY_SCOPE
        };
        // The canonical v2 value wins over a stale compatibility copy.
        let mut scopes = credentials.scopes;
        scopes.insert(scope.to_owned(), credentials.xai);
        Ok(scopes)
    }
}

/// Preserve provider namespaces that the xAI scope-map API does not own.
pub(crate) fn other_provider_entries(
    auth_file: &Path,
) -> serde_json::Map<String, serde_json::Value> {
    let Ok(bytes) = std::fs::read(auth_file) else {
        return serde_json::Map::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return serde_json::Map::new();
    };
    if !is_provider_store_envelope(&value) {
        return serde_json::Map::new();
    }
    serde_json::from_value::<ProviderAuthStore>(value)
        .map(|store| store.providers.other)
        .unwrap_or_default()
}

fn provider_store_for_write(
    auth_file: &Path,
    auth_store: &AuthStore,
) -> std::io::Result<ProviderAuthStore> {
    let other = other_provider_entries(auth_file);
    let mut store = if auth_store.is_empty() {
        ProviderAuthStore {
            version: PROVIDER_STORE_VERSION,
            providers: ProviderEntries {
                xai: None,
                other: Default::default(),
            },
        }
    } else {
        ProviderAuthStore::from_legacy(auth_store.clone())?
    };
    store.providers.other = other;
    Ok(store)
}

enum DecodedAuthStore {
    V2(AuthStore),
    Legacy(AuthStore),
}

/// RAII guard for an exclusive advisory lock on `auth.json.lock`.
/// The lock is released when the inner `File` is dropped (closing the FD).
pub(crate) struct AuthFileLock {
    pub(super) _file: File,
}

impl AuthFileLock {
    /// Returns `true` while this guard still refers to the **live**
    /// `auth.json.lock` inode.
    ///
    /// A waiter that finds a holder stuck past the stale-lock timeout breaks
    /// the lock by `unlink`ing the file and recreating it on a fresh inode
    /// (see [`crate::auth::manager::lock`]). The usual cause of a "stuck"
    /// holder is a process **suspended across system sleep** while holding the
    /// lock: it stays alive (so the kernel never releases its flock) yet makes
    /// no progress, so siblings break it. When such a holder resumes, its
    /// flock lives on the now-deleted inode — it no longer holds the live lock
    /// even though this `AuthFileLock` still exists.
    ///
    /// Callers about to perform an irreversible, lock-protected action
    /// (sending a refresh token to the IdP, writing `auth.json`) MUST
    /// re-validate first; otherwise two processes can spend the same refresh
    /// token and trip token-family revocation.
    ///
    /// Non-Unix has no inode concept, so this conservatively returns `true`.
    #[cfg(unix)]
    pub(crate) fn still_live(&self, auth_json_path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let lock_path = auth_json_path.with_file_name("auth.json.lock");
        let (Ok(fd_meta), Ok(path_meta)) = (self._file.metadata(), std::fs::metadata(&lock_path))
        else {
            // Lock file gone or unreadable → we no longer hold the live lock.
            return false;
        };
        fd_meta.ino() == path_meta.ino() && fd_meta.dev() == path_meta.dev()
    }

    #[cfg(not(unix))]
    pub(crate) fn still_live(&self, _auth_json_path: &Path) -> bool {
        true
    }
}

pub fn read_auth_json(auth_file: &Path) -> std::io::Result<AuthStore> {
    let mut file = File::open(auth_file)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    // Empty files are valid (recover from prior crash/partial write).
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(AuthStore::new());
    }

    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let decoded = if is_provider_store_envelope(&value) {
        let v2 = serde_json::from_str::<ProviderAuthStore>(trimmed)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        DecodedAuthStore::V2(v2.into_legacy()?)
    } else {
        let legacy = serde_json::from_str(trimmed)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        DecodedAuthStore::Legacy(legacy)
    };

    match decoded {
        DecodedAuthStore::V2(store) => Ok(store),
        DecodedAuthStore::Legacy(store) => {
            // A migration is best-effort when another auth writer already
            // owns the lock.  Returning the legacy value is safe; that writer
            // or the next read will retry, and we never overwrite its v2 file.
            migrate_legacy_store(auth_file, &store)?;
            Ok(store)
        }
    }
}

/// v2 is recognized only by its complete canonical envelope. A legacy scope
/// literally named `version` must remain a legacy map, not a malformed v2.
fn is_provider_store_v2(value: &serde_json::Value) -> bool {
    value.get("version").and_then(serde_json::Value::as_u64) == Some(PROVIDER_STORE_VERSION.into())
        && is_provider_store_envelope(value)
}

fn is_provider_store_envelope(value: &serde_json::Value) -> bool {
    value
        .get("version")
        .is_some_and(serde_json::Value::is_number)
        && value
            .get("providers")
            .is_some_and(serde_json::Value::is_object)
}

/// Migrate only while holding the existing auth.json advisory lock.  Re-read
/// after acquisition so an earlier v2 writer always wins over stale legacy
/// bytes observed before waiting for the lock.
fn migrate_legacy_store(auth_file: &Path, legacy: &AuthStore) -> std::io::Result<()> {
    if legacy.is_empty() {
        return Ok(());
    }
    let Some(lock) = super::manager::lock::try_lock_auth_file_nonblocking(auth_file) else {
        return Ok(());
    };
    migrate_legacy_store_while_locked(auth_file, &lock)
}

/// Migrate legacy scope-map bytes while the caller holds the live
/// `auth.json.lock`. This deliberately does not call [`read_auth_json`]: that
/// reader may attempt to acquire the same non-reentrant advisory lock.
pub(super) fn migrate_legacy_store_while_locked(
    auth_file: &Path,
    lock: &AuthFileLock,
) -> std::io::Result<()> {
    let current = std::fs::read_to_string(auth_file)?;
    let trimmed = current.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let current_value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if is_provider_store_v2(&current_value) {
        return Ok(());
    }
    // Preserve the exact legacy data read under the lock. A parse failure is
    // returned and leaves the original bytes untouched.
    let locked_legacy: AuthStore = serde_json::from_str(trimmed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if !lock.still_live(auth_file) {
        // A stale lock inode may coexist with a live writer. Re-readers will
        // retry migration; this holder must never perform the irreversible write.
        return Ok(());
    }
    write_auth_json(auth_file, &locked_legacy)
}

/// Read auth.json, returning an empty map if the file does not exist.
///
/// Non-empty corrupt JSON, permission errors, etc. are returned as errors
/// so the caller can decide whether to skip the write (to avoid clobbering
/// sibling scopes).
///
/// Kept for the test-only `persist_and_swap` and as a strict reader.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used from tests only; remove expect when wired in production"
    )
)]
pub(crate) fn read_auth_json_or_empty(auth_file: &Path) -> std::io::Result<AuthStore> {
    match read_auth_json(auth_file) {
        Ok(map) => Ok(map),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AuthStore::new()),
        Err(e) => Err(e),
    }
}

/// Best-effort backup of a corrupt (unparseable) auth.json.
///
/// If the file exists and `read_auth_json` fails with `InvalidData`,
/// it is renamed to `auth.json.corrupt.<millis>` (sibling in the same
/// directory) and the backup path is returned. Used before recovery
/// writes so the original bytes are never silently lost.
pub(crate) fn backup_corrupt_auth_file(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    if serde_json::from_str::<serde_json::Value>(&contents)
        .ok()
        .is_some_and(|value| is_provider_store_envelope(&value))
    {
        // An unsupported provider-store version is not corruption. Retain it
        // verbatim so a newer client can still read it.
        return None;
    }
    if read_auth_json(path).is_ok() {
        return None;
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "auth.json".to_string());

    let backup_name = format!("{}.corrupt.{}", file_name, ts);
    let backup = path.with_file_name(backup_name);

    match std::fs::rename(path, &backup) {
        Ok(()) => {
            tracing::warn!(
                original = %path.display(),
                backup = %backup.display(),
                "auth: backed up corrupt auth.json before recovery write"
            );
            // Must reach unified.jsonl: the tracing line above is invisible
            // in production captures, and this is the only record of both
            // the corruption and where the original bytes went.
            xai_grok_telemetry::unified_log::error(
                "auth: corrupt auth.json backed up",
                None,
                Some(serde_json::json!({
                    "original": path.display().to_string(),
                    "backup": backup.display().to_string(),
                })),
            );
            Some(backup)
        }
        Err(e) => {
            tracing::warn!(error = %e, "auth: failed to rename corrupt auth.json for backup");
            xai_grok_telemetry::unified_log::error(
                "auth: corrupt auth.json backup failed",
                None,
                Some(serde_json::json!({
                    "original": path.display().to_string(),
                    "error": e.to_string(),
                })),
            );
            None
        }
    }
}

/// Read auth.json for an upcoming write, with recovery for corrupt files.
///
/// - Missing/empty → empty map (safe to write fresh)
/// - Valid JSON → parsed map
/// - Non-empty corrupt JSON → backs up to `auth.json.corrupt.<millis>`,
///   then returns empty map so the caller can write the new credential.
///
/// Other I/O errors (PermissionDenied, etc.) are still returned as errors.
pub(crate) fn read_auth_json_or_empty_recovering_corrupt(
    auth_file: &Path,
) -> std::io::Result<AuthStore> {
    read_auth_json_or_empty_recovering_corrupt_with_lock(auth_file, None)
}

fn read_auth_json_or_empty_recovering_corrupt_with_lock(
    auth_file: &Path,
    lock: Option<&AuthFileLock>,
) -> std::io::Result<AuthStore> {
    match read_auth_json(auth_file) {
        Ok(map) => Ok(map),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AuthStore::new()),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            if lock.is_some_and(|lock| !lock.still_live(auth_file)) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "auth lock replaced",
                ));
            }
            if backup_corrupt_auth_file(auth_file).is_some() {
                Ok(AuthStore::new())
            } else {
                // A failed backup must not turn corruption into silent data
                // loss by overwriting the only copy with an empty store.
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

/// Persist `auth.json`, preferring a crash-safe atomic write but falling
/// back to a non-atomic in-place write when the disk is full.
///
/// The atomic path (temp + rename) needs free space >= the file size,
/// because the old file and a full temp copy coexist until the rename. On a
/// nearly-full disk that temp copy can fail with `StorageFull` (ENOSPC)
/// even though the credentials themselves are tiny. When that happens we
/// retry with an in-place truncate+write, which only needs the freed blocks
/// of the old file — far less than the temp-copy approach.
///
/// The in-place path is non-atomic, with two accepted trade-offs:
/// - If the in-place write itself fails (e.g. a concurrent process grabs the
///   just-freed blocks, or a crash mid-write), the prior bytes are restored
///   best-effort so a torn/empty file never *replaces* the previous on-disk
///   credential — on-disk state ends up no worse than before the attempt.
/// - Unlocked concurrent readers can still observe a torn (partial) file
///   during the brief write window; a partial file is healed on the next
///   read via [`read_auth_json_or_empty_recovering_corrupt`] (backup +
///   relogin). This window is inherent to any sub-1×-free single-file
///   replace and is preferable to persisting nothing at all, which would
///   leave every concurrent process with a stale, already-revoked token.
pub(super) fn write_auth_json(auth_file: &Path, auth_store: &AuthStore) -> std::io::Result<()> {
    let provider_store = provider_store_for_write(auth_file, auth_store)?;
    write_provider_store_with(auth_file, &provider_store, write_provider_store_atomic)
}

/// Dispatch helper: run `atomic`, and on `StorageFull` fall back to an
/// in-place write. Split out (with `atomic` injectable) so the disk-full
/// fallback is unit-testable without an actually-full filesystem.
fn write_provider_store_with(
    auth_file: &Path,
    provider_store: &ProviderAuthStore,
    atomic: fn(&Path, &ProviderAuthStore) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match atomic(auth_file, provider_store) {
        Err(e) if e.kind() == std::io::ErrorKind::StorageFull => {
            tracing::warn!(
                path = %auth_file.display(),
                "auth: disk full during atomic write, falling back to in-place write"
            );
            // Must reach unified.jsonl: a silent in-memory-only credential
            // (the prior behavior) leaves sibling processes with a stale
            // refresh token and no record of why. Surface it loudly.
            xai_grok_telemetry::unified_log::warn(
                "auth: disk full, falling back to non-atomic in-place write",
                None,
                Some(serde_json::json!({
                    "path": auth_file.display().to_string(),
                })),
            );
            write_provider_store_in_place(auth_file, provider_store)
        }
        other => other,
    }
}

/// Serialize `auth_store` to `path` (truncate + rewrite), owner-only (0o600)
/// and `fsync`'d. Shared core of the atomic path (which targets the temp
/// file) and the in-place fallback (which targets `auth.json` directly).
///
/// Uses streaming `to_writer_pretty` through a `BufWriter` to avoid
/// allocating the entire JSON string in memory — eliminates OOM risk under
/// severe memory pressure.
fn write_store_to(path: &Path, provider_store: &ProviderAuthStore) -> std::io::Result<()> {
    use crate::util::secure_file::open_secure_file;

    if let Some(parent) = path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            set_secure_directory_permissions(parent)?;
        }
    }
    let file = open_secure_file(path)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &provider_store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|e| e.into_error())?
        .sync_all()?;
    #[cfg(windows)]
    {
        crate::util::secure_file::set_windows_secure_permissions(path)?;
    }
    Ok(())
}

pub(super) fn set_secure_directory_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Atomic write: tmp + rename. Unix `rename(2)` replaces atomically;
/// Windows `rename` requires removing the target first.
fn write_provider_store_atomic(
    auth_file: &Path,
    provider_store: &ProviderAuthStore,
) -> std::io::Result<()> {
    let tmp = auth_file.with_extension(format!("json.{}.tmp", std::process::id()));
    write_store_to(&tmp, provider_store)?;
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(auth_file);
    }
    std::fs::rename(&tmp, auth_file)?;
    sync_parent_directory(auth_file);
    Ok(())
}

/// Atomic owner-only writer for provider namespaces that are not represented
/// by the legacy xAI `AuthStore`.  Callers must hold `auth.json.lock`.
pub(super) fn write_provider_json_atomic(
    auth_file: &Path,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    use crate::util::secure_file::open_secure_file;
    let tmp = auth_file.with_extension(format!("json.{}.tmp", std::process::id()));
    let file = open_secure_file(&tmp)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|e| e.into_error())?
        .sync_all()?;
    #[cfg(windows)]
    crate::util::secure_file::set_windows_secure_permissions(&tmp)?;
    #[cfg(windows)]
    let _ = std::fs::remove_file(auth_file);
    std::fs::rename(tmp, auth_file)?;
    sync_parent_directory(auth_file);
    Ok(())
}

/// Best-effort parent-directory sync makes a successful rename durable on
/// filesystems that do not implicitly flush the directory entry.
fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

/// Non-atomic fallback: truncate and rewrite `auth.json` in place.
///
/// Used only when [`write_auth_json_atomic`] fails with `StorageFull`.
/// Opening with truncation first frees the old content's blocks before the
/// new bytes are written, so this needs only the file size in free space
/// rather than the temp-copy approach's file-size-of-headroom.
///
/// Truncation is destructive, so the prior bytes are snapshotted first and
/// restored best-effort if the rewrite fails partway — a failed fallback
/// must not leave an empty/torn file where a parseable (if stale) credential
/// used to be. A partial file that survives (because even the restore failed)
/// is healed on the next read via [`read_auth_json_or_empty_recovering_corrupt`].
fn write_provider_store_in_place(
    auth_file: &Path,
    provider_store: &ProviderAuthStore,
) -> std::io::Result<()> {
    write_provider_store_in_place_with(auth_file, provider_store, write_store_to)
}

/// Inner of [`write_auth_json_in_place`] with `write` injectable so the
/// rollback-on-failure path is unit-testable without an actually-full disk.
fn write_provider_store_in_place_with(
    auth_file: &Path,
    provider_store: &ProviderAuthStore,
    write: fn(&Path, &ProviderAuthStore) -> std::io::Result<()>,
) -> std::io::Result<()> {
    // Snapshot the prior bytes so a torn/empty write can be rolled back to
    // the previous on-disk credential. `None` when the file is absent.
    let prior = std::fs::read(auth_file).ok();
    match write(auth_file, provider_store) {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Some(prior) = prior
                && let Err(restore_err) = restore_prior_bytes(auth_file, &prior)
            {
                tracing::warn!(
                    error = %restore_err,
                    "auth: failed to restore prior auth.json after in-place write failure"
                );
            }
            Err(e)
        }
    }
}

/// Best-effort rollback: rewrite `bytes` (owner-only, `fsync`'d) after a
/// failed in-place write so a torn/empty file does not replace the prior
/// credential.
fn restore_prior_bytes(auth_file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use crate::util::secure_file::open_secure_file;

    let mut file = open_secure_file(auth_file)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(windows)]
    {
        crate::util::secure_file::set_windows_secure_permissions(auth_file)?;
    }
    Ok(())
}

/// Read a single auth token from `auth.json` by scope key.
/// Falls back to the legacy `https://accounts.x.ai/sign-in` scope key
/// when the requested scope is not found (devbox auth.json migration).
pub fn read_token_by_scope(grok_home: &Path, scope: &str) -> anyhow::Result<String> {
    let path = grok_home.join("auth.json");
    let store =
        read_auth_json(&path).map_err(|_| anyhow::anyhow!("Not logged in. Run `grok login`."))?;
    lookup_auth(&store, scope).map(|a| a.key).ok_or_else(|| {
        anyhow::anyhow!("Your auth token is invalid. Run `grok login` to re-authenticate.")
    })
}

/// Read the API key from the `xai::api_key` scope in auth.json.
pub fn read_api_key(grok_home: &Path) -> Option<String> {
    let path = grok_home.join("auth.json");
    let map = read_auth_json(&path).ok()?;
    map.get(API_KEY_SCOPE).map(|a| a.key.clone())
}

/// Store a plain API key in auth.json under the `xai::api_key` scope.
///
/// Uses the corrupt-recovery reader so a malformed auth.json (e.g. from a
/// previous crash) can be healed when the user sets an API key.
pub fn store_api_key(grok_home: &Path, api_key: &str) -> std::io::Result<()> {
    store_api_key_with(grok_home, api_key, |_| {})
}

fn store_api_key_with(
    grok_home: &Path,
    api_key: &str,
    before_write: fn(&Path),
) -> std::io::Result<()> {
    let path = grok_home.join("auth.json");
    // Recovery (read → corrupt backup → replacement write) must be one
    // critical section, otherwise a concurrent fresh credential can be
    // mistaken for the corrupt bytes we observed before locking.
    if !grok_home.exists() {
        std::fs::create_dir_all(grok_home)?;
        set_secure_directory_permissions(grok_home)?;
    }
    let _lock = super::manager::lock::try_lock_auth_file_nonblocking(&path)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::WouldBlock, "auth store is busy"))?;
    let mut map = read_auth_json_or_empty_recovering_corrupt_with_lock(&path, Some(&_lock))?;
    map.insert(
        API_KEY_SCOPE.to_owned(),
        GrokAuth {
            key: api_key.to_owned(),
            auth_mode: AuthMode::ApiKey,
            ..Default::default()
        },
    );
    before_write(&path);
    if !_lock.still_live(&path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "auth lock replaced",
        ));
    }
    write_auth_json(&path, &map)
}

/// Remove the `xai::api_key` scope from auth.json.
pub fn clear_api_key(grok_home: &Path) -> std::io::Result<()> {
    let path = grok_home.join("auth.json");
    if let Ok(mut map) = read_auth_json(&path) {
        map.remove(API_KEY_SCOPE);
        if map.is_empty() && other_provider_entries(&path).is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            write_auth_json(&path, &map)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod provider_store_red_tests {
    use super::*;

    fn legacy_store(key: &str) -> AuthStore {
        let mut store = AuthStore::new();
        store.insert(
            super::super::model::LEGACY_SCOPE.to_owned(),
            GrokAuth {
                key: key.to_owned(),
                ..GrokAuth::test_default()
            },
        );
        store
    }

    #[test]
    fn migrates_legacy_xai_store_to_versioned_provider_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let legacy = legacy_store("legacy-secret");
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        assert_eq!(
            read_auth_json(&path)
                .unwrap()
                .get(super::super::model::LEGACY_SCOPE)
                .unwrap()
                .key,
            "legacy-secret"
        );

        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(
            json["providers"]["xai"]["credentials"]["xai"]["key"],
            "legacy-secret"
        );
    }

    #[test]
    fn migration_is_idempotent_and_never_overwrites_existing_v2_credential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let existing = legacy_store("existing-secret");
        write_auth_json(&path, &existing).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert_eq!(
            read_auth_json(&path)
                .unwrap()
                .get(super::super::model::LEGACY_SCOPE)
                .unwrap()
                .key,
            "existing-secret"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn legacy_scope_named_version_is_not_misdetected_as_v2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut legacy = legacy_store("legacy-secret");
        legacy.insert("version".to_owned(), GrokAuth::test_default());
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let read = read_auth_json(&path).unwrap();
        assert!(read.contains_key("version"));
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(is_provider_store_v2(&json));
    }

    #[test]
    fn v2_canonical_credential_overrides_stale_compatibility_scope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut scopes = legacy_store("stale-secret");
        let canonical = GrokAuth {
            key: "canonical-secret".to_owned(),
            ..GrokAuth::test_default()
        };
        let store = ProviderAuthStore {
            version: PROVIDER_STORE_VERSION,
            providers: ProviderEntries {
                xai: Some(XaiProvider {
                    credentials: XaiCredentials {
                        xai: canonical,
                        scopes,
                    },
                }),
                other: serde_json::Map::new(),
            },
        };
        std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();

        assert_eq!(
            read_auth_json(&path).unwrap()[super::super::model::LEGACY_SCOPE].key,
            "canonical-secret"
        );
    }

    #[test]
    fn unsupported_provider_store_version_is_not_migrated_or_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut value =
            serde_json::to_value(ProviderAuthStore::from_legacy(legacy_store("future")).unwrap())
                .unwrap();
        value["version"] = serde_json::json!(3);
        let original = serde_json::to_vec(&value).unwrap();
        std::fs::write(&path, &original).unwrap();

        assert_eq!(
            read_auth_json(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(store_api_key(dir.path(), "replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn store_api_key_refuses_write_after_its_lock_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&legacy_store("old-secret")).unwrap(),
        )
        .unwrap();

        let err = store_api_key_with(dir.path(), "new-secret", |auth_path| {
            let lock_path = auth_path.with_file_name("auth.json.lock");
            let old_lock = auth_path.with_file_name("replaced.lock");
            std::fs::rename(&lock_path, old_lock).unwrap();
            std::fs::write(lock_path, b"replacement").unwrap();
        })
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            read_auth_json(&path).unwrap()[super::super::model::LEGACY_SCOPE].key,
            "old-secret"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_lock_guard_detects_replaced_lock_inode() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.json");
        std::fs::write(&auth_path, b"{}").unwrap();
        let lock = super::super::manager::lock::try_lock_auth_file_nonblocking(&auth_path).unwrap();
        let lock_path = dir.path().join("auth.json.lock");
        let old_lock_path = dir.path().join("old.lock");
        std::fs::rename(&lock_path, old_lock_path).unwrap();
        std::fs::write(&lock_path, b"replacement").unwrap();
        assert!(!lock.still_live(&auth_path));
    }

    #[test]
    fn corrupt_store_is_backed_up_before_a_recovery_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(&path, b"{not-json").unwrap();

        store_api_key(dir.path(), "replacement-secret").unwrap();

        let recovered = read_auth_json(&path).unwrap();
        assert_eq!(recovered[API_KEY_SCOPE].key, "replacement-secret");
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("auth.json.corrupt.")
            })
            .count();
        assert_eq!(backups, 1, "corrupt bytes must be retained in a backup");
    }

    #[cfg(unix)]
    #[test]
    fn provider_store_keeps_directory_and_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("private");
        store_api_key(&nested, "secret").unwrap();
        assert_eq!(
            std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(nested.join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[cfg(test)]
mod write_fallback_tests {
    use super::*;

    fn sample_store() -> AuthStore {
        let mut map = AuthStore::new();
        map.insert(
            API_KEY_SCOPE.to_owned(),
            GrokAuth {
                key: "secret-key".to_owned(),
                auth_mode: AuthMode::ApiKey,
                ..Default::default()
            },
        );
        map
    }

    fn read_key(path: &Path) -> Option<String> {
        read_auth_json(path)
            .ok()
            .and_then(|m| m.get(API_KEY_SCOPE).map(|a| a.key.clone()))
    }

    fn fake_storage_full(_: &Path, _: &ProviderAuthStore) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
    }

    fn fake_permission_denied(_: &Path, _: &ProviderAuthStore) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }

    /// Simulates an in-place write that truncates the file (destroying the
    /// old content, as `open_secure_file` does) and then fails partway — the
    /// torn-write case the rollback must recover from.
    fn fake_truncate_then_fail(path: &Path, _: &ProviderAuthStore) -> std::io::Result<()> {
        crate::util::secure_file::open_secure_file(path)?; // truncates to 0 bytes
        Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
    }

    #[test]
    fn in_place_write_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        write_provider_store_in_place(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
        )
        .unwrap();
        assert_eq!(read_key(&path).as_deref(), Some("secret-key"));
    }

    #[cfg(unix)]
    #[test]
    fn in_place_write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        write_provider_store_in_place(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "in-place write must stay 0o600");
    }

    /// A `StorageFull` (ENOSPC) failure on the atomic path must fall back to
    /// the in-place write so the credential still lands on disk.
    #[test]
    fn falls_back_to_in_place_on_storage_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        write_provider_store_with(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
            fake_storage_full,
        )
        .unwrap();
        assert_eq!(
            read_key(&path).as_deref(),
            Some("secret-key"),
            "disk-full atomic write must fall back to a successful in-place write"
        );
    }

    /// Non-ENOSPC errors must propagate unchanged and must NOT trigger the
    /// in-place fallback (e.g. a permission error should not write the file).
    #[test]
    fn propagates_non_storage_full_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let err = write_provider_store_with(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
            fake_permission_denied,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!path.exists(), "non-ENOSPC failure must not write the file");
    }

    /// The normal (real atomic) path still works end to end.
    #[test]
    fn atomic_write_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        write_auth_json(&path, &sample_store()).unwrap();
        assert_eq!(read_key(&path).as_deref(), Some("secret-key"));
    }

    /// A fallback write that truncates then fails must roll back to the prior
    /// bytes instead of leaving an empty/torn file — otherwise a second
    /// disk-full failure would destroy a previously-valid credential.
    #[test]
    fn in_place_restores_prior_bytes_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        // Seed a valid prior credential.
        write_provider_store_in_place(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
        )
        .unwrap();
        assert_eq!(read_key(&path).as_deref(), Some("secret-key"));

        let mut replacement = AuthStore::new();
        replacement.insert(
            API_KEY_SCOPE.to_owned(),
            GrokAuth {
                key: "replacement-key".to_owned(),
                auth_mode: AuthMode::ApiKey,
                ..Default::default()
            },
        );
        let err = write_provider_store_in_place_with(
            &path,
            &ProviderAuthStore::from_legacy(replacement).unwrap(),
            fake_truncate_then_fail,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::StorageFull);
        assert_eq!(
            read_key(&path).as_deref(),
            Some("secret-key"),
            "a failed in-place write must restore the prior credential, not leave an empty file"
        );
    }

    /// Rollback after a failed write must keep the file owner-only (0o600).
    #[cfg(unix)]
    #[test]
    fn in_place_restore_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        write_provider_store_in_place(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
        )
        .unwrap();
        let _ = write_provider_store_in_place_with(
            &path,
            &ProviderAuthStore::from_legacy(sample_store()).unwrap(),
            fake_truncate_then_fail,
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "restored file must stay 0o600");
    }
}
