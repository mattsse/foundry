//! Helper types for working with [revm](foundry_evm::revm)

use crate::{
    eth::error::BlockchainError,
    revm::{Account, AccountInfo},
    U256,
};
use ethers::prelude::{Address, Bytes, H256};
use foundry_evm::{
    executor::{
        fork::{BlockchainDb, SharedBackend},
        DatabaseRef,
    },
    revm::{db::CacheDB, Database, DatabaseCommit},
    HashMap as Map,
};
use tracing::log::trace;

/// This bundles all required revm traits
pub trait Db: DatabaseRef + Database + DatabaseCommit + Send + Sync + 'static {
    /// Inserts an account
    fn insert_account(&mut self, address: Address, account: AccountInfo);

    /// Sets the nonce of the given address
    fn set_nonce(&mut self, address: Address, nonce: u64) {
        let mut info = self.basic(address);
        info.nonce = nonce;
        self.insert_account(address, info);
    }

    /// Sets the balance of the given address
    fn set_balance(&mut self, address: Address, balance: U256) {
        let mut info = self.basic(address);
        info.balance = balance;
        self.insert_account(address, info);
    }

    /// Sets the balance of the given address
    fn set_code(&mut self, address: Address, code: Bytes) {
        let mut info = self.basic(address);
        info.code = Some(code.to_vec().into());
        self.insert_account(address, info);
    }

    /// Sets the balance of the given address
    fn set_storage_at(&mut self, address: Address, slot: U256, val: U256);
}

/// Implement the helper for revm's db
impl<ExtDB: DatabaseRef + Send + Sync + 'static> Db for CacheDB<ExtDB> {
    fn insert_account(&mut self, address: Address, account: AccountInfo) {
        self.insert_cache(address, account)
    }

    fn set_storage_at(&mut self, address: Address, slot: U256, val: U256) {
        self.insert_cache_storage(address, slot, val)
    }
}

/// Implement the helper for the fork database
impl Db for ForkedDatabase {
    fn insert_account(&mut self, address: Address, account: AccountInfo) {
        self.db.db().do_insert_account(address, account)
    }

    fn set_storage_at(&mut self, address: Address, slot: U256, val: U256) {
        let mut db = self.db.db().storage.write();
        db.entry(address).or_default().insert(slot, val);
    }
}

/// a [revm::Database] that's forked off another client
///
/// The `backend` is used to retrieve (missing) data, which is then fetched from the remote
/// endpoint. The inner in-memory database holds this storage and will be used for write operations.
/// This database uses the `backend` for read and the `db` for write operations. But note the
/// `backend` will also write (missing) data to the `db` in the background
#[derive(Debug, Clone)]
pub struct ForkedDatabase {
    /// responsible for fetching missing data
    ///
    /// This is responsible for getting data
    backend: SharedBackend,
    /// Contains all the data already fetched
    ///
    /// This is used for change commits
    db: BlockchainDb,
}

impl ForkedDatabase {
    /// Creates a new instance of this DB
    pub fn new(backend: SharedBackend, db: BlockchainDb) -> Self {
        Self { backend, db }
    }

    /// Reset the fork to a fresh forked state, and optionally update the fork config
    pub fn reset(
        &self,
        _url: Option<String>,
        block_number: Option<u64>,
    ) -> Result<(), BlockchainError> {
        if let Some(block_number) = block_number {
            self.backend
                .set_pinned_block(block_number)
                .map_err(|err| BlockchainError::Internal(err.to_string()))?;
        }

        // TODO need to find a way to update generic provider via url

        self.db.db().clear();
        trace!(target: "fork", "Cleared database");
        Ok(())
    }

    /// Flushes the cache to disk if configured
    pub fn flush_cache(&self) {
        self.db.cache().flush()
    }
}

impl Database for ForkedDatabase {
    fn basic(&mut self, address: Address) -> AccountInfo {
        self.backend.basic(address)
    }

    fn code_by_hash(&mut self, code_hash: H256) -> bytes::Bytes {
        self.backend.code_by_hash(code_hash)
    }

    fn storage(&mut self, address: Address, index: U256) -> U256 {
        self.backend.storage(address, index)
    }

    fn block_hash(&mut self, number: U256) -> H256 {
        self.backend.block_hash(number)
    }
}

impl DatabaseRef for ForkedDatabase {
    fn basic(&self, address: Address) -> AccountInfo {
        self.backend.basic(address)
    }

    fn code_by_hash(&self, code_hash: H256) -> bytes::Bytes {
        self.backend.code_by_hash(code_hash)
    }

    fn storage(&self, address: Address, index: U256) -> U256 {
        self.backend.storage(address, index)
    }

    fn block_hash(&self, number: U256) -> H256 {
        self.backend.block_hash(number)
    }
}

impl DatabaseCommit for ForkedDatabase {
    fn commit(&mut self, changes: Map<Address, Account>) {
        self.db.db().do_commit(changes)
    }
}
