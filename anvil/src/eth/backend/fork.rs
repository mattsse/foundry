//! Support for forking off another client

use ethers::{
    prelude::{Http, Provider},
    types::H256,
};
use std::{collections::HashMap, sync::Arc};

use crate::eth::{backend::db::ForkedDatabase, error::BlockchainError};

use ethers::{
    prelude::BlockNumber,
    providers::{Middleware, ProviderError},
    types::{
        Address, Block, Bytes, Filter, Log, Trace, Transaction, TransactionReceipt, TxHash, U256,
    },
};
use foundry_evm::utils::u256_to_h256_le;
use parking_lot::{
    lock_api::{RwLockReadGuard, RwLockWriteGuard},
    RawRwLock, RwLock,
};
use tracing::trace;

/// Represents a fork of a remote client
///
/// This type contains a subset of the [`EthApi`](crate::eth::EthApi) functions but will exclusively
/// fetch the requested data from the remote client, if it wasn't already fetched.
#[derive(Debug, Clone)]
pub struct ClientFork {
    /// Contains the cached data
    pub storage: Arc<RwLock<ForkedStorage>>,
    /// contains the info how the fork is configured
    // Wrapping this in a lock, ensures we can update this on the fly via additional custom RPC
    // endpoints
    pub config: Arc<RwLock<ClientForkConfig>>,
    /// This also holds a handle to the underlying database
    pub database: ForkedDatabase,
}

// === impl ClientFork ===

impl ClientFork {
    /// Reset the fork to a fresh forked state, and optionally update the fork config
    pub fn reset(
        &self,
        url: Option<String>,
        block_number: Option<u64>,
    ) -> Result<(), BlockchainError> {
        self.database.reset(url.clone(), block_number)?;
        self.config.write().update(url, block_number)?;
        self.clear_cached_storage();
        Ok(())
    }

    /// Removes all data cached from previous responses
    pub fn clear_cached_storage(&self) {
        self.storage.write().clear()
    }

    /// Returns true whether the block predates the fork
    pub fn predates_fork(&self, block: u64) -> bool {
        block <= self.block_number()
    }

    pub fn block_number(&self) -> u64 {
        self.config.read().block_number
    }

    pub fn block_hash(&self) -> H256 {
        self.config.read().block_hash
    }

    pub fn eth_rpc_url(&self) -> String {
        self.config.read().eth_rpc_url.clone()
    }

    pub fn chain_id(&self) -> u64 {
        self.config.read().chain_id
    }

    fn provider(&self) -> Arc<Provider<Http>> {
        self.config.read().provider.clone()
    }

