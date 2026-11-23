use crate::{ErrorCode, Failure, MAXIMUM_RAW_RECOVERY_BYTES};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

pub(crate) const GENERATION_STAGE_PREFIX: &str = ".generation-";
pub(crate) const GENERATION_STAGE_DATABASE_PREFIX: &str = ".tmp";
pub(crate) const GENERATION_STAGE_RANDOM_LENGTH: usize = 6;

pub(crate) fn private_new_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub(crate) fn regular(path: &Path) -> Result<(), Failure> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.is_symlink() {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(())
}
/// Open without following a replaced final link or blocking on a substituted FIFO.
pub(crate) fn open_regular(path: &Path) -> Result<File, Failure> {
    open_regular_with(path, Failure::from)
}

fn open_regular_with(
    path: &Path,
    map_io: impl Fn(std::io::Error) -> Failure,
) -> Result<File, Failure> {
    if !fs::symlink_metadata(path).map_err(&map_io)?.is_file() {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = options.open(path).map_err(&map_io)?;
    validate_regular_handle(&file, map_io)?;
    Ok(file)
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

fn validate_regular_handle(
    file: &File,
    map_io: impl Fn(std::io::Error) -> Failure,
) -> Result<(), Failure> {
    let metadata = file.metadata().map_err(map_io)?;
    #[cfg(windows)]
    reject_reparse_point(&metadata)?;
    if !metadata.is_file() {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(())
}

#[cfg(windows)]
fn reject_reparse_point(metadata: &fs::Metadata) -> Result<(), Failure> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(())
}
/// Distinguishes absence from unreadable entries and dangling symlinks.
pub(crate) fn exists(path: &Path) -> Result<bool, Failure> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && !path.components().any(|part| part == Component::ParentDir)
}

/// Resolves host parent aliases and reports whether the root was observed to exist.
pub(crate) fn resolve_root(path: &Path) -> Result<(PathBuf, bool), Failure> {
    if !valid_absolute_path(path) {
        return Err(Failure::new(ErrorCode::InvalidConfiguration));
    }
    // Strip trailing separators and `.` before checking the root itself for links.
    let supplied: PathBuf = path.components().collect();
    let mut ancestor = supplied.as_path();
    let mut missing = Vec::new();
    loop {
        if exists(ancestor)? {
            let root_exists = missing.is_empty();
            if root_exists {
                existing_directory(ancestor)?;
            }
            let mut resolved = fs::canonicalize(ancestor)?;
            existing_directory(&resolved)?;
            for component in missing.into_iter().rev() {
                resolved.push(component);
            }
            return Ok((resolved, root_exists));
        }
        missing.push(
            ancestor
                .file_name()
                .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?;
    }
}
pub(crate) fn directory(path: &Path) -> Result<(), Failure> {
    if exists(path)? {
        return existing_directory(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?;
    directory(parent)?;
    let created = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path)
        }
        #[cfg(not(unix))]
        {
            fs::create_dir(path)
        }
    };
    match created {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            existing_directory(path)?
        }
        Err(error) => return Err(error.into()),
    }
    // A synced child alone does not make its new entry in the parent durable.
    sync_directory(parent)
}
pub(crate) fn existing_directory(path: &Path) -> Result<(), Failure> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(())
}

/// Identifies the private layout left by a generation write before publication.
/// The directory and its bytes remain available for manual inspection.
pub(crate) fn is_generation_stage(entry: &fs::DirEntry) -> Result<bool, Failure> {
    let name = entry.file_name();
    if !name
        .to_str()
        .is_some_and(|name| generation_stage_name(name, GENERATION_STAGE_PREFIX))
    {
        return Ok(false);
    }

    let directory = entry.path();
    existing_directory(&directory)?;
    #[cfg(windows)]
    reject_reparse_point(&fs::symlink_metadata(&directory)?)?;
    let mut database_name = None::<String>;
    let mut has_main = false;
    for child in fs::read_dir(directory)? {
        let child = child?;
        let Ok(name) = child.file_name().into_string() else {
            return Ok(false);
        };
        let base = ["-journal", "-wal", "-shm"]
            .into_iter()
            .find_map(|suffix| name.strip_suffix(suffix))
            .unwrap_or(&name);
        if !generation_stage_name(base, GENERATION_STAGE_DATABASE_PREFIX)
            || database_name
                .as_deref()
                .is_some_and(|expected| expected != base)
        {
            return Ok(false);
        }
        drop(open_regular(&child.path())?);
        database_name.get_or_insert_with(|| base.to_owned());
        has_main |= name == base;
    }
    Ok(database_name.is_none() || has_main)
}

