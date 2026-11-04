//! Typed tables over Fjall's optimistic, serializable transactions.
//! No database-wide writer lock or application compression. Content keys are
//! immutable hashes; mutable state uses workspace/conversation-specific keys.
use anyhow::{Result, ensure};
use fjall::{OptimisticTxDatabase, OptimisticTxKeyspace, Readable};
use std::{
    cell::RefCell,
    marker::PhantomData,
    ops::{Bound, RangeBounds},
    path::Path,
};

pub use fjall;
mod journal;
pub use journal::{DURABILITY_LIMIT, Status as DurabilityStatus, statuses as durability_statuses};

pub trait Codec {
    type Input<'a>: Copy;
    fn encode(value: Self::Input<'_>) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<Self::Input<'_>>;
}
impl Codec for &str {
    type Input<'a> = &'a str;
    fn encode(value: &str) -> Vec<u8> {
        value.as_bytes().to_vec()
    }
    fn decode(bytes: &[u8]) -> Result<&str> {
        Ok(std::str::from_utf8(bytes)?)
    }
}
impl Codec for &[u8] {
    type Input<'a> = &'a [u8];
    fn encode(value: &[u8]) -> Vec<u8> {
        value.to_vec()
    }
    fn decode(bytes: &[u8]) -> Result<&[u8]> {
        Ok(bytes)
    }
}
impl Codec for u64 {
    type Input<'a> = u64;
    fn encode(value: u64) -> Vec<u8> {
        value.to_be_bytes().to_vec()
    }
    fn decode(bytes: &[u8]) -> Result<u64> {
        Ok(u64::from_be_bytes(bytes.try_into()?))
    }
}
impl Codec for bool {
    type Input<'a> = bool;
    fn encode(value: bool) -> Vec<u8> {
        vec![u8::from(value)]
    }
    fn decode(bytes: &[u8]) -> Result<bool> {
        ensure!(matches!(bytes, [0] | [1]), "invalid stored boolean");
        Ok(bytes[0] != 0)
    }
}

#[derive(Clone, Copy)]
pub struct TableDefinition<K, V> {
    name: &'static str,
    types: PhantomData<(K, V)>,
}
impl<K, V> TableDefinition<K, V> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            types: PhantomData,
        }
    }
    pub fn name(&self) -> &str {
        self.name
    }
}

#[derive(Debug)]
pub enum TableError {
    TableDoesNotExist(String),
    Storage(anyhow::Error),
}
impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TableDoesNotExist(name) => write!(f, "table does not exist: {name}"),
            Self::Storage(e) => write!(f, "{e:#}"),
        }
    }
}
impl std::error::Error for TableError {}

pub struct AccessGuard<T: Codec> {
    bytes: fjall::Slice,
    types: PhantomData<T>,
}
impl<T: Codec> AccessGuard<T> {
    fn new(bytes: fjall::Slice) -> Result<Self> {
        T::decode(&bytes)?;
        Ok(Self {
            bytes,
            types: PhantomData,
        })
    }
    pub fn value(&self) -> T::Input<'_> {
        T::decode(&self.bytes).expect("validated value")
    }
}

