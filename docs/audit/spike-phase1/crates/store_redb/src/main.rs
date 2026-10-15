//! Phase 1 spike: redb as embedded transactional catalog/metadata store.
//!
//! Tests (all measured):
//!  1. insert 10k records (u64 key -> 128xf32 blob + json-ish payload) in ONE txn
//!  2. close, reopen, verify count + spot-check values (durability / restart)
//!  3. update a record, delete a record, verify
//!  4. second table (metadata index) coexisting in the same file
//!
//! Run: cargo run -p store_redb --release

use anyhow::Result;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::time::Instant;

const VECTORS: TableDefinition<u64, &[u8]> = TableDefinition::new("vectors");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");
const N: usize = 10_000;
const DIM: usize = 128;

fn main() -> Result<()> {
    let path = "/tmp/polyvec-spike/store.redb";
    std::fs::create_dir_all("/tmp/polyvec-spike")?;
    let _ = std::fs::remove_file(path); // fresh start for repeatability

    // deterministic payload: 128 f32 LE bytes per vector
    let payload = |i: usize| -> Vec<u8> {
        let mut b = Vec::with_capacity(DIM * 4);
        for d in 0..DIM {
            let x = ((i.wrapping_mul(31).wrapping_add(d)) as f32) / 1000.0;
            b.extend_from_slice(&x.to_le_bytes());
        }
        b
    };

    // ---- 1. bulk insert in a single transaction ----
    let db = Database::create(path)?;
    let t = Instant::now();
    {
        let txn = db.begin_write()?;
        {
            let mut tvec = txn.open_table(VECTORS)?;
            let mut tmeta = txn.open_table(META)?;
            for i in 0..N as u64 {
                let p = payload(i as usize);
                tvec.insert(i, p.as_slice())?;
            }
            tmeta.insert("collection", "spike")?;
            tmeta.insert("dims", "128")?;
            tmeta.insert("metric", "cosine")?;
        }
        txn.commit()?;
    }
    println!(
        "[redb] insert {N} vector blobs + 3 meta rows, 1 txn: {:?}",
        t.elapsed()
    );

    // ---- 2. close + reopen (restart durability) ----
    drop(db);
    let t = Instant::now();
    let db = Database::open(path)?;
    println!("[redb] reopen: {:?}", t.elapsed());
    {
        let txn = db.begin_read()?;
        let tvec = txn.open_table(VECTORS)?;
        let tmeta = txn.open_table(META)?;
        assert_eq!(tvec.len()?, N as u64, "count changed across restart!");
        // spot-check 100 deterministic values
        for i in (0..N as u64).step_by(100) {
            let got = tvec.get(i)?.unwrap();
            assert_eq!(
                got.value(),
                payload(i as usize).as_slice(),
                "value mismatch at {i}"
            );
        }
        assert_eq!(tmeta.get("collection")?.unwrap().value(), "spike");
        println!(
            "[redb] restart durability: OK (count={} + 100 spot checks)",
            tvec.len()?
        );
    }

    // ---- 3. update + delete ----
    {
        let txn = db.begin_write()?;
        {
            let mut tvec = txn.open_table(VECTORS)?;
            let mut upd = payload(42);
            upd[0] = 0xFF; // mutate one byte
            tvec.insert(42u64, upd.as_slice())?; // update
            tvec.remove(7u64)?; // delete
        }
        txn.commit()?;
    }
    {
        let txn = db.begin_read()?;
        let tvec = txn.open_table(VECTORS)?;
        let v42 = tvec.get(42u64)?.unwrap();
        assert_eq!(v42.value()[0], 0xFF, "update not visible");
        assert!(tvec.get(7u64)?.is_none(), "delete not visible");
        assert_eq!(tvec.len()?, N as u64 - 1);
        println!(
            "[redb] update(42) + delete(7): OK, count now {}",
            tvec.len()?
        );
    }

    // ---- 4. crash-ish recovery note: uncommitted txn is rolled back ----
    {
        let txn = db.begin_write()?;
        {
            let mut tvec = txn.open_table(VECTORS)?;
            tvec.insert(99999u64, payload(1).as_slice())?;
        }
        drop(txn); // no commit -> must not persist
    }
    {
        let txn = db.begin_read()?;
        let tvec = txn.open_table(VECTORS)?;
        assert!(tvec.get(99999u64)?.is_none(), "uncommitted write leaked!");
        println!("[redb] uncommitted txn rolled back: OK");
    }

    println!("\n===== DONE =====");
    Ok(())
}
