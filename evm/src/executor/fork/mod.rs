mod backend;

pub use backend::{BackendHandler, SharedBackend};
use ethers::types::BlockNumber;
use revm::Env;

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
    /// Whether to enable rpc storage caching for this fork
    pub enable_caching: bool,
    /// The URL to a node for fetching remote state
    pub url: String,
    /// The block to fork against
    pub block: BlockNumber,
    /// chain id to use, if `None` then the chain_id will be fetched from the endpoint
    pub chain_id: Option<u64>,
    /// The env to create this fork, main purpose is to provide some metadata for the fork
    pub env: Env,
}
