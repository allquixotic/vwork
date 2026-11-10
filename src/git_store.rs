//! libgit2's object interface backed by Fjall. No filesystem repository is opened
//! for virtual operations. Each callback owns an Arc, contains panics, and uses
//! libgit2's allocator for buffers transferred across the C ABI.
use crate::kv::{Database, TableDefinition};
use anyhow::{Context, Result, ensure};
use git2::{Binding, ObjectType, Odb, Oid};
use libgit2_sys as raw;
use std::{ffi::c_void, sync::Arc};

pub const OBJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("git_objects_v1");
pub const HEADERS: TableDefinition<&str, &[u8]> = TableDefinition::new("git_headers_v1");
pub const OVERLAYS: TableDefinition<&str, &[u8]> = TableDefinition::new("git_overlay_members_v1");
pub const MEMBERS: TableDefinition<&str, &[u8]> = TableDefinition::new("git_members_v1");

pub fn initialize(db: &Database) -> Result<()> {
    let mut missing = false;
    {
        let tx = db.begin_read()?;
        for table in [OBJECTS, HEADERS, MEMBERS, OVERLAYS, CANDIDATES] {
            match tx.open_table(table) {
                Ok(_) => {}
                Err(crate::kv::TableError::TableDoesNotExist(_)) => missing = true,
                Err(error) => return Err(error.into()),
            }
        }
    }
    if !missing {
        return Ok(());
    }
    let tx = db.begin_write()?;
    tx.open_table(OBJECTS)?;
    tx.open_table(HEADERS)?;
    tx.open_table(MEMBERS)?;
    tx.open_table(OVERLAYS)?;
    tx.open_table(CANDIDATES)?;
    tx.commit()?;
    Ok(())
}

pub const CANDIDATES: TableDefinition<&str, &[u8]> =
    TableDefinition::new("git_delta_candidates_v1");