fn generation_stage_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == GENERATION_STAGE_RANDOM_LENGTH
            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

/// An absent backup selector may only refer to the original fixed-name layout.
pub(crate) fn legacy_backup_only(directory: &Path) -> Result<bool, Failure> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !legacy_backup_layout_member(&entry, &entry.file_name())? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn legacy_backup_layout_member(entry: &fs::DirEntry, name: &OsStr) -> Result<bool, Failure> {
    if [
        "snapshot.sqlite",
        "snapshot.sqlite-journal",
        "snapshot.sqlite-wal",
        "snapshot.sqlite-shm",
    ]
    .into_iter()
    .any(|member| name == member)
    {
        return Ok(true);
    }
    // Finder and Explorer can add metadata without changing which checkpoint
    // the legacy directory selects. Leave these ordinary files untouched.
    let desktop_metadata = name.to_str().is_some_and(|name| {
        [".DS_Store", "desktop.ini", "Thumbs.db"]
            .into_iter()
            .any(|metadata| name.eq_ignore_ascii_case(metadata))
    });
    Ok(desktop_metadata && entry.file_type()?.is_file())
}
pub(crate) fn family_member(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}
pub(crate) fn small_file(path: &Path) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    open_regular(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(bytes)
}
pub(crate) fn lock(root: &Path) -> Result<File, Failure> {
    open_lock(root, true)
}
pub(crate) fn existing_lock(root: &Path) -> Result<File, Failure> {
    open_lock(root, false)
}
fn open_lock(root: &Path, create: bool) -> Result<File, Failure> {
    let path = root.join("store.lock");
    let existed = exists(&path)?;
    if existed {
        regular(&path)?;
    } else if !create {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    no_follow(&mut options);
    let file = match options.open(&path) {
        Err(error)
            if cfg!(target_os = "macos")
                && existed
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
                ) =>
        {
            // Locking does not change the file's contents. Do not create or
            // follow a replacement, or weaken the exclusive lock below.
            open_regular(&path)?
        }
        result => {
            let file = result?;
            validate_regular_handle(&file, Failure::from)?;
            file
        }
    };
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => Failure::new(ErrorCode::Busy),
        std::fs::TryLockError::Error(_) => Failure::new(ErrorCode::IoFailure),
    })?;
    Ok(file)
}
pub(crate) fn staged_file(path: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile, Failure> {
    let parent = path
        .parent()
        .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?;
    // Replacements retain file attributes on Windows; staging must use normal files.
    let mut file = tempfile::Builder::new().make_in(parent, private_new_file)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}
pub(crate) fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    staged_file(path, bytes)?
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    sync_directory(
        path.parent()
            .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?,
    )
}
/// Publishes a staged file without replacing an existing entry. The caller
/// syncs the destination directory after publication.
pub(crate) fn persist_new(source: tempfile::TempPath, destination: &Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        source
            .persist_noclobber(destination)
            .map_err(|error| error.error)
    }
    #[cfg(windows)]
    {
        move_file(&source, destination, false)
    }
}
pub(crate) fn atomic_replace(source: &Path, destination: &Path) -> Result<(), Failure> {
    if exists(destination)? {
        regular(destination)?;
    }
    #[cfg(not(windows))]
    fs::rename(source, destination)?;
    #[cfg(windows)]
    move_file(source, destination, true)?;
    sync_directory(
        destination
            .parent()
            .ok_or_else(|| Failure::new(ErrorCode::InvalidConfiguration))?,
    )
}
#[cfg(windows)]
fn move_file(source: &Path, destination: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
pub(crate) fn sync_directory(path: &Path) -> Result<(), Failure> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub(crate) fn sync_file(path: &Path) -> Result<(), Failure> {
    let mut options = OpenOptions::new();
    options.read(true);
    // Windows FlushFileBuffers requires GENERIC_WRITE access.
    #[cfg(windows)]
    options.write(true);
    options.open(path)?.sync_all()?;
    Ok(())
}
/// Modification dates are metadata, not an integrity condition. Preserve signed
/// Unix seconds for valid files written before the epoch, including fractions.
pub(crate) fn modification_time(metadata: &fs::Metadata) -> Result<f64, Failure> {
    Ok(match metadata.modified()?.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    })
}
pub(crate) fn preserve_family(path: &Path, root: &Path) -> Result<PathBuf, Failure> {
    let expected = family_fingerprint(path)?;
    let parent = root.join("Recovery");
    directory(&parent)?;
    let identity: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
    let destination = parent.join(identity);
    if exists(&destination)? {
        existing_directory(&destination)?;
        if family_fingerprint(&destination.join(path.file_name().unwrap()))? != expected {
            return Err(Failure::new(ErrorCode::InvalidStoredData));
        }
        if family_fingerprint(path)? != expected {
            return Err(Failure::new(ErrorCode::Busy));
        }
        // Finish publication durability even after an earlier process stopped
        // between renaming a complete capture and syncing its parent.
        sync_directory(&parent)?;
        sync_directory(root)?;
        return Ok(destination);
    }
    let staging = tempfile::Builder::new()
        .prefix(".capture-")
        .tempdir_in(&parent)?;
    let result = staging.path();
    copy_family_into(path, result, MAXIMUM_RAW_RECOVERY_BYTES, true)?;
    sync_directory(result)?;
    let captured = result.join(path.file_name().unwrap());
    let fingerprint = family_fingerprint(&captured)?;
    if fingerprint != expected || fingerprint != family_fingerprint(path)? {
        return Err(Failure::new(ErrorCode::Busy));
    }
    if exists(&destination)? {
        existing_directory(&destination)?;
        if family_fingerprint(&destination.join(path.file_name().unwrap()))? != fingerprint {
            return Err(Failure::new(ErrorCode::InvalidStoredData));
        }
    } else {
        fs::rename(result, &destination)?;
    }
    sync_directory(&parent)?;
    sync_directory(root)?;
    Ok(destination)
}

