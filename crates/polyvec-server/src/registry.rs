//! Multi-collection registry: the server's handle on open [`CollectionStore`]s.
//!
//! Collections live at `<data_dir>/<collection-id>/`. The collection id is
//! always a validated [`CollectionId`] slug (`[a-z0-9-_]`, 1–64 chars), so
//! joining it onto the data dir cannot escape — path traversal is impossible
//! by construction, not by filtering.
//!
//! Stores open **lazily** on first use: startup stays fast with many
//! collections, and each collection pays its recovery cost on demand.
//! `GET /v1/collections` (listing) never opens a store — it reads each
//! collection's config straight from the redb catalog via
//! [`read_collection_config`](polyvec_storage::read_collection_config),
//! without loading the ANN index.
//!
//! Concurrency: one [`std::sync::Mutex`] per open store serializes writers;
//! the registry map has its own mutex. Handlers run store operations inside
//! `web::block` and never hold a mutex across `.await`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use polyvec_core::{CollectionConfig, CollectionId, Error, Result};
use polyvec_storage::{CollectionStore, read_collection_config};

/// Shared handle to one open collection.
pub type SharedStore = Arc<Mutex<CollectionStore>>;

/// Opens `<data_dir>` and hands out lazily-opened collection stores.
pub struct Registry {
    data_dir: PathBuf,
    stores: Mutex<HashMap<CollectionId, SharedStore>>,
}

// Compile-time proof that `CollectionStore` can cross into `web::block`
// (which requires `Send`). If a field ever stops being `Send`, this fails
// the build instead of degrading into a runtime surprise.
#[allow(dead_code)]
fn _assert_store_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<CollectionStore>();
}

impl Registry {
    /// Creates a registry rooted at `data_dir` (created on demand by the
    /// binary; listing treats a missing dir as "no collections").
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            stores: Mutex::new(HashMap::new()),
        }
    }

    /// The data directory.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Filesystem location of a collection. Safe by construction: the id is
    /// a validated slug, so this can never escape `data_dir`.
    #[must_use]
    pub fn collection_dir(&self, id: &CollectionId) -> PathBuf {
        self.data_dir.join(id.as_str())
    }

    /// Returns the open store for `id`, opening (and recovering) it on
    /// first use.
    ///
    /// # Errors
    /// [`Error::NotFound`] if no collection exists at the id's directory.
    pub fn get_or_open(&self, id: &CollectionId) -> Result<SharedStore> {
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(store) = stores.get(id) {
            return Ok(Arc::clone(store));
        }
        let dir = self.collection_dir(id);
        let (store, recovery) = CollectionStore::open(&dir)?;
        if !matches!(recovery, polyvec_storage::Recovery::Clean) {
            log::info!("collection '{id}' recovered on open: {recovery:?}");
        }
        let shared = Arc::new(Mutex::new(store));
        stores.insert(id.clone(), Arc::clone(&shared));
        Ok(shared)
    }

    /// Creates a collection and registers its store.
    ///
    /// # Errors
    /// [`Error::AlreadyExists`] if the id is taken (in memory or on disk).
    pub fn create(&self, config: CollectionConfig) -> Result<()> {
        let id = config.name.clone();
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stores.contains_key(&id) {
            return Err(Error::AlreadyExists {
                what: format!("collection '{id}'"),
            });
        }
        let dir = self.collection_dir(&id);
        let store = CollectionStore::create(&dir, config)?;
        stores.insert(id, Arc::new(Mutex::new(store)));
        Ok(())
    }

    /// Removes a collection: unregisters the store, then deletes its
    /// directory.
    ///
    /// A request that already holds the store's `Arc` may still finish
    /// against the unlinked directory (its redb handle stays valid); it
    /// cannot corrupt anything, and a later open reports `NotFound`.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the collection does not exist.
    pub fn remove(&self, id: &CollectionId) -> Result<()> {
        let mut stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let existed_in_memory = stores.remove(id).is_some();
        let dir = self.collection_dir(id);
        let existed_on_disk = dir.exists();
        if !existed_in_memory && !existed_on_disk {
            return Err(Error::NotFound {
                what: format!("collection '{id}'"),
            });
        }
        if existed_on_disk {
            std::fs::remove_dir_all(&dir).map_err(|e| Error::Storage {
                detail: format!("remove collection dir {}: {e}", dir.display()),
            })?;
        }
        Ok(())
    }

    /// Lists every collection found in the data dir, cheapest-first: each
    /// config is read from the redb catalog without loading the ANN index.
    /// Directories that hold no collection are skipped.
    ///
    /// Collections that are already open are listed from memory: redb
    /// refuses to open a database file twice in one process
    /// ("Database already open. Cannot acquire lock"), so re-reading their
    /// catalogs from disk would fail.
    ///
    /// # Errors
    /// [`Error::Storage`] if the data dir cannot be read, or a collection
    /// fails to decode (corrupt catalogs surface here instead of being
    /// silently skipped).
    pub fn list(&self) -> Result<Vec<CollectionConfig>> {
        // Open stores first (from memory — no disk open, no lock conflict).
        let stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut configs = Vec::with_capacity(stores.len());
        let mut open_names = std::collections::HashSet::with_capacity(stores.len());
        for (id, shared) in stores.iter() {
            let guard = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            configs.push(guard.config().clone());
            open_names.insert(id.as_str().to_string());
        }
        drop(stores);

        // Then the data dir, skipping what is already listed.
        let entries = std::fs::read_dir(&self.data_dir).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Error::NotFound {
                    what: format!("data dir {}", self.data_dir.display()),
                };
            }
            Error::Storage {
                detail: format!("read data dir {}: {e}", self.data_dir.display()),
            }
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::Storage {
                detail: format!("read data dir entry: {e}"),
            })?;
            if !entry
                .file_type()
                .map_err(|e| Error::Storage {
                    detail: format!("stat data dir entry: {e}"),
                })?
                .is_dir()
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if open_names.contains(&name) {
                continue;
            }
            match read_collection_config(&entry.path()) {
                Ok(config) => configs.push(config),
                Err(Error::NotFound { .. }) => {} // stray directory, not a collection
                Err(other) => return Err(other),
            }
        }
        configs.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        Ok(configs)
    }

    /// Flushes every open collection. Called on graceful shutdown; each
    /// collection's result is reported so a failed checkpoint is visible.
    #[must_use]
    pub fn flush_all(&self) -> Vec<(String, Result<()>)> {
        let stores = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut results = Vec::with_capacity(stores.len());
        for (id, shared) in stores.iter() {
            let result = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .flush();
            results.push((id.as_str().to_string(), result));
        }
        results
    }
}

/// Locks a shared store, converting poisoning into an internal error
/// (a poisoned mutex means a previous holder panicked — a bug, never user
/// error).
///
/// # Errors
/// [`Error::Internal`] if the mutex is poisoned.
pub fn lock_store(store: &SharedStore) -> Result<MutexGuard<'_, CollectionStore>> {
    store.lock().map_err(|_| Error::Internal {
        detail: "collection store mutex poisoned".to_string(),
    })
}