/// The fixed header keeps libgit2 read_header independent of delta expansion.
/// kind:u8, reconstructed length:u64 LE, encoding:u8, depth:u8, payload.
/// Delta payload: base Git OID (20 bytes), standard Git delta instructions.
pub fn raw_record(kind: ObjectType, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![kind.raw() as u8];
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(bytes);
    out
}
pub fn delta_base(data: &[u8]) -> Result<Option<Oid>> {
    ensure!(data.len() >= 11, "invalid Git object record");
    match data[9] {
        0 => {
            ensure!(data[10] == 0, "invalid raw Git object depth");
            Ok(None)
        }
        1 => {
            ensure!(
                data.len() >= 31 && (1..=crate::git_delta::MAX_DEPTH).contains(&data[10]),
                "invalid Git delta header"
            );
            Ok(Some(Oid::from_bytes(&data[11..31])?))
        }
        _ => anyhow::bail!("unknown Git object representation"),
    }
}
/// Small immutable metadata stays in index blocks, separate from large values.
/// It includes the delta base and encoded length, so header reads, deduplication,
/// stats and GC never fetch or inflate object payloads just to inspect them.
fn metadata(record: &[u8]) -> Result<Vec<u8>> {
    let n = if delta_base(record)?.is_some() {
        31
    } else {
        11
    };
    let mut value = record[..n].to_vec();
    value.extend_from_slice(&(record.len() as u64).to_le_bytes());
    Ok(value)
}
pub fn encoded_len(header: &[u8]) -> Result<u64> {
    let n = if delta_base(header)?.is_some() {
        31
    } else {
        11
    };
    ensure!(header.len() == n + 8, "invalid Git metadata length");
    Ok(u64::from_le_bytes(header[n..].try_into()?))
}
pub fn read(db: &Database, oid: Oid) -> Result<(ObjectType, Vec<u8>)> {
    let tx = db.begin_read()?;
    let table = tx.open_table(OBJECTS)?;
    let headers = tx.open_table(HEADERS)?;
    let mut chain = Vec::new();
    let mut current = oid;
    loop {
        ensure!(
            chain.len() <= crate::git_delta::MAX_DEPTH as usize,
            "Git delta chain too deep"
        );
        ensure!(
            !chain.iter().any(|(id, _)| *id == current),
            "cyclic Git delta"
        );
        let record = table
            .get(current.to_string().as_str())?
            .context("Git object missing from store")?
            .value()
            .to_vec();
        let base = delta_base(&record)?;
        ensure!(
            headers
                .get(current.to_string().as_str())?
                .context("Git metadata missing")?
                .value()
                == metadata(&record)?,
            "Git metadata mismatch"
        );
        chain.push((current, record));
        if let Some(base) = base {
            current = base;
        } else {
            break;
        }
    }
    let mut bytes = Vec::new();
    let mut previous_depth = 0;
    let mut previous_kind = None;
    for (id, record) in chain.iter().rev() {
        let kind =
            ObjectType::from_raw(record[0] as raw::git_object_t).context("invalid object type")?;
        let len = usize::try_from(u64::from_le_bytes(record[1..9].try_into()?))?;
        ensure!(
            len <= crate::git_delta::MAX_OBJECT_BYTES,
            "Git object exceeds decoding limit"
        );
        bytes = if record[9] == 0 {
            record[11..].to_vec()
        } else {
            ensure!(
                previous_kind == Some(kind) && record[10] == previous_depth + 1,
                "Git delta ancestry mismatch"
            );
            crate::git_delta::apply(&bytes, &record[31..], len)?
        };
        ensure!(
            bytes.len() == len && Oid::hash_object(kind, &bytes)? == *id,
            "Git object integrity failure"
        );
        previous_depth = record[10];
        previous_kind = Some(kind);
    }
    Ok((previous_kind.context("missing object")?, bytes))
}
fn candidate_prefixes(kind: ObjectType, bytes: &[u8]) -> Vec<String> {
    if bytes.len() < 256 || bytes.len() > crate::git_delta::MAX_DELTA_BYTES {
        return vec![];
    }
    let bucket = usize::BITS - bytes.len().leading_zeros();
    [0, bytes.len() / 2, bytes.len() - 64]
        .into_iter()
        .map(|offset| {
            let hash = blake3::hash(&bytes[offset..offset + 64]);
            format!("{}/{bucket}/{}/", kind.raw(), &hash.to_hex()[..16])
        })
        .collect()
}
/// Bounded transaction-local candidates let a bulk import delta-compress its
/// first batch without reading mutable shared candidate ranges in the OCC set.
#[derive(Default)]
pub(crate) struct GitBatch {
    known: std::collections::HashSet<Oid>,
    candidates: std::collections::VecDeque<(Oid, ObjectType, u8, Vec<u8>)>,
    bytes: usize,
}
impl GitBatch {
    fn remember(&mut self, oid: Oid, kind: ObjectType, record: &[u8], bytes: &[u8]) {
        self.known.insert(oid);
        if bytes.len() < 256 || bytes.len() > 4 * 1024 * 1024 {
            return;
        }
        while self.candidates.len() >= 8 || self.bytes + bytes.len() > 16 * 1024 * 1024 {
            if let Some((_, _, _, old)) = self.candidates.pop_front() {
                self.bytes -= old.len();
            } else {
                break;
            }
        }
        self.bytes += bytes.len();
        self.candidates
            .push_back((oid, kind, record[10], bytes.to_vec()));
    }
    fn improve(&self, kind: ObjectType, bytes: &[u8], best: &mut Vec<u8>) {
        let hints = candidate_prefixes(kind, bytes);
        if hints.is_empty() {
            return;
        }
        for (base, base_kind, depth, source) in self
            .candidates
            .iter()
            .rev()
            .filter(|(_, k, d, _)| *k == kind && *d < crate::git_delta::MAX_DEPTH)
            .take(4)
        {
            debug_assert_eq!(kind, *base_kind);
            if !candidate_prefixes(kind, source)
                .iter()
                .any(|hint| hints.contains(hint))
            {
                continue;
            }
            improve(*base, *depth, source, bytes, best);
        }
    }
}
fn improve(base: Oid, depth: u8, source: &[u8], bytes: &[u8], best: &mut Vec<u8>) {
    if let Some(delta) = crate::git_delta::encode(source, bytes)
        && delta.len() + 31 < best.len()
    {
        best.truncate(11);
        best[1..9].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        best[9] = 1;
        best[10] = depth + 1;
        best.extend_from_slice(base.as_bytes());
        best.extend(delta);
    }
}
fn prepare_in(
    tx: &crate::kv::WriteTransaction<'_>,
    oid: Oid,
    kind: ObjectType,
    bytes: &[u8],
) -> Result<Option<Vec<u8>>> {
    if tx.git_batch.borrow().known.contains(&oid) {
        return Ok(None);
    }
    let mut prepared = prepare(tx.database(), oid, kind, bytes)?;
    if let Some(record) = prepared.as_mut() {
        tx.git_batch.borrow().improve(kind, bytes, record);
        tx.git_batch.borrow_mut().remember(oid, kind, record, bytes);
    }
    Ok(prepared)
}
fn prepare(db: &Database, oid: Oid, kind: ObjectType, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    ensure!(
        bytes.len() <= crate::git_delta::MAX_OBJECT_BYTES,
        "Git object exceeds storage limit"
    );
    ensure!(
        Oid::hash_object(kind, bytes)? == oid,
        "Git object hash mismatch"
    );
    let tx = db.begin_read()?;
    if tx
        .open_table(HEADERS)?
        .get(oid.to_string().as_str())?
        .is_some()
    {
        return Ok(None);
    }
    prepare_encoding(db, kind, bytes).map(Some)
}
fn prepare_encoding(db: &Database, kind: ObjectType, bytes: &[u8]) -> Result<Vec<u8>> {
    let tx = db.begin_read()?;
    let mut best = raw_record(kind, bytes);
    if let Ok(candidates) = tx.open_table(CANDIDATES) {
        let mut tested = std::collections::BTreeSet::new();
        for prefix in candidate_prefixes(kind, bytes) {
            let end = format!("{prefix}~");
            for row in candidates.range(prefix.as_str()..end.as_str())?.take(4) {
                let (key, _) = row?;
                let base = Oid::from_str(&key.value()[prefix.len()..])?;
                if !tested.insert(base) || tested.len() > 6 {
                    continue;
                }
                let Some(record) = tx.open_table(HEADERS)?.get(base.to_string().as_str())? else {
                    continue;
                };
                let data = record.value();
                ensure!(data.len() >= 11, "invalid Git object header");
                if data[10] >= crate::git_delta::MAX_DEPTH {
                    continue;
                }
                let depth = data[10];
                let (_, source) = read(db, base)?;
                improve(base, depth, &source, bytes, &mut best);
            }
        }
    }
    Ok(best)
}
/// Migration-only repack. Call with exclusive destination ownership.
pub fn repack(db: &Database, mut progress: impl FnMut() -> Result<()>) -> Result<()> {
    let read_tx = db.begin_read()?;
    let objects = read_tx.open_table(OBJECTS)?;
    let mut batch = db.begin_write()?;
    let mut count = 0;
    let mut weight = 0;
    for row in objects.iter()? {
        let (key, record) = row?;
        let oid = Oid::from_str(key.value())?;
        let done = format!("done/{oid}");
        if db
            .begin_read()?
            .open_table(CANDIDATES)?
            .get(done.as_str())?
            .is_some()
        {
            // Older interrupted staging may already have its encoding marker.
            // Fill metadata without choosing a new encoding for an existing base.
            if db
                .begin_read()?
                .open_table(HEADERS)?
                .get(key.value())?
                .is_none()
            {
                db.transaction(|tx| {
                    tx.open_table(HEADERS)?
                        .insert(key.value(), &metadata(record.value())?)
                })?;
            }
            progress()?;
            continue;
        }
        let data = record.value();
        // A copied raw record has no metadata yet. Existing encoded records
        // were published by an earlier committed repack batch.
        let (kind, bytes) = if data[9] == 0 {
            (
                ObjectType::from_raw(data[0] as raw::git_object_t)
                    .context("invalid object type")?,
                data[11..].to_vec(),
            )
        } else {
            read(db, oid)?
        };
        let mut replacement = if data[9] == 0 {
            prepare_encoding(db, kind, &bytes)?
        } else {
            data.to_vec()
        };
        if data[9] == 0 {
            batch
                .git_batch
                .borrow()
                .improve(kind, &bytes, &mut replacement);
        }
        batch
            .git_batch
            .borrow_mut()
            .remember(oid, kind, &replacement, &bytes);
        weight += replacement.len();
        count += 1;
        batch
            .open_table(OBJECTS)?
            .insert(key.value(), &replacement)?;
        batch
            .open_table(HEADERS)?
            .insert(key.value(), &metadata(&replacement)?)?;
        for prefix in candidate_prefixes(kind, &bytes) {
            batch
                .open_table(CANDIDATES)?
                .insert(format!("{prefix}{oid}").as_str(), b"")?;
        }
        batch.open_table(CANDIDATES)?.insert(done.as_str(), b"")?;
        if count >= 128 || weight >= 16 * 1024 * 1024 {
            batch.commit()?;
            batch = db.begin_write()?;
            count = 0;
            weight = 0;
        }
        progress()?;
    }
    batch.commit()?;
    Ok(())
}
pub fn write(
    db: &Database,
    repo: &str,
    oid: Oid,
    kind: ObjectType,
    bytes: &[u8],
    _level: i32,
) -> Result<()> {
    let prepared = prepare(db, oid, kind, bytes)?;
    db.transaction(|tx| insert_prepared(tx, repo, oid, kind, bytes, prepared.as_deref(), MEMBERS))
}
fn insert_prepared(
    tx: &crate::kv::WriteTransaction<'_>,
    repo: &str,
    oid: Oid,
    kind: ObjectType,
    bytes: &[u8],
    prepared: Option<&[u8]>,
    membership: TableDefinition<&str, &[u8]>,
) -> Result<()> {
    let key = oid.to_string();
    if let Some(record) = prepared {
        // Only first publication reads this shared key into the conflict set.
        // Two imports may prepare different delta encodings for the same OID;
        // overwriting a published base could otherwise create a delta cycle.
        let mut headers = tx.open_table(HEADERS)?;
        if headers.get(key.as_str())?.is_none() {
            let mut objects = tx.open_table(OBJECTS)?;
            ensure!(
                objects.get(key.as_str())?.is_none(),
                "Git object metadata missing; existing payload preserved"
            );
            objects.insert(key.as_str(), record)?;
            headers.insert(key.as_str(), &metadata(record)?)?;
            for prefix in candidate_prefixes(kind, bytes) {
                tx.open_table(CANDIDATES)?
                    .insert(format!("{prefix}{key}").as_str(), b"".as_slice())?;
            }
        }
    }
    let member = format!("{repo}/{key}");
    // Membership is monotonic between exclusive GC runs. Avoid rewriting the
    // same hot repository/object key on every conversation's identical edit.
    if tx
        .database()
        .begin_read()?
        .open_table(membership)?
        .get(member.as_str())?
        .is_none()
    {
        tx.open_table(membership)?
            .insert(member.as_str(), b"".as_slice())?;
    }
    Ok(())
}
pub(crate) fn write_record(
    tx: &crate::kv::WriteTransaction<'_>,
    repo: &str,
    oid: Oid,
    kind: ObjectType,
    bytes: &[u8],
    _level: i32,
) -> Result<()> {
    let prepared = prepare_in(tx, oid, kind, bytes)?;
    insert_prepared(tx, repo, oid, kind, bytes, prepared.as_deref(), MEMBERS)
}

