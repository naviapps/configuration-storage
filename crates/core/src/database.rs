use crate::{Configuration, ErrorCode, Failure, Snapshot};
use rusqlite::{
    Connection, OpenFlags, TransactionBehavior,
    backup::{Backup, StepResult},
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};
use uuid::Uuid;

const STORE_SCHEMA: &str = "CREATE TABLE store_state (id INTEGER PRIMARY KEY CHECK(id=1), owner TEXT NOT NULL, generation TEXT NOT NULL, payload BLOB NOT NULL, digest TEXT NOT NULL)";

pub(crate) fn digest(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
/// Opaque editing identity, bound to both the stored write nonce and verified bytes.
pub(crate) fn generation(owner: &str, nonce: &str, checksum: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"configuration-storage:generation:v1\0");
    for part in [owner, nonce, checksum] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// This store uses single-database transactions. A rollback journal that names
/// a super-journal can make SQLite open or delete files outside the managed family.
/// Reject the reference without resolving it, for both live and copied databases.
fn refuse_external_journal(path: &Path) -> Result<(), Failure> {
    let journal = crate::filesystem::family_member(path, "-journal");
    if !crate::filesystem::exists(&journal)? {
        return Ok(());
    }
    let mut file = crate::filesystem::open_regular(&journal)?;
    if file.metadata()?.len() < 16 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-16))?;
    let mut footer = [0; 16];
    file.read_exact(&mut footer)?;
    if footer[8..] == [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
        && u32::from_be_bytes(footer[..4].try_into().unwrap()) != 0
    {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(())
}

pub(crate) fn open(path: &Path, write: bool) -> Result<Connection, Failure> {
    crate::filesystem::regular(path)?;
    crate::filesystem::check_raw_family_size(path)?;
    refuse_external_journal(path)?;
    let connection = Connection::open_with_flags(
        path,
        if write {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        },
    )?;
    connection.busy_timeout(Duration::from_millis(100))?;
    if write {
        connection.execute_batch("PRAGMA synchronous=EXTRA; PRAGMA fullfsync=ON;")?;
    }
    Ok(connection)
}
/// Initializes a file exclusively reserved by the caller, without creating a missing path.
pub(crate) fn initialize(
    path: &Path,
    configuration: &Configuration,
    payload: &[u8],
) -> Result<Snapshot, Failure> {
    let mut connection = open(path, true)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(STORE_SCHEMA)?;
    transaction.execute_batch("PRAGMA user_version=1;")?;
    let nonce = Uuid::new_v4().to_string();
    let checksum = digest(payload);
    let changed = transaction.execute(
        "INSERT INTO store_state VALUES(1,?1,?2,?3,?4)",
        rusqlite::params![configuration.owner, nonce, payload, checksum],
    )?;
    let snapshot = Snapshot {
        generation: Some(generation(&configuration.owner, &nonce, &checksum)),
        payload: Some(payload.to_vec()),
    };
    accepted_write(&transaction, configuration, &snapshot, changed)?;
    transaction.commit()?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(snapshot)
}

/// Check the stored envelope inside the write transaction before acknowledging it.
pub(crate) fn accepted_write(
    connection: &Connection,
    configuration: &Configuration,
    expected: &Snapshot,
    changed: usize,
) -> Result<(), Failure> {
    if changed != 1 || read(connection, configuration)? != *expected {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(())
}

pub(crate) fn read(
    connection: &Connection,
    configuration: &Configuration,
) -> Result<Snapshot, Failure> {
    if connection.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? != 1 {
        return Err(Failure::new(ErrorCode::UnsupportedFormatVersion));
    }
    // Reject any additional schema object without copying untrusted SQL text into Rust.
    let mut schema = connection.prepare(
        "SELECT type='table' AND name='store_state' AND sql=?1 FROM sqlite_master LIMIT 2",
    )?;
    let mut rows = schema.query([STORE_SCHEMA])?;
    if rows
        .next()?
        .map(|row| row.get::<_, Option<bool>>(0))
        .transpose()?
        .flatten()
        != Some(true)
        || rows.next()?.is_some()
    {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    let count: i64 = connection.query_row("SELECT count(*) FROM store_state", [], |r| r.get(0))?;
    if count != 1 {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    // Compare ownership without materializing it. Limit UUID text to its longest
    // accepted representation (urn:uuid: plus a hyphenated UUID). Count characters
    // independently of the database encoding and reject NUL, which truncates length().
    let mut envelope = connection.prepare(
        "SELECT CASE WHEN typeof(owner)='text' THEN owner=?1 END,
                CASE WHEN length(generation)<=45 AND instr(generation,char(0))=0 THEN generation END,
                length(payload),
                CASE WHEN length(digest)=64 AND instr(digest,char(0))=0 THEN digest END
         FROM store_state WHERE id=1",
    )?;
    let mut rows = envelope.query([&configuration.owner])?;
    let row = rows
        .next()?
        .ok_or_else(|| Failure::new(ErrorCode::InvalidStoredData))?;
    match row.get::<_, Option<bool>>(0)? {
        Some(true) => {}
        Some(false) => return Err(Failure::new(ErrorCode::OwnerMismatch)),
        None => return Err(Failure::new(ErrorCode::InvalidStoredData)),
    }
    let (nonce, size, checksum): (String, i64, String) = (row.get(1)?, row.get(2)?, row.get(3)?);
    if Uuid::parse_str(&nonce).is_err()
        || size < 0
        || size as u64 > configuration.maximum_payload_bytes as u64
    {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    let payload: Vec<u8> =
        connection.query_row("SELECT payload FROM store_state WHERE id=1", [], |r| {
            r.get(0)
        })?;
    if digest(&payload) != checksum {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    Ok(Snapshot {
        generation: Some(generation(&configuration.owner, &nonce, &checksum)),
        payload: Some(payload),
    })
}
pub(crate) fn verified(path: &Path, configuration: &Configuration) -> Result<Snapshot, Failure> {
    let mut connection = open(path, false)?;
    verified_connection(&mut connection, configuration)
}

/// Only for a disposable inspection copy: recovery may consume its copied sidecars.
pub(crate) fn verified_inspection_copy(
    path: &Path,
    configuration: &Configuration,
) -> Result<Snapshot, Failure> {
    let mut connection = open(path, true)?;
    verified_connection(&mut connection, configuration)
}

fn verified_connection(
    connection: &mut Connection,
    configuration: &Configuration,
) -> Result<Snapshot, Failure> {
    let transaction = connection.transaction()?;
    let status: String = transaction.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if status != "ok" {
        return Err(Failure::new(ErrorCode::InvalidStoredData));
    }
    let snapshot = read(&transaction, configuration)?;
    transaction.commit()?;
    Ok(snapshot)
}
/// Copies into an empty regular file exclusively reserved by the caller.
pub(crate) fn copy(source: &Path, target: &Path) -> Result<(), Failure> {
    let source = open(source, false)?;
    crate::filesystem::regular(target)?;
    let mut destination = Connection::open(target)?;
    destination.execute_batch("PRAGMA synchronous=EXTRA; PRAGMA fullfsync=ON;")?;
    {
        let backup = Backup::new(&source, &mut destination)?;
        loop {
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => return Err(Failure::new(ErrorCode::Busy)),
                _ => return Err(Failure::new(ErrorCode::IoFailure)),
            }
        }
    }
    destination.close().map_err(|(_, error)| error)?;
    crate::filesystem::sync_file(target)?;
    Ok(())
}