pub struct Database {
    pub(crate) inner: OptimisticTxDatabase,
    journal: journal::Journal,
}
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let inner = OptimisticTxDatabase::builder(path)
            .manual_journal_persist(true)
            .cache_size(64 * 1024 * 1024)
            .max_journaling_size(128 * 1024 * 1024)
            .journal_compression(fjall::CompressionType::Lz4)
            .worker_threads(2)
            .open()?;
        let journal = journal::Journal::start(inner.clone())?;
        Ok(Self { inner, journal })
    }
    pub(crate) fn keyspace(&self, name: &str) -> Result<OptimisticTxKeyspace> {
        Ok(self.inner.keyspace(name, || {
            fjall::KeyspaceCreateOptions::default()
                .max_memtable_size(8 * 1024 * 1024)
                .with_kv_separation(Some(
                    fjall::KvSeparationOptions::default()
                        .separation_threshold(4096)
                        .compression(fjall::CompressionType::Lz4),
                ))
                .data_block_compression_policy(fjall::config::CompressionPolicy::all(
                    fjall::CompressionType::Lz4,
                ))
                .index_block_compression_policy(fjall::config::CompressionPolicy::all(
                    fjall::CompressionType::Lz4,
                ))
        })?)
    }
    pub fn begin_read(&self) -> Result<ReadTransaction<'_>> {
        Ok(ReadTransaction {
            db: self,
            snapshot: self.inner.read_tx(),
        })
    }
    pub fn begin_write(&self) -> Result<WriteTransaction<'_>> {
        self.journal.check_write()?;
        Ok(WriteTransaction {
            db: self,
            immediate: false,
            git_batch: RefCell::new(Default::default()),
            inner: RefCell::new(Some(self.inner.write_tx()?.durability(None))),
        })
    }
    pub fn disk_space(&self) -> Result<u64> {
        Ok(self.inner.disk_space()?)
    }
    pub fn persist(&self) -> Result<()> {
        self.journal.persist(&self.inner)
    }
    pub fn durability_status(&self) -> DurabilityStatus {
        self.journal.status()
    }
    pub fn compact(&self) -> Result<()> {
        self.persist()?;
        for name in self.inner.list_keyspace_names() {
            let space = self.keyspace(&name)?;
            space.inner().rotate_memtable_and_wait()?;
            space.inner().major_compact()?;
        }
        self.persist()
    }
    /// Retry only pure storage closures. Never replay tools or external effects.
    pub fn transaction<T>(
        &self,
        mut f: impl FnMut(&WriteTransaction<'_>) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..16 {
            let tx = self.begin_write()?;
            let value = f(&tx)?;
            match tx.commit() {
                Ok(()) => return Ok(value),
                Err(e) if e.is::<Conflict>() && attempt < 15 => std::thread::yield_now(),
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        // Explicit shutdown calls persist and reports errors. Drop is also a
        // final best-effort barrier for CLI/tests and exceptional teardown.
        if let Err(error) = self.persist() {
            eprintln!("Workspace shutdown sync failed: {error}");
        }
    }
}

#[derive(Debug)]
pub struct Conflict;
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("revision conflict: concurrent storage change; refresh and retry")
    }
}
impl std::error::Error for Conflict {}