/// Reuse an immutable, already verified object without reading its payload
/// from either the source ODB or Fjall. Engine lifecycle guards exclude GC.
/// Membership still needs publication when another repository owns the copy.
pub(crate) fn link_existing(
    tx: &crate::kv::WriteTransaction<'_>,
    repo: &str,
    oid: Oid,
) -> Result<bool> {
    let read = tx.database().begin_read()?;
    let headers = read.open_table(HEADERS)?;
    let Some(header) = headers.get(oid.to_string().as_str())? else {
        return Ok(false);
    };
    encoded_len(header.value())?;
    let member = format!("{repo}/{oid}");
    if read.open_table(MEMBERS)?.get(member.as_str())?.is_none() {
        tx.open_table(MEMBERS)?.insert(member.as_str(), b"")?;
    }
    Ok(true)
}

pub(crate) fn has_object(db: &Database, oid: Oid) -> Result<bool> {
    Ok(db
        .begin_read()?
        .open_table(HEADERS)?
        .get(oid.to_string().as_str())?
        .is_some())
}

/// Overlay objects are reachable through revision roots, not permanent import
/// membership. Publishing their membership and revision is one atomic commit.
pub(crate) fn write_overlay(
    tx: &crate::kv::WriteTransaction<'_>,
    repo: &str,
    oid: Oid,
    bytes: &[u8],
) -> Result<()> {
    let prepared = prepare_in(tx, oid, ObjectType::Blob, bytes)?;
    insert_prepared(
        tx,
        repo,
        oid,
        ObjectType::Blob,
        bytes,
        prepared.as_deref(),
        OVERLAYS,
    )
}
/// A committed Git tree retains its blobs even after its virtual revision pin
/// is forgotten. Promotion adds membership only, never another content copy.
pub(crate) fn retain_objects(
    db: &Database,
    repo: &str,
    oids: impl Iterator<Item = String>,
) -> Result<()> {
    let read = db.begin_read()?;
    let members = read.open_table(MEMBERS)?;
    let objects = read.open_table(HEADERS)?;
    let mut retain = std::collections::BTreeSet::new();
    for oid in oids {
        let key = format!("{repo}/{oid}");
        if members.get(key.as_str())?.is_none() {
            ensure!(
                objects.get(oid.as_str())?.is_some(),
                "Cannot retain missing Git object"
            );
            retain.insert(key);
        }
    }
    if !retain.is_empty() {
        db.transaction(|tx| {
            let mut members = tx.open_table(MEMBERS)?;
            for key in &retain {
                members.insert(key.as_str(), b"")?;
            }
            Ok(())
        })?;
    }
    Ok(())
}
pub fn contains(db: &Database, repo: &str, oid: Oid) -> Result<bool> {
    let tx = db.begin_read()?;
    let key = format!("{repo}/{oid}");
    Ok(tx.open_table(MEMBERS)?.get(key.as_str())?.is_some()
        || tx.open_table(OVERLAYS)?.get(key.as_str())?.is_some())
}