/// Refuse oversized or non-regular managed members before SQLite sees them.
pub(crate) fn check_raw_family_size(path: &Path) -> Result<(), Failure> {
    let mut remaining = MAXIMUM_RAW_RECOVERY_BYTES;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        match fs::symlink_metadata(family_member(path, suffix)) {
            Ok(metadata) if metadata.is_file() => {
                remaining = remaining
                    .checked_sub(metadata.len())
                    .ok_or_else(|| Failure::new(ErrorCode::RawRecoveryLimitExceeded))?;
            }
            Ok(_) => return Err(Failure::new(ErrorCode::InvalidStoredData)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn copy_family_into(
    path: &Path,
    directory: &Path,
    maximum: u64,
    durable: bool,
) -> Result<(), Failure> {
    let mut remaining = maximum;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let source = family_member(path, suffix);
        if exists(&source)? {
            let input = source_file(&source)?;
            if input.metadata()?.len() > remaining {
                return Err(Failure::new(ErrorCode::RawRecoveryLimitExceeded));
            }
            let mut input = input.take(remaining.saturating_add(1));
            let target = directory.join(source.file_name().unwrap());
            let mut output = private_new_file(&target)?;
            let copied = std::io::copy(&mut input, &mut output)?;
            if copied > remaining {
                return Err(Failure::new(ErrorCode::RawRecoveryLimitExceeded));
            }
            remaining -= copied;
            if durable {
                output.sync_all()?;
            }
        }
    }
    Ok(())
}

fn family_fingerprint(path: &Path) -> Result<[u8; 32], Failure> {
    bounded_family_fingerprint(path, MAXIMUM_RAW_RECOVERY_BYTES)
}

pub(crate) fn bounded_family_fingerprint(path: &Path, maximum: u64) -> Result<[u8; 32], Failure> {
    let mut state = Sha256::new();
    state.update(b"configuration-storage-raw-family-v1\0");
    let name = path.file_name().unwrap_or_default().as_encoded_bytes();
    state.update((name.len() as u64).to_le_bytes());
    state.update(name);
    let mut remaining = maximum;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        hash_file_bounded(&mut state, &family_member(path, suffix), 0, &mut remaining)?;
    }
    Ok(state.finalize().into())
}

pub(crate) fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn is_managed_backup_member(name: &str) -> bool {
    let main = ["-journal", "-wal", "-shm"]
        .into_iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name);
    main.strip_suffix(".sqlite")
        .is_some_and(|stem| stem == "snapshot" || canonical_uuid(stem))
}

