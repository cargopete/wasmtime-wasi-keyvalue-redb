# wasmtime-wasi-keyvalue-redb

A persistent, embedded [redb]-backed host implementation of the [wasi:keyvalue] API for Wasmtime.

Drop-in alternative to the official `wasmtime-wasi-keyvalue` crate (in-memory only) for use cases
that need durable local storage with no external services.

## Design

All data lives in a single [redb] table (`"wasi_kv"`). Bucket names are namespaced via key
encoding: `{bucket_name}\0{user_key}`. Multiple buckets share one database file.

```
open("logs")   → keys stored as "logs\0{key}"
open("cache")  → keys stored as "cache\0{key}"
open("")       → normalised to "default\0{key}"
```

## Usage

```rust
use wasmtime::{Engine, Store, component::{Linker, ResourceTable}};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_keyvalue_redb::{WasiKeyValueRedb, WasiKeyValueRedbCtx, WasiKeyValueRedbCtxBuilder};

struct Ctx {
    table: ResourceTable,
    wasi: WasiCtx,
    kv: WasiKeyValueRedbCtx,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

let kv_ctx = WasiKeyValueRedbCtxBuilder::new()
    .database_path("/var/lib/my-component/kv.redb")?
    .build()?;

let mut linker = Linker::<Ctx>::new(&Engine::default());
wasmtime_wasi_keyvalue_redb::add_to_linker(&mut linker, |h: &mut Ctx| {
    WasiKeyValueRedb::new(&h.kv, &mut h.table)
})?;
```

## WIT interface coverage

| Interface | Status |
|---|---|
| `wasi:keyvalue/store` — `open`, `get`, `set`, `delete`, `exists`, `list-keys` | done |
| `wasi:keyvalue/atomics` — `increment` | done |
| `wasi:keyvalue/batch` — `get-many`, `set-many`, `delete-many` | done |

## Running tests

```
cargo test
```

## Related

- [wasmtime-wasi-keyvalue-redis](https://github.com/cargopete/wasmtime-wasi-keyvalue-redis) — Redis backend (satisfies Phase 2 portability criteria)
- [WebAssembly/wasi-keyvalue](https://github.com/WebAssembly/wasi-keyvalue) — the proposal
- [bytecodealliance/wasmtime](https://github.com/bytecodealliance/wasmtime) — runtime
- [redb](https://github.com/cberner/redb) — the embedded database

[redb]: https://www.redb.org
[wasi:keyvalue]: https://github.com/WebAssembly/wasi-keyvalue
