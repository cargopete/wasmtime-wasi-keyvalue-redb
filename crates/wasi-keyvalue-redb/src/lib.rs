//! # wasmtime-wasi-keyvalue-redb
//!
//! A [redb]-backed host implementation of the [wasi:keyvalue] API for Wasmtime.
//!
//! This crate provides a persistent, embedded key-value store backend using [redb],
//! implementing the same interface contract as `wasmtime-wasi-keyvalue` so it can be
//! used as a drop-in alternative wherever persistent storage is needed.
//!
//! # Design
//!
//! All data lives in a single redb table (`"wasi_kv"`). Each bucket maps to a logical
//! namespace via key encoding: `{bucket_name}\0{user_key}`. The null-byte separator is
//! safe because WIT identifiers (used as bucket names) and typical KV keys cannot contain it.
//!
//! Multiple buckets are supported by passing different identifiers to `open()`. An empty
//! identifier maps to the `"default"` bucket.
//!
//! # Example
//!
//! ```no_run
//! use wasmtime::{Engine, Store, component::{Linker, ResourceTable}};
//! use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
//! use wasmtime_wasi_keyvalue_redb::{WasiKeyValueRedb, WasiKeyValueRedbCtx, WasiKeyValueRedbCtxBuilder};
//!
//! struct Ctx {
//!     table: ResourceTable,
//!     wasi: WasiCtx,
//!     kv: WasiKeyValueRedbCtx,
//! }
//!
//! impl WasiView for Ctx {
//!     fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
//!         wasmtime_wasi::WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
//!     }
//! }
//!
//! # fn main() -> anyhow::Result<()> {
//! let kv_ctx = WasiKeyValueRedbCtxBuilder::new()
//!     .database_path("/tmp/my-component.redb")?
//!     .build()?;
//!
//! let mut linker = Linker::<Ctx>::new(&Engine::default());
//! wasmtime_wasi_keyvalue_redb::add_to_linker(&mut linker, |h: &mut Ctx| {
//!     WasiKeyValueRedb::new(&h.kv, &mut h.table)
//! })?;
//! # Ok(()) }
//! ```
//!
//! [redb]: https://www.redb.org
//! [wasi:keyvalue]: https://github.com/WebAssembly/wasi-keyvalue

mod generated {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "wasi:keyvalue/imports",
        imports: { default: trappable },
        with: {
            "wasi:keyvalue/store.bucket": crate::Bucket,
        },
        trappable_error_type: {
            "wasi:keyvalue/store.error" => crate::Error,
        },
    });
}

use self::generated::wasi::keyvalue;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use wasmtime::Result;
use wasmtime::component::{HasData, Resource, ResourceTable, ResourceTableError};

// Single table for all buckets. Keys are encoded as `{bucket}\0{user_key}`.
const KV: TableDefinition<&str, &[u8]> = TableDefinition::new("wasi_kv");

// ---------------------------------------------------------------------------
// Key encoding helpers
// ---------------------------------------------------------------------------

fn encode_key(bucket: &str, key: &str) -> String {
    format!("{}\0{}", bucket, key)
}

fn bucket_prefix(bucket: &str) -> String {
    format!("{}\0", bucket)
}