    fn storage_read(&self) -> RwLockReadGuard<'_, RawRwLock, ForkedStorage> {
        self.storage.read()
    }

    fn storage_write(&self) -> RwLockWriteGuard<'_, RawRwLock, ForkedStorage> {
        self.storage.write()
    }

    pub async fn storage_at(
        &self,
        address: Address,
        index: U256,
        number: Option<BlockNumber>,
    ) -> Result<H256, ProviderError> {
        let index = u256_to_h256_le(index);
        self.provider().get_storage_at(address, index, number.map(Into::into)).await
    }

    pub async fn logs(&self, filter: &Filter) -> Result<Vec<Log>, ProviderError> {
        self.provider().get_logs(filter).await
    }

    pub async fn get_code(
        &self,
        address: Address,
        blocknumber: u64,
    ) -> Result<Bytes, ProviderError> {
        trace!(target: "backend::fork", "get_code={:?}", address);
        if let Some(code) = self.storage_read().code_at.get(&(address, blocknumber)).cloned() {
            return Ok(code)
        }

        let code = self.provider().get_code(address, Some(blocknumber.into())).await?;
        let mut storage = self.storage_write();
        storage.code_at.insert((address, blocknumber), code.clone());

        Ok(code)
    }

    pub async fn get_balance(
        &self,
        address: Address,
        blocknumber: u64,
    ) -> Result<U256, ProviderError> {
        trace!(target: "backend::fork", "get_balance={:?}", address);
        self.provider().get_balance(address, Some(blocknumber.into())).await
    }

    pub async fn get_nonce(
        &self,
        address: Address,
        blocknumber: u64,
    ) -> Result<U256, ProviderError> {
        trace!(target: "backend::fork", "get_nonce={:?}", address);
        self.provider().get_transaction_count(address, Some(blocknumber.into())).await
    }

    pub async fn transaction_by_block_number_and_index(
        &self,
        number: u64,
        index: usize,
    ) -> Result<Option<Transaction>, ProviderError> {
        if let Some(block) = self.block_by_number(number).await? {
            if let Some(tx_hash) = block.transactions.get(index) {
                return self.transaction_by_hash(*tx_hash).await
            }
        }
        Ok(None)
    }

    pub async fn transaction_by_block_hash_and_index(
        &self,
        hash: H256,
        index: usize,
    ) -> Result<Option<Transaction>, ProviderError> {
        if let Some(block) = self.block_by_hash(hash).await? {
            if let Some(tx_hash) = block.transactions.get(index) {
                return self.transaction_by_hash(*tx_hash).await
            }
        }
        Ok(None)
    }

    pub async fn transaction_by_hash(
        &self,
        hash: H256,
    ) -> Result<Option<Transaction>, ProviderError> {
        trace!(target: "backend::fork", "transaction_by_hash={:?}", hash);
        if let tx @ Some(_) = self.storage_read().transactions.get(&hash).cloned() {
            return Ok(tx)
        }

        if let Some(tx) = self.provider().get_transaction(hash).await? {
            let mut storage = self.storage_write();
            storage.transactions.insert(hash, tx.clone());
            return Ok(Some(tx))
        }
        Ok(None)
    }

    pub async fn trace_transaction(&self, hash: H256) -> Result<Vec<Trace>, ProviderError> {
        if let Some(traces) = self.storage_read().transaction_traces.get(&hash).cloned() {
            return Ok(traces)
        }

        let traces = self.provider().trace_transaction(hash).await?;
        let mut storage = self.storage_write();
        storage.transaction_traces.insert(hash, traces.clone());

        Ok(traces)
    }

    pub async fn trace_block(&self, number: u64) -> Result<Vec<Trace>, ProviderError> {
        if let Some(traces) = self.storage_read().block_traces.get(&number).cloned() {
            return Ok(traces)
        }

        let traces = self.provider().trace_block(number.into()).await?;
        let mut storage = self.storage_write();
        storage.block_traces.insert(number, traces.clone());

        Ok(traces)
    }

    pub async fn transaction_receipt(
        &self,
        hash: H256,
    ) -> Result<Option<TransactionReceipt>, ProviderError> {
        if let Some(receipt) = self.storage_read().transaction_receipts.get(&hash).cloned() {
            return Ok(Some(receipt))
        }

        if let Some(receipt) = self.provider().get_transaction_receipt(hash).await? {
            let mut storage = self.storage_write();
            storage.transaction_receipts.insert(hash, receipt.clone());
            return Ok(Some(receipt))
        }

        Ok(None)
    }

    pub async fn block_by_hash(&self, hash: H256) -> Result<Option<Block<TxHash>>, ProviderError> {
        if let Some(block) = self.storage_read().blocks.get(&hash).cloned() {
            return Ok(Some(block))
        }

        if let Some(block) = self.provider().get_block(hash).await? {
            let number = block.number.unwrap().as_u64();
            let mut storage = self.storage_write();
            storage.hashes.insert(number, hash);
            storage.blocks.insert(hash, block.clone());
            return Ok(Some(block))
        }
        Ok(None)
    }

    pub async fn block_by_number(
        &self,
        number: u64,
    ) -> Result<Option<Block<TxHash>>, ProviderError> {
        if let Some(block) = self
            .storage_read()
            .hashes
            .get(&number)
            .copied()
            .and_then(|hash| self.storage_read().blocks.get(&hash).cloned())
        {
            return Ok(Some(block))
        }

        if let Some(block) = self.provider().get_block(number).await? {
            let hash = block.hash.unwrap();
            let mut storage = self.storage_write();
            storage.hashes.insert(number, hash);
            storage.blocks.insert(hash, block.clone());
            return Ok(Some(block))
        }

        Ok(None)
    }
}

/// Contains all fork metadata
#[derive(Debug, Clone)]
pub struct ClientForkConfig {
    pub eth_rpc_url: String,
    pub block_number: u64,
    pub block_hash: H256,
    // TODO make provider agnostic
    pub provider: Arc<Provider<Http>>,
    pub chain_id: u64,
}

// === impl ClientForkConfig ===

impl ClientForkConfig {
    /// Updates the forking metadata
    ///
    /// # Errors
    ///
    /// This will fail if no new provider could be established (erroneous URL)
    pub fn update(
        &mut self,
        url: Option<String>,
        block_number: Option<u64>,
    ) -> Result<(), BlockchainError> {
        if let Some(url) = url {
            self.provider = Arc::new(
                Provider::try_from(&url).map_err(|_| BlockchainError::InvalidUrl(url.clone()))?,
            );
            trace!(target: "fork", "Updated rpc url  {}", url);
            self.eth_rpc_url = url;
        }
        if let Some(block_number) = block_number {
            self.block_number = block_number;
            trace!(target: "fork", "Updated block number {}", block_number);
        }

        Ok(())
    }
}

/// Contains cached state fetched to serve EthApi requests
#[derive(Debug, Clone, Default)]
pub struct ForkedStorage {
    pub blocks: HashMap<H256, Block<TxHash>>,
    pub hashes: HashMap<u64, H256>,
    pub transactions: HashMap<H256, Transaction>,
    pub transaction_receipts: HashMap<H256, TransactionReceipt>,
    pub transaction_traces: HashMap<H256, Vec<Trace>>,
    pub block_traces: HashMap<u64, Vec<Trace>>,
    pub code_at: HashMap<(Address, u64), Bytes>,
}

// === impl ForkedStorage ===

impl ForkedStorage {
    /// Clears all data
    pub fn clear(&mut self) {
        // simply replace with a completely new, empty instance
        *self = Self::default()
    }
}
