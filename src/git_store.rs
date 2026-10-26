//! libgit2's object interface backed by redb. No filesystem repository is opened
//! for virtual operations. Each callback owns an Arc, contains panics, and uses
//! libgit2's allocator for buffers transferred across the C ABI.
use anyhow::{Context, Result, ensure};
use git2::{Binding, ObjectType, Odb, Oid};
use libgit2_sys as raw;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::{ffi::c_void, io::Read, sync::Arc};

pub const OBJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("git_objects_v1");
pub const MEMBERS: TableDefinition<&str, &[u8]> = TableDefinition::new("git_members_v1");

pub fn initialize(db: &Database) -> Result<()> {
    let tx = db.begin_write()?;
    tx.open_table(OBJECTS)?;
    tx.open_table(MEMBERS)?;
    tx.commit()?;
    Ok(())
}

pub fn read(db: &Database, oid: Oid) -> Result<(ObjectType, Vec<u8>)> {
    let tx = db.begin_read()?;
    let table = tx.open_table(OBJECTS)?;
    let stored = table
        .get(oid.to_string().as_str())?
        .context("Git object missing from redb")?;
    let data = stored.value();
    ensure!(data.len() >= 9, "invalid Git object record");
    let kind = ObjectType::from_raw(data[0] as raw::git_object_t).context("invalid object type")?;
    let len = u64::from_le_bytes(data[1..9].try_into()?);
    let mut bytes = Vec::new();
    zstd::stream::read::Decoder::new(&data[9..])?
        .take(len.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == len && Oid::hash_object(kind, &bytes)? == oid,
        "Git object integrity failure"
    );
    Ok((kind, bytes))
}

pub fn write(
    db: &Database,
    repo: &str,
    oid: Oid,
    kind: ObjectType,
    bytes: &[u8],
    level: i32,
) -> Result<()> {
    let tx = db.begin_write()?;
    write_record(&tx, repo, oid, kind, bytes, level)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn write_record(
    tx: &redb::WriteTransaction,
    repo: &str,
    oid: Oid,
    kind: ObjectType,
    bytes: &[u8],
    level: i32,
) -> Result<()> {
    ensure!(
        Oid::hash_object(kind, bytes)? == oid,
        "Git object hash mismatch"
    );
    {
        let mut table = tx.open_table(OBJECTS)?;
        let key = oid.to_string();
        let exists = table.get(key.as_str())?.is_some();
        if !exists {
            let mut encoded = vec![kind.raw() as u8];
            encoded.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            encoded.extend(zstd::stream::encode_all(bytes, level)?);
            table.insert(key.as_str(), encoded.as_slice())?;
        }
        tx.open_table(MEMBERS)?
            .insert(format!("{repo}/{key}").as_str(), b"".as_slice())?;
    }
    Ok(())
}

pub fn contains(db: &Database, repo: &str, oid: Oid) -> Result<bool> {
    let tx = db.begin_read()?;
    Ok(tx
        .open_table(MEMBERS)?
        .get(format!("{repo}/{oid}").as_str())?
        .is_some())
}

pub fn members(db: &Database, repo: &str) -> Result<Vec<Oid>> {
    let prefix = format!("{repo}/");
    let tx = db.begin_read()?;
    let table = tx.open_table(MEMBERS)?;
    let mut ids = Vec::new();
    for item in table.range(prefix.as_str()..)? {
        let (k, _) = item?;
        if !k.value().starts_with(&prefix) {
            break;
        }
        ids.push(Oid::from_str(&k.value()[prefix.len()..])?);
    }
    Ok(ids)
}

#[repr(C)]
struct Backend {
    raw: raw::git_odb_backend,
    db: Arc<Database>,
    repo: String,
    level: i32,
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
                    c"redb Git object operation failed".as_ptr(),
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
        let tx = b.db.begin_read()?;
        if tx
            .open_table(MEMBERS)?
            .get(format!("{}/{id}", b.repo).as_str())?
            .is_none()
        {
            return Ok(raw::GIT_ENOTFOUND);
        }
        drop(tx);
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
        let tx = b.db.begin_read()?;
        if tx
            .open_table(MEMBERS)?
            .get(format!("{}/{id}", b.repo).as_str())?
            .is_none()
        {
            return Ok(raw::GIT_ENOTFOUND);
        }
        let table = tx.open_table(OBJECTS)?;
        let Some(value) = table.get(id.to_string().as_str())? else {
            return Ok(raw::GIT_ENOTFOUND);
        };
        let data = value.value();
        ensure!(data.len() >= 9, "invalid Git object record");
        unsafe {
            *len = usize::try_from(u64::from_le_bytes(data[1..9].try_into()?))?;
            *kind = data[0] as raw::git_object_t;
        }
        Ok(0)
    })
}
extern "C" fn exists_cb(backend: *mut raw::git_odb_backend, oid: *const raw::git_oid) -> i32 {
    callback(|| {
        let b = unsafe { &*(backend as *const Backend) };
        let id = unsafe { Oid::from_raw(oid) };
        let tx = b.db.begin_read()?;
        Ok(i32::from(
            tx.open_table(MEMBERS)?
                .get(format!("{}/{id}", b.repo).as_str())?
                .is_some(),
        ))
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
pub fn odb(db: Arc<Database>, repo: &str, level: i32) -> Result<Odb<'static>> {
    let odb = Odb::new()?;
    let mut b = Box::new(Backend {
        raw: unsafe { std::mem::zeroed() },
        db,
        repo: repo.into(),
        level,
    });
    ensure!(
        unsafe { raw::git_odb_init_backend(&mut b.raw, raw::GIT_ODB_BACKEND_VERSION) } == 0,
        "initialize redb backend"
    );
    b.raw.read = Some(read_cb);
    b.raw.read_header = Some(header_cb);
    b.raw.exists = Some(exists_cb);
    b.raw.write = Some(write_cb);
    b.raw.free = Some(free_cb);
    let ptr = Box::into_raw(b);
    let code = unsafe { raw::git_odb_add_backend(odb.raw(), &mut (*ptr).raw, 1) };
    if code < 0 {
        unsafe {
            drop(Box::from_raw(ptr));
        }
        anyhow::bail!("attach redb Git backend");
    }
    Ok(odb)
}
