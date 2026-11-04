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
}
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let inner = OptimisticTxDatabase::builder(path)
            .cache_size(64 * 1024 * 1024)
            .max_journaling_size(128 * 1024 * 1024)
            .journal_compression(fjall::CompressionType::Lz4)
            .worker_threads(2)
            .open()?;
        Ok(Self { inner })
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
        Ok(WriteTransaction {
            db: self,
            git_batch: RefCell::new(Default::default()),
            inner: RefCell::new(Some(
                self.inner
                    .write_tx()?
                    .durability(Some(fjall::PersistMode::SyncAll)),
            )),
        })
    }
    pub fn disk_space(&self) -> Result<u64> {
        Ok(self.inner.disk_space()?)
    }
    pub fn persist(&self) -> Result<()> {
        Ok(self.inner.persist(fjall::PersistMode::SyncAll)?)
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
        let tx = self.inner.get_mut().take().expect("live transaction");
        *self.inner.get_mut() = Some(tx.durability(Some(match durability {
            Durability::None => fjall::PersistMode::Buffer,
            Durability::Immediate => fjall::PersistMode::SyncAll,
        })));
        Ok(())
    }
    pub fn commit(self) -> Result<()> {
        self.inner
            .into_inner()
            .expect("live transaction")
            .commit()?
            .map_err(|_| Conflict.into())
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
