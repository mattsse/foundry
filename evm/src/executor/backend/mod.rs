use crate::executor::{fork::SharedBackend, Fork};
use ethers::prelude::{H160, H256, U256};
use revm::{
    db::{CacheDB, DatabaseRef, EmptyDB},
    AccountInfo, Env, InMemoryDB,
};

mod in_memory_db;
use crate::executor::{
    fork::{database::ForkDbSnapshot, CreateFork, ForkId, MultiFork},
    snapshot::Snapshots,
};
pub use in_memory_db::MemDb;

/// Provides the underlying `revm::Database` implementation.
///
/// A `Backend` can be initialised in two forms:
///
/// # 1. Empty in-memory Database
/// This is the default variant: an empty `revm::Database`
///
/// # 2. Forked Database
/// A `revm::Database` that forks off a remote client
///
/// In addition to that we support forking manually on the fly.
/// Additional forks can be created and their state can be switched manually.
#[derive(Debug, Clone)]
pub struct Backend2 {
    /// The access point for managing forks
    forks: MultiFork,
    /// The database that holds the entire state, uses an internal database depending on current
    /// state
    pub db: CacheDB<Backend>,
    /// Contains snapshots made at a certain point
    snapshots: Snapshots<BackendSnapshot>,
}

// === impl Backend ===

impl Backend2 {
    /// Creates a new instance of `Backend`
    ///
    /// This will spawn a new background thread that manages forks and will establish a fork if
    /// `fork` is `Some`. If `fork` is `None` this `Backend` will launch with an in-memory
    /// Database
    pub fn new(fork: Option<Fork>, env: &Env) -> Self {
        todo!()
    }

    pub fn insert_snapshot(&self) -> U256 {
        todo!()
    }

    pub fn revert_snapshot(&mut self, id: U256) -> bool {
        todo!()
    }

    /// Creates a new fork but does _not_ select it
    pub fn create_fork(&mut self, fork: CreateFork) -> eyre::Result<ForkId> {
        self.forks.create_fork(fork)
    }

    /// Selects the fork's state
    ///
    /// **Note**: this does not change the local state, but swaps the remote state
    ///
    /// # Errors
    ///
    /// Returns an error if no fork with the given `id` exists
    pub fn select_fork(&mut self, id: ForkId) -> eyre::Result<()> {
        todo!()
    }
}

/// The Database that holds the state
#[derive(Debug, Clone)]
enum BackendDatabase {
    /// Backend is an in-memory `revm::Database`
    Memory(InMemoryDB),
    /// Backed is currently serving data from the remote endpoint identified by the `ForkId`
    Fork(SharedBackend, ForkId),
}

/// Represents a snapshot of the entire state
#[derive(Debug, Clone)]
enum BackendSnapshot {
    Memory(InMemoryDB),
    Fork(ForkDbSnapshot),
}

/// Variants of a [revm::Database]
#[derive(Debug, Clone)]
pub enum Backend {
    /// Simple in memory [revm::Database]
    Simple(EmptyDB),
    /// A [revm::Database] that forks of a remote location and can have multiple consumers of the
    /// same data
    Forked(SharedBackend),
    // TODO
}

impl Backend {
    /// Instantiates a new backend union based on whether there was or not a fork url specified
    pub async fn new(fork: Option<Fork>, env: &Env) -> Self {
        if let Some(fork) = fork {
            Backend::Forked(fork.spawn_backend(env).await)
        } else {
            Self::simple()
        }
    }

    /// Creates an empty in memory database
    pub fn simple() -> Self {
        Backend::Simple(EmptyDB::default())
    }
}

impl DatabaseRef for Backend {
    fn basic(&self, address: H160) -> AccountInfo {
        match self {
            Backend::Simple(inner) => inner.basic(address),
            Backend::Forked(inner) => inner.basic(address),
        }
    }

    fn code_by_hash(&self, address: H256) -> bytes::Bytes {
        match self {
            Backend::Simple(inner) => inner.code_by_hash(address),
            Backend::Forked(inner) => inner.code_by_hash(address),
        }
    }

    fn storage(&self, address: H160, index: U256) -> U256 {
        match self {
            Backend::Simple(inner) => inner.storage(address, index),
            Backend::Forked(inner) => inner.storage(address, index),
        }
    }

    fn block_hash(&self, number: U256) -> H256 {
        match self {
            Backend::Simple(inner) => inner.block_hash(number),
            Backend::Forked(inner) => inner.block_hash(number),
        }
    }
}