pub fn members(db: &Database, repo: &str) -> Result<Vec<Oid>> {
    let prefix = format!("{repo}/");
    let tx = db.begin_read()?;
    let mut ids = std::collections::BTreeSet::new();
    for definition in [MEMBERS, OVERLAYS] {
        let table = tx.open_table(definition)?;
        for item in table.range(prefix.as_str()..)? {
            let (k, _) = item?;
            if !k.value().starts_with(&prefix) {
                break;
            }
            ids.insert(Oid::from_str(&k.value()[prefix.len()..])?);
        }
    }
    Ok(ids.into_iter().collect())
}

#[repr(C)]
struct Backend {
    raw: raw::git_odb_backend,
    db: Arc<Database>,
    repo: String,
    level: i32,
    enumerate: bool,
}

// libgit2 C enums are signed on Windows and unsigned on Unix.
#[allow(clippy::unnecessary_cast)]
fn callback(f: impl FnOnce() -> Result<i32>) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(code)) => code,
        _ => {
            // Deliberately avoid passing repository data or credential text to C logs.
            unsafe {
                raw::git_error_set_str(
                    raw::GIT_ERROR_ODB as i32,
                    c"Fjall Git object operation failed".as_ptr(),
                );
            }
            -1
        }
    }
}
extern "C" fn read_cb(
    out: *mut *mut c_void,
    len: *mut usize,
    kind: *mut raw::git_object_t,
    backend: *mut raw::git_odb_backend,
    oid: *const raw::git_oid,
) -> i32 {
    callback(|| {
        // SAFETY: libgit2 supplies valid callback arguments; Backend is repr(C)
        // with its ABI header first and remains alive until free_cb.
        let b = unsafe { &*(backend as *const Backend) };
        let id = unsafe { Oid::from_raw(oid) };
        if !contains(&b.db, &b.repo, id)? {
            return Ok(raw::GIT_ENOTFOUND);
        }
        let (object_type, bytes) = read(&b.db, id)?;
        unsafe {
            let allocated = raw::git_odb_backend_data_alloc(backend, bytes.len().max(1));
            ensure!(!allocated.is_null(), "Git object allocation failed");
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated.cast::<u8>(), bytes.len());
            *out = allocated;
            *len = bytes.len();
            *kind = object_type.raw();
        }
        Ok(0)
    })
}
extern "C" fn header_cb(
    len: *mut usize,
    kind: *mut raw::git_object_t,
    backend: *mut raw::git_odb_backend,
    oid: *const raw::git_oid,
) -> i32 {
    callback(|| {
        let b = unsafe { &*(backend as *const Backend) };
        let id = unsafe { Oid::from_raw(oid) };
        if !contains(&b.db, &b.repo, id)? {
            return Ok(raw::GIT_ENOTFOUND);
        }
        let tx = b.db.begin_read()?;
        let table = tx.open_table(HEADERS)?;
        let Some(value) = table.get(id.to_string().as_str())? else {
            return Ok(raw::GIT_ENOTFOUND);
        };
        let data = value.value();
        encoded_len(data)?;
        let object_type =
            ObjectType::from_raw(data[0] as raw::git_object_t).context("invalid object type")?;
        let object_len = usize::try_from(u64::from_le_bytes(data[1..9].try_into()?))?;
        ensure!(
            object_len <= crate::git_delta::MAX_OBJECT_BYTES,
            "Git object exceeds decoding limit"
        );
        unsafe {
            *len = object_len;
            *kind = object_type.raw();
        }
        Ok(0)
    })
}
extern "C" fn exists_cb(backend: *mut raw::git_odb_backend, oid: *const raw::git_oid) -> i32 {
    callback(|| {
        let b = unsafe { &*(backend as *const Backend) };
        let id = unsafe { Oid::from_raw(oid) };
        Ok(i32::from(contains(&b.db, &b.repo, id)?))
    })
}
extern "C" fn write_cb(
    backend: *mut raw::git_odb_backend,
    oid: *const raw::git_oid,
    data: *const c_void,
    len: usize,
    kind: raw::git_object_t,
) -> i32 {
    callback(|| {
        let b = unsafe { &*(backend as *const Backend) };
        let id = unsafe { Oid::from_raw(oid) };
        let bytes = if len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) }
        };
        write(
            &b.db,
            &b.repo,
            id,
            ObjectType::from_raw(kind).context("invalid object type")?,
            bytes,
            b.level,
        )?;
        Ok(0)
    })
}
extern "C" fn free_cb(backend: *mut raw::git_odb_backend) {
    // SAFETY: sole ownership transfers to libgit2 on successful add_backend.
    unsafe {
        drop(Box::from_raw(backend as *mut Backend));
    }
}
extern "C" fn foreach_cb(
    backend: *mut raw::git_odb_backend,
    cb: raw::git_odb_foreach_cb,
    payload: *mut c_void,
) -> i32 {
    callback(|| {
        let b = unsafe { &*(backend as *const Backend) };
        if b.enumerate {
            for oid in members(&b.db, &b.repo)? {
                let code = cb.context("missing Git enumeration callback")?(oid.raw(), payload);
                if code != 0 {
                    return Ok(code);
                }
            }
        }
        Ok(0)
    })
}
pub fn odb(db: Arc<Database>, repo: &str, level: i32) -> Result<Odb<'static>> {
    let odb = Odb::new()?;
    attach(&odb, db, repo, level, false)?;
    Ok(odb)
}