pub struct ReadTransaction<'a> {
    db: &'a Database,
    snapshot: fjall::Snapshot,
}
impl ReadTransaction<'_> {
    pub fn open_table<K: Codec, V: Codec>(
        &self,
        definition: TableDefinition<K, V>,
    ) -> std::result::Result<Table<'_, K, V>, TableError> {
        if !self.db.inner.keyspace_exists(definition.name) {
            return Err(TableError::TableDoesNotExist(definition.name.into()));
        }
        let space = self
            .db
            .keyspace(definition.name)
            .map_err(TableError::Storage)?;
        Ok(Table {
            space,
            source: Source::Read(&self.snapshot),
            types: PhantomData,
        })
    }
}
pub enum Durability {
    None,
    Immediate,
}
pub struct WriteTransaction<'a> {
    db: &'a Database,
    immediate: bool,
    pub(crate) git_batch: RefCell<crate::git_store::GitBatch>,
    inner: RefCell<Option<fjall::OptimisticWriteTx>>,
}
impl WriteTransaction<'_> {
    pub fn database(&self) -> &Database {
        self.db
    }
    pub fn open_table<K: Codec, V: Codec>(
        &self,
        definition: TableDefinition<K, V>,
    ) -> Result<Table<'_, K, V>> {
        Ok(Table {
            space: self.db.keyspace(definition.name)?,
            source: Source::Write(&self.inner),
            types: PhantomData,
        })
    }
    pub fn set_durability(&mut self, durability: Durability) -> Result<()> {
        self.immediate = matches!(durability, Durability::Immediate);
        Ok(())
    }
    pub fn commit(self) -> Result<()> {
        self.db.journal.check_write()?;
        self.inner
            .into_inner()
            .expect("live transaction")
            .commit()?
            .map_err(|_| anyhow::Error::from(Conflict))?;
        self.db.journal.committed();
        if self.immediate {
            self.db.persist()?;
        }
        Ok(())
    }
    pub fn abort(self) -> Result<()> {
        Ok(())
    }
}
enum Source<'a> {
    Read(&'a fjall::Snapshot),
    Write(&'a RefCell<Option<fjall::OptimisticWriteTx>>),
}
pub struct Table<'a, K: Codec, V: Codec> {
    space: OptimisticTxKeyspace,
    source: Source<'a>,
    types: PhantomData<(K, V)>,
}
impl<K: Codec, V: Codec> Table<'_, K, V> {
    pub fn get(&self, key: K::Input<'_>) -> Result<Option<AccessGuard<V>>> {
        let key = K::encode(key);
        let value = match &self.source {
            Source::Read(tx) => tx.get(&self.space, key)?,
            Source::Write(tx) => tx
                .borrow()
                .as_ref()
                .expect("live transaction")
                .get(&self.space, key)?,
        };
        value.map(AccessGuard::new).transpose()
    }
    pub fn insert(&mut self, key: K::Input<'_>, value: V::Input<'_>) -> Result<()> {
        let Source::Write(tx) = &self.source else {
            anyhow::bail!("read-only transaction")
        };
        let key = K::encode(key);
        ensure!(key.len() <= 65535, "storage key exceeds limit");
        tx.borrow_mut().as_mut().expect("live transaction").insert(
            &self.space,
            key,
            V::encode(value),
        );
        Ok(())
    }
    pub fn remove(&mut self, key: K::Input<'_>) -> Result<Option<AccessGuard<V>>> {
        let previous = self.get(key)?;
        self.delete(key)?;
        Ok(previous)
    }
    /// Delete without fetching a potentially large value. Callers must hold
    /// an exclusive lifecycle guard or otherwise not need a previous-value check.
    pub fn delete(&mut self, key: K::Input<'_>) -> Result<()> {
        let Source::Write(tx) = &self.source else {
            anyhow::bail!("read-only transaction")
        };
        tx.borrow_mut()
            .as_mut()
            .expect("live transaction")
            .remove(&self.space, K::encode(key));
        Ok(())
    }
    pub fn iter(&self) -> Result<TableIter<K, V>> {
        self.range_bytes((Bound::Unbounded, Bound::Unbounded))
    }
    pub fn keys(&self) -> Result<KeyIter<K>> {
        Ok(KeyIter {
            inner: self.iter()?.inner,
            types: PhantomData,
        })
    }
    pub fn range_keys<'a>(&self, range: impl RangeBounds<K::Input<'a>>) -> Result<KeyIter<K>> {
        Ok(KeyIter {
            inner: self.range(range)?.inner,
            types: PhantomData,
        })
    }
    pub fn range<'a>(&self, range: impl RangeBounds<K::Input<'a>>) -> Result<TableIter<K, V>> {
        let encode = |bound: Bound<&K::Input<'a>>| match bound {
            Bound::Included(v) => Bound::Included(K::encode(*v)),
            Bound::Excluded(v) => Bound::Excluded(K::encode(*v)),
            Bound::Unbounded => Bound::Unbounded,
        };
        self.range_bytes((encode(range.start_bound()), encode(range.end_bound())))
    }
    fn range_bytes(&self, bounds: (Bound<Vec<u8>>, Bound<Vec<u8>>)) -> Result<TableIter<K, V>> {
        let iter = match &self.source {
            Source::Read(tx) => tx.range(&self.space, bounds),
            Source::Write(tx) => tx
                .borrow()
                .as_ref()
                .expect("live transaction")
                .range(&self.space, bounds),
        };
        Ok(TableIter {
            inner: iter,
            types: PhantomData,
        })
    }
    pub fn len(&self) -> Result<u64> {
        self.keys()?.try_fold(0, |n, item| {
            item?;
            Ok(n + 1)
        })
    }
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.keys()?.next().transpose()?.is_none())
    }
}
pub struct KeyIter<K: Codec> {
    inner: fjall::Iter,
    types: PhantomData<K>,
}
impl<K: Codec> Iterator for KeyIter<K> {
    type Item = Result<AccessGuard<K>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|guard| AccessGuard::new(guard.key()?))
    }
}
pub struct TableIter<K: Codec, V: Codec> {
    inner: fjall::Iter,
    types: PhantomData<(K, V)>,
}
impl<K: Codec, V: Codec> Iterator for TableIter<K, V> {
    type Item = Result<(AccessGuard<K>, AccessGuard<V>)>;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|guard| {
            let (k, v) = guard.into_inner()?;
            Ok((AccessGuard::new(k)?, AccessGuard::new(v)?))
        })
    }
}
impl<K: Codec, V: Codec> DoubleEndedIterator for TableIter<K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|guard| {
            let (k, v) = guard.into_inner()?;
            Ok((AccessGuard::new(k)?, AccessGuard::new(v)?))
        })
    }
}