/// The exclusive upper bound for range scans over a bucket's keys.
/// All encoded keys for `bucket` are < `{bucket}\x01`.
fn bucket_range_end(bucket: &str) -> String {
    format!("{}\x01", bucket)
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[doc(hidden)]
pub enum Error {
    NoSuchStore,
    AccessDenied,
    Other(String),
}

impl From<ResourceTableError> for Error {
    fn from(err: ResourceTableError) -> Self {
        Self::Other(err.to_string())
    }
}

macro_rules! impl_from_redb {
    ($($t:ty),*) => {
        $(impl From<$t> for Error {
            fn from(e: $t) -> Self { Self::Other(e.to_string()) }
        })*
    }
}

impl_from_redb!(
    redb::Error,
    redb::DatabaseError,
    redb::TableError,
    redb::StorageError,
    redb::TransactionError,
    redb::CommitError
);

// ---------------------------------------------------------------------------
// Bucket
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub struct Bucket {
    /// Logical bucket name — becomes the first segment of each encoded key.
    name: String,
}

// ---------------------------------------------------------------------------
// Context + Builder
// ---------------------------------------------------------------------------

/// Holds the redb [`Database`] handle.
pub struct WasiKeyValueRedbCtx {
    db: Database,
}

/// Builder for [`WasiKeyValueRedbCtx`].
#[derive(Default)]
pub struct WasiKeyValueRedbCtxBuilder {
    db: Option<Database>,
}

impl WasiKeyValueRedbCtxBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or create) a redb database file at `path`.
    pub fn database_path(mut self, path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let db = Database::create(path.as_ref())?;
        self.db = Some(db);
        Ok(self)
    }

    /// Use an already-opened [`Database`] (useful for testing with temp databases).
    pub fn database(mut self, db: Database) -> Self {
        self.db = Some(db);
        self
    }

    pub fn build(self) -> anyhow::Result<WasiKeyValueRedbCtx> {
        let db = self.db.ok_or_else(|| {
            anyhow::anyhow!("no database configured; call database_path() or database()")
        })?;
        // Ensure the table exists.
        let write_txn = db.begin_write()?;
        write_txn.open_table(KV)?;
        write_txn.commit()?;
        Ok(WasiKeyValueRedbCtx { db })
    }
}

// ---------------------------------------------------------------------------
// View struct
// ---------------------------------------------------------------------------

/// Short-lived view implementing the wasi:keyvalue host traits.
pub struct WasiKeyValueRedb<'a> {
    ctx: &'a WasiKeyValueRedbCtx,
    table: &'a mut ResourceTable,
}

impl<'a> WasiKeyValueRedb<'a> {
    pub fn new(ctx: &'a WasiKeyValueRedbCtx, table: &'a mut ResourceTable) -> Self {
        Self { ctx, table }
    }

    fn db(&self) -> &Database {
        &self.ctx.db
    }
}

// ---------------------------------------------------------------------------
// store::Host
// ---------------------------------------------------------------------------

impl keyvalue::store::Host for WasiKeyValueRedb<'_> {
    fn open(&mut self, identifier: String) -> Result<Resource<Bucket>, Error> {
        let name = if identifier.is_empty() {
            "default".to_string()
        } else {
            identifier
        };
        Ok(self.table.push(Bucket { name })?)
    }

    fn convert_error(&mut self, err: Error) -> Result<keyvalue::store::Error> {
        Ok(match err {
            Error::NoSuchStore => keyvalue::store::Error::NoSuchStore,
            Error::AccessDenied => keyvalue::store::Error::AccessDenied,
            Error::Other(s) => keyvalue::store::Error::Other(s),
        })
    }
}

// ---------------------------------------------------------------------------
// store::HostBucket
// ---------------------------------------------------------------------------