/// Bind the selector and the selected family. If selection is ambiguous,
/// bind only managed backup members, including orphaned sidecars.
#[derive(PartialEq, Eq)]
pub(crate) struct BackupFingerprint {
    pub digest: [u8; 32],
    pub selection: BackupSelection,
}

#[derive(PartialEq, Eq)]
pub(crate) enum BackupSelection {
    Checkpoint(String),
    DamagedMarker([u8; 32]),
    Unselected,
}

pub(crate) fn backup_fingerprint(
    root: &Path,
    reservation: Option<&OsStr>,
) -> Result<BackupFingerprint, Failure> {
    const MAXIMUM_MANAGED_MEMBERS: usize = 1024;
    let mut state = Sha256::new();
    state.update(b"configuration-storage-backup-files-v2\0");
    let mut remaining = MAXIMUM_RAW_RECOVERY_BYTES;
    let marker = hash_file_bounded(
        &mut state,
        &root.join("active-backup"),
        4097,
        &mut remaining,
    )?;
    let directory = root.join("Backups");
    let exists = exists(&directory)?;
    if exists {
        existing_directory(&directory)?;
    }
    let selection = if let Some((name, marker)) = marker.as_ref().and_then(|marker| {
        std::str::from_utf8(&marker.prefix)
            .ok()
            .filter(|name| canonical_uuid(name))
            .map(|name| (name, marker))
    }) {
        state.update([1]);
        let name = format!("{name}.sqlite");
        let path = directory.join(&name);
        let present = hash_file_bounded(&mut state, &path, 0, &mut remaining)?.is_some();
        for suffix in ["-journal", "-wal", "-shm"] {
            hash_file_bounded(&mut state, &family_member(&path, suffix), 0, &mut remaining)?;
        }
        if present {
            BackupSelection::Checkpoint(name)
        } else {
            BackupSelection::DamagedMarker(marker.digest)
        }
    } else {
        state.update([0]);
        let mut names = BTreeSet::new();
        let mut legacy_only = true;
        if exists {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name();
                // An owned publication candidate is checked separately before selection.
                if reservation == Some(name.as_os_str()) {
                    continue;
                }
                legacy_only = legacy_only && legacy_backup_layout_member(&entry, &name)?;
                if name.to_str().is_some_and(is_managed_backup_member) {
                    names.insert(name);
                    if names.len() > MAXIMUM_MANAGED_MEMBERS {
                        return Err(Failure::new(ErrorCode::RawRecoveryLimitExceeded));
                    }
                }
            }
        }
        for name in &names {
            let bytes = name.as_encoded_bytes();
            state.update((bytes.len() as u64).to_le_bytes());
            state.update(bytes);
            hash_file_bounded(&mut state, &directory.join(name), 0, &mut remaining)?;
        }
        if let Some(marker) = marker {
            BackupSelection::DamagedMarker(marker.digest)
        } else if legacy_only && names.contains(OsStr::new("snapshot.sqlite")) {
            BackupSelection::Checkpoint("snapshot.sqlite".into())
        } else {
            BackupSelection::Unselected
        }
    };
    Ok(BackupFingerprint {
        digest: state.finalize().into(),
        selection,
    })
}