#[cfg(test)]
mod durability_tests {
    use super::*;
    use std::io::{BufRead, Write};
    const DATA: TableDefinition<&str, &str> = TableDefinition::new("durability_fixture");

    #[test]
    fn v37_crash_writer_helper() {
        let Some(path) = std::env::var_os("VWORK_JOURNAL_CRASH_PATH") else {
            return;
        };
        let db = Database::open(Path::new(&path)).unwrap();
        let mut tx = db.begin_write().unwrap();
        let immediate = std::env::var("VWORK_JOURNAL_CRASH_MODE").unwrap() == "immediate";
        if immediate {
            tx.set_durability(Durability::Immediate).unwrap();
        }
        tx.open_table(DATA)
            .unwrap()
            .insert("key", "saved content")
            .unwrap();
        tx.commit().unwrap();
        let start = std::time::Instant::now();
        while db.durability_status().pending_transactions != 0 {
            assert!(
                start.elapsed() < DURABILITY_LIMIT,
                "real periodic sync missed deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        println!("JOURNAL_DURABLE_READY");
        std::io::stdout().flush().unwrap();
        // Parent forcibly terminates this process: neither Database nor Fjall
        // Drop can make the test accidentally pass by syncing on clean close.
        std::thread::sleep(std::time::Duration::from_secs(90));
        std::process::exit(3);
    }
    fn crash_reopen(mode: &str) {
        let temp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "kv::durability_tests::v37_crash_writer_helper",
                "--nocapture",
            ])
            .env("VWORK_JOURNAL_CRASH_PATH", temp.path())
            .env("VWORK_JOURNAL_CRASH_MODE", mode)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let ready = std::io::BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(std::result::Result::ok)
            .any(|line| line.contains("JOURNAL_DURABLE_READY"));
        let _ = child.kill();
        child.wait().unwrap();
        assert!(ready, "child failed before durable barrier");
        let reopened = Database::open(temp.path()).unwrap();
        let read = reopened.begin_read().unwrap();
        let table = read.open_table(DATA).unwrap();
        assert_eq!(table.get("key").unwrap().unwrap().value(), "saved content");
    }
    #[test]
    fn v37_periodic_sync_survives_process_kill_within_sixty_seconds() {
        crash_reopen("periodic");
    }
    #[test]
    fn v37_explicit_save_survives_process_kill() {
        crash_reopen("immediate");
    }
}