impl keyvalue::store::HostBucket for WasiKeyValueRedb<'_> {
    fn get(&mut self, bucket: Resource<Bucket>, key: String) -> Result<Option<Vec<u8>>, Error> {
        let bucket = self.table.get(&bucket)?;
        let encoded = encode_key(&bucket.name, &key);
        let read_txn = self.db().begin_read()?;
        let table = read_txn.open_table(KV)?;
        match table.get(encoded.as_str()).map_err(Error::from)? {
            Some(guard) => Ok(Some(guard.value().to_vec())),
            None => Ok(None),
        }
    }

    fn set(&mut self, bucket: Resource<Bucket>, key: String, value: Vec<u8>) -> Result<(), Error> {
        let bucket = self.table.get(&bucket)?;
        let encoded = encode_key(&bucket.name, &key);
        let write_txn = self.db().begin_write()?;
        {
            let mut table = write_txn.open_table(KV)?;
            table.insert(encoded.as_str(), value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn delete(&mut self, bucket: Resource<Bucket>, key: String) -> Result<(), Error> {
        let bucket = self.table.get(&bucket)?;
        let encoded = encode_key(&bucket.name, &key);
        let write_txn = self.db().begin_write()?;
        {
            let mut table = write_txn.open_table(KV)?;
            table.remove(encoded.as_str())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn exists(&mut self, bucket: Resource<Bucket>, key: String) -> Result<bool, Error> {
        let bucket = self.table.get(&bucket)?;
        let encoded = encode_key(&bucket.name, &key);
        let read_txn = self.db().begin_read()?;
        let table = read_txn.open_table(KV)?;
        Ok(table.get(encoded.as_str()).map_err(Error::from)?.is_some())
    }

    fn list_keys(
        &mut self,
        bucket: Resource<Bucket>,
        cursor: Option<u64>,
    ) -> Result<keyvalue::store::KeyResponse, Error> {
        let bucket = self.table.get(&bucket)?;
        let prefix = bucket_prefix(&bucket.name);
        let end = bucket_range_end(&bucket.name);
        let prefix_len = prefix.len();

        let read_txn = self.db().begin_read()?;
        let table = read_txn.open_table(KV)?;

        let skip = cursor.unwrap_or(0) as usize;
        let mut keys: Vec<String> = Vec::new();
        for entry in table
            .range(prefix.as_str()..end.as_str())
            .map_err(Error::from)?
            .skip(skip)
        {
            let (k, _v) = entry.map_err(Error::from)?;
            keys.push(k.value()[prefix_len..].to_string());
        }

        Ok(keyvalue::store::KeyResponse { keys, cursor: None })
    }

    fn drop(&mut self, bucket: Resource<Bucket>) -> Result<()> {
        self.table.delete(bucket)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// atomics::Host
// ---------------------------------------------------------------------------

impl keyvalue::atomics::Host for WasiKeyValueRedb<'_> {
    fn increment(
        &mut self,
        bucket: Resource<Bucket>,
        key: String,
        delta: u64,
    ) -> Result<u64, Error> {
        let bucket = self.table.get(&bucket)?;
        let encoded = encode_key(&bucket.name, &key);
        let write_txn = self.db().begin_write()?;
        let new_value = {
            let mut table = write_txn.open_table(KV)?;
            let current: u64 = match table.get(encoded.as_str()).map_err(Error::from)? {
                Some(guard) => std::str::from_utf8(guard.value())
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0),
                None => 0,
            };
            let next = current.saturating_add(delta);
            table.insert(encoded.as_str(), next.to_string().as_bytes())?;
            next
        };
        write_txn.commit()?;
        Ok(new_value)
    }
}

// ---------------------------------------------------------------------------
// batch::Host
// ---------------------------------------------------------------------------

impl keyvalue::batch::Host for WasiKeyValueRedb<'_> {
    fn get_many(
        &mut self,
        bucket: Resource<Bucket>,
        keys: Vec<String>,
    ) -> Result<Vec<Option<(String, Vec<u8>)>>, Error> {
        let bucket = self.table.get(&bucket)?;
        let read_txn = self.db().begin_read()?;
        let table = read_txn.open_table(KV)?;
        let mut results = Vec::with_capacity(keys.len());
        for key in keys {
            let encoded = encode_key(&bucket.name, &key);
            let entry = match table.get(encoded.as_str()).map_err(Error::from)? {
                Some(guard) => Some((key.clone(), guard.value().to_vec())),
                None => None,
            };
            results.push(entry);
        }
        Ok(results)
    }

    fn set_many(
        &mut self,
        bucket: Resource<Bucket>,
        key_values: Vec<(String, Vec<u8>)>,
    ) -> Result<(), Error> {
        let bucket = self.table.get(&bucket)?;
        let write_txn = self.db().begin_write()?;
        {
            let mut table = write_txn.open_table(KV)?;
            for (key, value) in &key_values {
                let encoded = encode_key(&bucket.name, key);
                table.insert(encoded.as_str(), value.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn delete_many(&mut self, bucket: Resource<Bucket>, keys: Vec<String>) -> Result<(), Error> {
        let bucket = self.table.get(&bucket)?;
        let write_txn = self.db().begin_write()?;
        {
            let mut table = write_txn.open_table(KV)?;
            for key in &keys {
                let encoded = encode_key(&bucket.name, key);
                table.remove(encoded.as_str())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Linker registration
// ---------------------------------------------------------------------------

/// Register all `wasi:keyvalue` interfaces into a [`wasmtime::component::Linker`].
pub fn add_to_linker<T: Send + 'static>(
    l: &mut wasmtime::component::Linker<T>,
    f: fn(&mut T) -> WasiKeyValueRedb<'_>,
) -> Result<()> {
    keyvalue::store::add_to_linker::<_, HasWasiKeyValueRedb>(l, f)?;
    keyvalue::atomics::add_to_linker::<_, HasWasiKeyValueRedb>(l, f)?;
    keyvalue::batch::add_to_linker::<_, HasWasiKeyValueRedb>(l, f)?;
    Ok(())
}

struct HasWasiKeyValueRedb;

impl HasData for HasWasiKeyValueRedb {
    type Data<'a> = WasiKeyValueRedb<'a>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::wasi::keyvalue::{
        atomics::Host as AtomicsHost,
        batch::Host as BatchHost,
        store::{Host, HostBucket},
    };
    use wasmtime::component::ResourceTable;

    // WIT borrow<bucket> params are single-use on the Rust host side.
    // Open a fresh handle before each operation; the underlying Bucket stays in the table.

    fn make_ctx() -> (WasiKeyValueRedbCtx, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = WasiKeyValueRedbCtxBuilder::new()
            .database_path(dir.path().join("test.redb"))
            .unwrap()
            .build()
            .unwrap();
        (ctx, dir)
    }

    macro_rules! bucket {
        ($kv:expr, $name:expr) => {
            Host::open(&mut $kv, $name.to_string()).unwrap()
        };
    }

    #[test]
    fn set_get_delete() {
        let (ctx, _dir) = make_ctx();
        let mut table = ResourceTable::new();
        let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);

        let b = bucket!(kv, "");
        HostBucket::set(&mut kv, b, "hello".to_string(), b"world".to_vec()).unwrap();
        let b = bucket!(kv, "");
        assert_eq!(HostBucket::get(&mut kv, b, "hello".to_string()).unwrap(), Some(b"world".to_vec()));
        let b = bucket!(kv, "");
        HostBucket::delete(&mut kv, b, "hello".to_string()).unwrap();
        let b = bucket!(kv, "");
        assert_eq!(HostBucket::get(&mut kv, b, "hello".to_string()).unwrap(), None);
    }

    #[test]
    fn exists() {
        let (ctx, _dir) = make_ctx();
        let mut table = ResourceTable::new();
        let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);

        let b = bucket!(kv, "bucket");
        assert!(!HostBucket::exists(&mut kv, b, "k".to_string()).unwrap());
        let b = bucket!(kv, "bucket");
        HostBucket::set(&mut kv, b, "k".to_string(), b"v".to_vec()).unwrap();
        let b = bucket!(kv, "bucket");
        assert!(HostBucket::exists(&mut kv, b, "k".to_string()).unwrap());
    }

    #[test]
    fn list_keys() {
        let (ctx, _dir) = make_ctx();
        let mut table = ResourceTable::new();
        let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);

        let b = bucket!(kv, "listing"); HostBucket::set(&mut kv, b, "a".to_string(), b"1".to_vec()).unwrap();
        let b = bucket!(kv, "listing"); HostBucket::set(&mut kv, b, "b".to_string(), b"2".to_vec()).unwrap();
        let b = bucket!(kv, "listing"); HostBucket::set(&mut kv, b, "c".to_string(), b"3".to_vec()).unwrap();

        let b = bucket!(kv, "listing");
        let resp = HostBucket::list_keys(&mut kv, b, None).unwrap();
        let mut keys = resp.keys;
        keys.sort();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn buckets_are_isolated() {
        let (ctx, _dir) = make_ctx();
        let mut table = ResourceTable::new();
        let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);

        let b = bucket!(kv, "alpha");
        HostBucket::set(&mut kv, b, "key".to_string(), b"from-alpha".to_vec()).unwrap();

        let b = bucket!(kv, "beta");
        assert_eq!(HostBucket::get(&mut kv, b, "key".to_string()).unwrap(), None);
        let b = bucket!(kv, "beta");
        assert!(HostBucket::list_keys(&mut kv, b, None).unwrap().keys.is_empty());
        let b = bucket!(kv, "alpha");
        assert_eq!(HostBucket::get(&mut kv, b, "key".to_string()).unwrap(), Some(b"from-alpha".to_vec()));
    }

    #[test]
    fn increment() {
        let (ctx, _dir) = make_ctx();
        let mut table = ResourceTable::new();
        let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);

        let b = bucket!(kv, "counters");
        assert_eq!(AtomicsHost::increment(&mut kv, b, "hits".to_string(), 1).unwrap(), 1);
        let b = bucket!(kv, "counters");
        assert_eq!(AtomicsHost::increment(&mut kv, b, "hits".to_string(), 4).unwrap(), 5);
        let b = bucket!(kv, "counters");
        assert_eq!(AtomicsHost::increment(&mut kv, b, "hits".to_string(), 10).unwrap(), 15);
    }

    #[test]
    fn batch_ops() {
        let (ctx, _dir) = make_ctx();
        let mut table = ResourceTable::new();
        let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);

        let b = bucket!(kv, "batch");
        BatchHost::set_many(&mut kv, b, vec![
            ("x".to_string(), b"1".to_vec()),
            ("y".to_string(), b"2".to_vec()),
            ("z".to_string(), b"3".to_vec()),
        ]).unwrap();

        let b = bucket!(kv, "batch");
        let results = BatchHost::get_many(&mut kv, b, vec![
            "x".to_string(), "missing".to_string(), "z".to_string(),
        ]).unwrap();
        assert_eq!(results[0], Some(("x".to_string(), b"1".to_vec())));
        assert_eq!(results[1], None);
        assert_eq!(results[2], Some(("z".to_string(), b"3".to_vec())));

        let b = bucket!(kv, "batch");
        BatchHost::delete_many(&mut kv, b, vec!["x".to_string(), "y".to_string()]).unwrap();
        let b = bucket!(kv, "batch");
        let resp = HostBucket::list_keys(&mut kv, b, None).unwrap();
        assert_eq!(resp.keys, vec!["z"]);
    }

    #[test]
    fn persists_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("persist.redb");

        {
            let ctx = WasiKeyValueRedbCtxBuilder::new()
                .database_path(&db_path).unwrap().build().unwrap();
            let mut table = ResourceTable::new();
            let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);
            let b = bucket!(kv, "data");
            HostBucket::set(&mut kv, b, "k".to_string(), b"still here".to_vec()).unwrap();
        }

        {
            let ctx = WasiKeyValueRedbCtxBuilder::new()
                .database_path(&db_path).unwrap().build().unwrap();
            let mut table = ResourceTable::new();
            let mut kv = WasiKeyValueRedb::new(&ctx, &mut table);
            let b = bucket!(kv, "data");
            assert_eq!(
                HostBucket::get(&mut kv, b, "k".to_string()).unwrap(),
                Some(b"still here".to_vec())
            );
        }
    }
}
