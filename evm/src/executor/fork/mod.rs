mod backend;

pub use backend::{BackendHandler, SharedBackend};
use ethers::types::BlockNumber;
use std::path::PathBuf;

mod init;
pub use init::environment;

mod cache;
pub use cache::{BlockchainDb, BlockchainDbMeta, JsonBlockCacheDB, MemDb};

pub mod database;

mod multi;
pub use multi::{ForkId, MultiFork, MultiForkHandler};

/// Represents a _fork_ of a remote chain whose data is available only via the `url` endpoint.
#[derive(Debug)]
pub struct CreateFork {
    /// Where to read the cached storage from
    pub cache_path: Option<PathBuf>,
    /// The URL to a node for fetching remote state
    pub url: String,
    /// The block to fork against
    pub block: BlockNumber,
}