/// Read-through backend for a temporary fetch receiver. Existing Fjall objects
/// satisfy negotiation and thin-pack bases; enumeration/import visits new disk
/// packs only. libgit2 writes received packs to its normal filesystem backend.
pub fn attach_readonly(odb: &Odb<'_>, db: Arc<Database>, repo: &str) -> Result<()> {
    attach(odb, db, repo, 3, true)
}
fn attach(odb: &Odb<'_>, db: Arc<Database>, repo: &str, level: i32, readonly: bool) -> Result<()> {
    let mut b = Box::new(Backend {
        raw: unsafe { std::mem::zeroed() },
        db,
        repo: repo.into(),
        level,
        enumerate: !readonly,
    });
    ensure!(
        unsafe { raw::git_odb_init_backend(&mut b.raw, raw::GIT_ODB_BACKEND_VERSION) } == 0,
        "initialize Fjall backend"
    );
    b.raw.read = Some(read_cb);
    b.raw.read_header = Some(header_cb);
    b.raw.exists = Some(exists_cb);
    b.raw.write = if readonly { None } else { Some(write_cb) };
    b.raw.foreach = Some(foreach_cb);
    b.raw.free = Some(free_cb);
    let ptr = Box::into_raw(b);
    let code = unsafe { raw::git_odb_add_backend(odb.raw(), &mut (*ptr).raw, 100) };
    if code < 0 {
        unsafe {
            drop(Box::from_raw(ptr));
        }
        anyhow::bail!("attach Fjall Git backend");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v36_headers_and_key_scans_do_not_load_object_payloads() -> Result<()> {
        let root = tempfile::tempdir()?;
        let db = Arc::new(Database::open(&root.path().join("store"))?);
        initialize(&db)?;
        let bytes = vec![42; 1024 * 1024];
        let oid = Oid::hash_object(ObjectType::Blob, &bytes)?;
        write(&db, "repo", oid, ObjectType::Blob, &bytes, 0)?;
        // Deliberately incompatible value codec proves key-only scans/counts
        // do not retrieve or decode values. Fjall's key guard skips blob reads.
        let keys_only: TableDefinition<&str, bool> = TableDefinition::new(OBJECTS.name());
        let tx = db.begin_read()?;
        let table = tx.open_table(keys_only)?;
        assert_eq!(table.len()?, 1);
        assert_eq!(table.keys()?.next().unwrap()?.value(), oid.to_string());
        assert!(table.get(oid.to_string().as_str()).is_err());
        drop(table);
        drop(tx);
        db.transaction(|tx| tx.open_table(OBJECTS)?.delete(oid.to_string().as_str()))?;
        // A fresh libgit2 ODB has no cached header. Metadata remains readable
        // even when the payload is unavailable; actual object reads fail closed.
        let odb = Odb::new()?;
        attach_readonly(&odb, db.clone(), "repo")?;
        assert_eq!(odb.read_header(oid)?, (bytes.len(), ObjectType::Blob));
        assert!(read(&db, oid).is_err());
        let header = db
            .begin_read()?
            .open_table(HEADERS)?
            .get(oid.to_string().as_str())?
            .unwrap();
        assert_eq!(header.value().len(), 19);
        assert_eq!(encoded_len(header.value())?, bytes.len() as u64 + 11);
        Ok(())
    }

    #[test]
    fn v35_duplicate_publication_cannot_rewrite_a_delta_base() -> Result<()> {
        let root = tempfile::tempdir()?;
        let db = Database::open(&root.path().join("store"))?;
        initialize(&db)?;
        let a = "shared source line with stable content\n"
            .repeat(2000)
            .into_bytes();
        let mut b = a.clone();
        b[120..124].copy_from_slice(b"edit");
        let a_id = Oid::hash_object(ObjectType::Blob, &a)?;
        let b_id = Oid::hash_object(ObjectType::Blob, &b)?;
        let older = db.begin_write()?;
        write_record(&older, "first", a_id, ObjectType::Blob, &a, 0)?;
        write_record(&older, "first", b_id, ObjectType::Blob, &b, 0)?;
        // The first import has prepared A(raw), B(delta A), but has not committed.
        // A second import publishes B(raw), then prepares A(delta B).
        write(&db, "second", b_id, ObjectType::Blob, &b, 0)?;
        let newer = db.begin_write()?;
        write_record(&newer, "second", a_id, ObjectType::Blob, &a, 0)?;
        // Blindly accepting both commits would leave A -> B -> A. The losing
        // first-publication transaction must conflict, preserving B's encoding.
        assert!(older.commit().unwrap_err().is::<crate::kv::Conflict>());
        newer.commit()?;
        assert_eq!(read(&db, a_id)?.1, a);
        assert_eq!(read(&db, b_id)?.1, b);
        assert_eq!(db.begin_read()?.open_table(OBJECTS)?.len()?, 2);
        Ok(())
    }
}