pub(crate) fn validate_captured_selector(
    path: &Path,
    expected: [u8; 32],
    changed: ErrorCode,
) -> Result<(), Failure> {
    if file_digest(path)? != Some(expected) {
        return Err(Failure::new(changed));
    }
    Ok(())
}

pub(crate) fn file_digest(path: &Path) -> Result<Option<[u8; 32]>, Failure> {
    let mut remaining = MAXIMUM_RAW_RECOVERY_BYTES;
    Ok(hash_file_bounded(&mut Sha256::new(), path, 0, &mut remaining)?.map(|file| file.digest))
}

#[derive(PartialEq, Eq)]
pub(crate) struct SelectionFingerprint {
    pub digest: [u8; 32],
    pub selector_digest: Option<[u8; 32]>,
}

/// Hashes bytes without opening SQLite, including corrupt files and explicit absence.
/// Only a canonical marker can select paths; malformed marker bytes are still bound.
pub(crate) fn selection_fingerprint(root: &Path) -> Result<SelectionFingerprint, Failure> {
    let mut state = Sha256::new();
    state.update(b"configuration-storage-selection-v1");
    let mut remaining = MAXIMUM_RAW_RECOVERY_BYTES;
    let marker = hash_file_bounded(
        &mut state,
        &root.join("active-generation"),
        4097,
        &mut remaining,
    )?;
    if let Some(marker) = &marker
        && let Ok(name) = std::str::from_utf8(&marker.prefix)
        && let Ok(id) = Uuid::parse_str(name)
        && id.to_string() == name
    {
        state.update([1]);
        let directory = root.join("Generations");
        if exists(&directory)? {
            existing_directory(&directory)?;
        }
        let selected = directory.join(format!("{name}.sqlite"));
        for suffix in ["", "-journal", "-wal", "-shm"] {
            hash_file_bounded(
                &mut state,
                &family_member(&selected, suffix),
                0,
                &mut remaining,
            )?;
        }
    } else {
        state.update([0]);
    }
    Ok(SelectionFingerprint {
        digest: state.finalize().into(),
        selector_digest: marker.map(|marker| marker.digest),
    })
}

/// A path disappearing after its presence was observed is a changing source,
/// not an unrelated I/O failure. The candidate facade further identifies main-file loss.
fn source_file(path: &Path) -> Result<File, Failure> {
    open_regular_with(path, |error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Failure::new(ErrorCode::Busy)
        } else {
            error.into()
        }
    })
}

struct HashedFile {
    prefix: Vec<u8>,
    digest: [u8; 32],
}

fn hash_file_bounded(
    state: &mut Sha256,
    path: &Path,
    prefix_limit: usize,
    remaining: &mut u64,
) -> Result<Option<HashedFile>, Failure> {
    if !exists(path)? {
        state.update([0]);
        return Ok(None);
    }
    state.update([1]);
    let mut file = source_file(path)?;
    if file.metadata()?.len() > *remaining {
        return Err(Failure::new(ErrorCode::RawRecoveryLimitExceeded));
    }
    let mut digest = Sha256::new();
    let mut prefix = Vec::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..capacity])?;
        if count == 0 {
            break;
        }
        if count as u64 > *remaining {
            return Err(Failure::new(ErrorCode::RawRecoveryLimitExceeded));
        }
        *remaining -= count as u64;
        digest.update(&buffer[..count]);
        let retain = count.min(prefix_limit - prefix.len());
        prefix.extend_from_slice(&buffer[..retain]);
    }
    let digest = digest.finalize().into();
    state.update(digest);
    Ok(Some(HashedFile { prefix, digest }))
}
