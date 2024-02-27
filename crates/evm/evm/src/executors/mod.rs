//! EVM executor abstractions, which can execute calls.
//!
//! Used for running tests, scripts, and interacting with the inner backend which holds the state.

// TODO: The individual executors in this module should be moved into the respective crates, and the
// `Executor` struct should be accessed using a trait defined in `foundry-evm-core` instead of
// the concrete `Executor` type.

use crate::inspectors::{
    cheatcodes::BroadcastableTransactions, Cheatcodes, InspectorData, InspectorStack,
};
use alloy_dyn_abi::{DynSolValue, FunctionExt, JsonAbiExt};
use alloy_json_abi::Function;
use alloy_primitives::{Address, Bytes, Log, U256};
use foundry_common::{abi::IntoFunction, evm::Breakpoints};
use foundry_evm_core::{
    backend::{Backend, DatabaseError, DatabaseExt, DatabaseResult, FuzzBackendWrapper},
    constants::{
        CALLER, CHEATCODE_ADDRESS, DEFAULT_CREATE2_DEPLOYER, DEFAULT_CREATE2_DEPLOYER_CODE,
    },
    debug::DebugArena,
    decode::RevertDecoder,
    utils::StateChangeset,
};
use foundry_evm_coverage::HitMaps;
use foundry_evm_traces::CallTraceArena;
use revm::{
    db::{DatabaseCommit, DatabaseRef},
    interpreter::{return_ok, CreateScheme, InstructionResult},
    primitives::{
        BlockEnv, Bytecode, Env, EnvWithHandlerCfg, ExecutionResult, Output, ResultAndState,
        SpecId, TransactTo, TxEnv,
    },
};
use std::collections::HashMap;

mod builder;
pub use builder::ExecutorBuilder;

pub mod fuzz;
pub use fuzz::FuzzedExecutor;

pub mod invariant;
pub use invariant::InvariantExecutor;

mod tracing;
pub use tracing::TracingExecutor;

/// A type that can execute calls
///
/// The executor can be configured with various `revm::Inspector`s, like `Cheatcodes`.
///
/// There are two ways of executing calls:
/// - `committing`: any state changes made during the call are recorded and are persisting
/// - `raw`: state changes only exist for the duration of the call and are discarded afterwards, in
///   other words: the state of the underlying database remains unchanged.
#[derive(Clone, Debug)]
pub struct Executor {
    /// The underlying `revm::Database` that contains the EVM storage.
    // Note: We do not store an EVM here, since we are really
    // only interested in the database. REVM's `EVM` is a thin
    // wrapper around spawning a new EVM on every call anyway,
    // so the performance difference should be negligible.
    pub backend: Backend,
    /// The EVM environment.
    pub env: EnvWithHandlerCfg,
    /// The Revm inspector stack.
    pub inspector: InspectorStack,
    /// The gas limit for calls and deployments. This is different from the gas limit imposed by
    /// the passed in environment, as those limits are used by the EVM for certain opcodes like
    /// `gaslimit`.
    gas_limit: U256,
}

impl Executor {
    #[inline]
    pub fn new(
        mut backend: Backend,
        env: EnvWithHandlerCfg,
        inspector: InspectorStack,
        gas_limit: U256,
    ) -> Self {
        // Need to create a non-empty contract on the cheatcodes address so `extcodesize` checks
        // does not fail
        backend.insert_account_info(
            CHEATCODE_ADDRESS,
            revm::primitives::AccountInfo {
                code: Some(Bytecode::new_raw(Bytes::from_static(&[0])).to_checked()),
                ..Default::default()
            },
        );

        Executor { backend, env, inspector, gas_limit }
    }

    /// Creates the default CREATE2 Contract Deployer for local tests and scripts.
    pub fn deploy_create2_deployer(&mut self) -> eyre::Result<()> {
        trace!("deploying local create2 deployer");
        let create2_deployer_account = self
            .backend
            .basic_ref(DEFAULT_CREATE2_DEPLOYER)?
            .ok_or_else(|| DatabaseError::MissingAccount(DEFAULT_CREATE2_DEPLOYER))?;

        // if the deployer is not currently deployed, deploy the default one
        if create2_deployer_account.code.map_or(true, |code| code.is_empty()) {
            let creator = "0x3fAB184622Dc19b6109349B94811493BF2a45362".parse().unwrap();

            // Probably 0, but just in case.
            let initial_balance = self.get_balance(creator)?;

            self.set_balance(creator, U256::MAX)?;
            let res =
                self.deploy(creator, DEFAULT_CREATE2_DEPLOYER_CODE.into(), U256::ZERO, None)?;
            trace!(create2=?res.address, "deployed local create2 deployer");

            self.set_balance(creator, initial_balance)?;
        }
        Ok(())
    }

    /// Set the balance of an account.
    pub fn set_balance(&mut self, address: Address, amount: U256) -> DatabaseResult<&mut Self> {
        trace!(?address, ?amount, "setting account balance");
        let mut account = self.backend.basic_ref(address)?.unwrap_or_default();
        account.balance = amount;

        self.backend.insert_account_info(address, account);
        Ok(self)
    }

    /// Gets the balance of an account
    pub fn get_balance(&self, address: Address) -> DatabaseResult<U256> {
        Ok(self.backend.basic_ref(address)?.map(|acc| acc.balance).unwrap_or_default())
    }

    /// Set the nonce of an account.
    pub fn set_nonce(&mut self, address: Address, nonce: u64) -> DatabaseResult<&mut Self> {
        let mut account = self.backend.basic_ref(address)?.unwrap_or_default();
        account.nonce = nonce;

        self.backend.insert_account_info(address, account);
        Ok(self)
    }

    /// Gets the nonce of an account
    pub fn get_nonce(&self, address: Address) -> DatabaseResult<u64> {
        Ok(self.backend.basic_ref(address)?.map(|acc| acc.nonce).unwrap_or_default())
    }

    #[inline]
    pub fn set_tracing(&mut self, tracing: bool) -> &mut Self {
        self.inspector.tracing(tracing);
        self
    }

    #[inline]
    pub fn set_debugger(&mut self, debugger: bool) -> &mut Self {
        self.inspector.enable_debugger(debugger);
        self
    }

    #[inline]
    pub fn set_trace_printer(&mut self, trace_printer: bool) -> &mut Self {
        self.inspector.print(trace_printer);
        self
    }

    #[inline]
    pub fn set_gas_limit(&mut self, gas_limit: U256) -> &mut Self {
        self.gas_limit = gas_limit;
        self
    }

    /// Calls the `setUp()` function on a contract.
    ///
    /// This will commit any state changes to the underlying database.
    ///
    /// Ayn changes made during the setup call to env's block environment are persistent, for
    /// example `vm.chainId()` will change the `block.chainId` for all subsequent test calls.
    pub fn setup(&mut self, from: Option<Address>, to: Address) -> Result<CallResult, EvmError> {
        trace!(?from, ?to, "setting up contract");

        let from = from.unwrap_or(CALLER);
        self.backend.set_test_contract(to).set_caller(from);
        let res = self.call_committing::<_, _>(from, to, "setUp()", vec![], U256::ZERO, None)?;

        // record any changes made to the block's environment during setup
        self.env.block = res.env.block.clone();
        // and also the chainid, which can be set manually
        self.env.cfg.chain_id = res.env.cfg.chain_id;

        match res.state_changeset.as_ref() {
            Some(changeset) => {
                let success = self
                    .ensure_success(to, res.reverted, changeset.clone(), false)
                    .map_err(|err| EvmError::Eyre(eyre::eyre!(err.to_string())))?;
                if success {
                    Ok(res)
                } else {
                    Err(EvmError::Execution(Box::new(ExecutionErr {
                        reverted: res.reverted,
                        reason: "execution error".to_owned(),
                        traces: res.traces,
                        gas_used: res.gas_used,
                        gas_refunded: res.gas_refunded,
                        stipend: res.stipend,
                        logs: res.logs,
                        debug: res.debug,
                        labels: res.labels,
                        state_changeset: None,
                        transactions: None,
                    })))
                }
            }
            None => Ok(res),
        }
    }

    /// Performs a call to an account on the current state of the VM.
    ///
    /// The state after the call is persisted.
    pub fn call_committing<T: Into<Vec<DynSolValue>>, F: IntoFunction>(
        &mut self,
        from: Address,
        to: Address,
        func: F,
        args: T,
        value: U256,
        rd: Option<&RevertDecoder>,
    ) -> Result<CallResult, EvmError> {
        let func = func.into();
        let calldata = Bytes::from(func.abi_encode_input(&args.into())?);
        let result = self.call_raw_committing(from, to, calldata, value)?;
        convert_call_result(rd, &func, result)
    }

    /// Performs a raw call to an account on the current state of the VM.
    ///
    /// The state after the call is persisted.
    pub fn call_raw_committing(
        &mut self,
        from: Address,
        to: Address,
        calldata: Bytes,
        value: U256,
    ) -> eyre::Result<RawCallResult> {
        let env = self.build_test_env(from, TransactTo::Call(to), calldata, value);
        let mut result = self.call_raw_with_env(env)?;
        self.commit(&mut result);
        Ok(result)
    }

    /// Executes the test function call
    pub fn execute_test<T: Into<Vec<DynSolValue>>, F: IntoFunction>(
        &mut self,
        from: Address,
        test_contract: Address,
        func: F,
        args: T,
        value: U256,
        rd: Option<&RevertDecoder>,
    ) -> Result<CallResult, EvmError> {
        let func = func.into();
        let calldata = Bytes::from(func.abi_encode_input(&args.into())?.to_vec());

        // execute the call
        let env = self.build_test_env(from, TransactTo::Call(test_contract), calldata, value);
        let call_result = self.call_raw_with_env(env)?;
        convert_call_result(rd, &func, call_result)
    }

    /// Performs a call to an account on the current state of the VM.
    ///
    /// The state after the call is not persisted.
    pub fn call<T: Into<Vec<DynSolValue>>, F: IntoFunction>(
        &self,
        from: Address,
        to: Address,
        func: F,
        args: T,
        value: U256,
        rd: Option<&RevertDecoder>,
    ) -> Result<CallResult, EvmError> {
        let func = func.into();
        let calldata = Bytes::from(func.abi_encode_input(&args.into())?.to_vec());
        let call_result = self.call_raw(from, to, calldata, value)?;
        convert_call_result(rd, &func, call_result)
    }

    /// Performs a raw call to an account on the current state of the VM.
    ///
    /// Any state modifications made by the call are not committed.
    ///
    /// This intended for fuzz calls, which try to minimize [Backend] clones by using a Cow of the
    /// underlying [Backend] so it only gets cloned when cheatcodes that require mutable access are
    /// used.
    pub fn call_raw(
        &self,
        from: Address,
        to: Address,
        calldata: Bytes,
        value: U256,
    ) -> eyre::Result<RawCallResult> {
        let mut inspector = self.inspector.clone();
        // Build VM
        let env = self.build_test_env(from, TransactTo::Call(to), calldata, value);
        let mut db = FuzzBackendWrapper::new(&self.backend);
        let result = db.inspect_ref(env.clone(), &mut inspector)?;

        // Persist the snapshot failure recorded on the fuzz backend wrapper.
        let has_snapshot_failure = db.has_snapshot_failure();
        convert_executed_result(env, inspector, result, has_snapshot_failure)
    }

    /// Execute the transaction configured in `env.tx` and commit the changes
    pub fn commit_tx_with_env(&mut self, env: EnvWithHandlerCfg) -> eyre::Result<RawCallResult> {
        let mut result = self.call_raw_with_env(env)?;
        self.commit(&mut result);
        Ok(result)
    }

    /// Execute the transaction configured in `env.tx`
    pub fn call_raw_with_env(&mut self, env: EnvWithHandlerCfg) -> eyre::Result<RawCallResult> {
        // execute the call
        let mut inspector = self.inspector.clone();
        let result = self.backend.inspect_ref(env.clone(), &mut inspector)?;
        convert_executed_result(env, inspector, result, self.backend.has_snapshot_failure())
    }

    /// Commit the changeset to the database and adjust `self.inspector_config`
    /// values according to the executed call result
    fn commit(&mut self, result: &mut RawCallResult) {
        // Persist changes to db
        if let Some(changes) = &result.state_changeset {
            self.backend.commit(changes.clone());
        }

        // Persist cheatcode state
        let mut cheatcodes = result.cheatcodes.take();
        if let Some(cheats) = cheatcodes.as_mut() {
            // Clear broadcastable transactions
            cheats.broadcastable_transactions.clear();
            debug!(target: "evm::executors", "cleared broadcastable transactions");

            // corrected_nonce value is needed outside of this context (setUp), so we don't
            // reset it.
        }
        self.inspector.cheatcodes = cheatcodes;

        // Persist the changed environment
        self.inspector.set_env(&result.env);
    }

    /// Deploys a contract using the given `env` and commits the new state to the underlying
    /// database
    pub fn deploy_with_env(
        &mut self,
        env: EnvWithHandlerCfg,
        rd: Option<&RevertDecoder>,
    ) -> Result<DeployResult, EvmError> {
        debug_assert!(
            matches!(env.tx.transact_to, TransactTo::Create(_)),
            "Expect create transaction"
        );
        trace!(sender=?env.tx.caller, "deploying contract");

        let mut result = self.call_raw_with_env(env)?;
        self.commit(&mut result);

        let RawCallResult {
            exit_reason,
            out,
            gas_used,
            gas_refunded,
            logs,
            labels,
            traces,
            debug,
            env,
            coverage,
            ..
        } = result;

        let result = match &out {
            Some(Output::Create(data, _)) => data.to_owned(),
            _ => Bytes::default(),
        };

        let address = match exit_reason {
            return_ok!() => {
                if let Some(Output::Create(_, Some(addr))) = out {
                    addr
                } else {
                    return Err(EvmError::Execution(Box::new(ExecutionErr {
                        reverted: true,
                        reason: "Deployment succeeded, but no address was returned. This is a bug, please report it".to_string(),
                        traces,
                        gas_used,
                        gas_refunded: 0,
                        stipend: 0,
                        logs,
                        debug,
                        labels,
                        state_changeset: None,
                        transactions: None,
                    })));
                }
            }
            _ => {
                let reason = rd.unwrap_or_default().decode(&result, Some(exit_reason));
                return Err(EvmError::Execution(Box::new(ExecutionErr {
                    reverted: true,
                    reason,
                    traces,
                    gas_used,
                    gas_refunded,
                    stipend: 0,
                    logs,
                    debug,
                    labels,
                    state_changeset: None,
                    transactions: None,
                })))
            }
        };

        // also mark this library as persistent, this will ensure that the state of the library is
        // persistent across fork swaps in forking mode
        self.backend.add_persistent_account(address);

        trace!(address=?address, "deployed contract");

        Ok(DeployResult { address, gas_used, gas_refunded, logs, traces, debug, env, coverage })
    }

    /// Deploys a contract and commits the new state to the underlying database.
    ///
    /// Executes a CREATE transaction with the contract `code` and persistent database state
    /// modifications
    pub fn deploy(
        &mut self,
        from: Address,
        code: Bytes,
        value: U256,
        rd: Option<&RevertDecoder>,
    ) -> Result<DeployResult, EvmError> {
        let env = self.build_test_env(from, TransactTo::Create(CreateScheme::Create), code, value);
        self.deploy_with_env(env, rd)
    }

    /// Check if a call to a test contract was successful.
    ///
    /// This function checks both the VM status of the call, DSTest's `failed` status and the
    /// `globalFailed` flag which is stored in `failed` inside the `CHEATCODE_ADDRESS` contract.
    ///
    /// DSTest will not revert inside its `assertEq`-like functions which allows
    /// to test multiple assertions in 1 test function while also preserving logs.
    ///
    /// If an `assert` is violated, the contract's `failed` variable is set to true, and the
    /// `globalFailure` flag inside the `CHEATCODE_ADDRESS` is also set to true, this way, failing
    /// asserts from any contract are tracked as well.
    ///
    /// In order to check whether a test failed, we therefore need to evaluate the contract's
    /// `failed` variable and the `globalFailure` flag, which happens by calling
    /// `contract.failed()`.
    pub fn is_success(
        &self,
        address: Address,
        reverted: bool,
        state_changeset: StateChangeset,
        should_fail: bool,
    ) -> bool {
        self.ensure_success(address, reverted, state_changeset, should_fail).unwrap_or_default()
    }

    /// This is the same as [Self::is_success] but intended for outcomes of [Self::call_raw] used in
    /// fuzzing and invariant testing.
    ///
    /// ## Background
    ///
    /// Executing and failure checking [Executor::ensure_success] are two steps, for ds-test
    /// legacy reasons failures can be stored in a global variables and needs to be called via a
    /// solidity call `failed()(bool)`. For fuzz tests we’re using the
    /// `FuzzBackendWrapper` which is a Cow of the executor’s backend which lazily clones the
    /// backend when it’s mutated via cheatcodes like `snapshot`. Snapshots make it even
    /// more complicated because now we also need to keep track of that global variable when we
    /// revert to a snapshot (because it is stored in state). Now, the problem is that
    /// the `FuzzBackendWrapper` is dropped after every call, so we need to keep track of the
    /// snapshot failure in the [RawCallResult] instead.
    pub fn is_raw_call_success(
        &self,
        address: Address,
        state_changeset: StateChangeset,
        call_result: &RawCallResult,
        should_fail: bool,
    ) -> bool {
        if call_result.has_snapshot_failure {
            // a failure occurred in a reverted snapshot, which is considered a failed test
            return should_fail
        }
        self.is_success(address, call_result.reverted, state_changeset, should_fail)
    }

    fn ensure_success(
        &self,
        address: Address,
        reverted: bool,
        state_changeset: StateChangeset,
        should_fail: bool,
    ) -> Result<bool, DatabaseError> {
        if self.backend.has_snapshot_failure() {
            // a failure occurred in a reverted snapshot, which is considered a failed test
            return Ok(should_fail)
        }

        // Construct a new VM with the state changeset
        let mut backend = self.backend.clone_empty();

        // we only clone the test contract and cheatcode accounts, that's all we need to evaluate
        // success
        for addr in [address, CHEATCODE_ADDRESS] {
            let acc = self.backend.basic_ref(addr)?.unwrap_or_default();
            backend.insert_account_info(addr, acc);
        }

        // If this test failed any asserts, then this changeset will contain changes `false -> true`
        // for the contract's `failed` variable and the `globalFailure` flag in the state of the
        // cheatcode address which are both read when we call `"failed()(bool)"` in the next step
        backend.commit(state_changeset);

        let mut success = !reverted;
        if success {
            // Check if a DSTest assertion failed
            let executor =
                Executor::new(backend, self.env.clone(), self.inspector.clone(), self.gas_limit);
            let call = executor.call(CALLER, address, "failed()(bool)", vec![], U256::ZERO, None);
            if let Ok(CallResult { result: failed, .. }) = call {
                debug!(?failed, "DSTest");
                success = !failed.as_bool().unwrap();
            }
        }

        let result = should_fail ^ success;
        debug!(should_fail, success, result);
        Ok(result)
    }

    /// Creates the environment to use when executing a transaction in a test context
    ///
    /// If using a backend with cheatcodes, `tx.gas_price` and `block.number` will be overwritten by
    /// the cheatcode state inbetween calls.
    fn build_test_env(
        &self,
        caller: Address,
        transact_to: TransactTo,
        data: Bytes,
        value: U256,
    ) -> EnvWithHandlerCfg {
        let env = Env {
            cfg: self.env.cfg.clone(),
            // We always set the gas price to 0 so we can execute the transaction regardless of
            // network conditions - the actual gas price is kept in `self.block` and is applied by
            // the cheatcode handler if it is enabled
            block: BlockEnv {
                basefee: U256::ZERO,
                gas_limit: self.gas_limit,
                ..self.env.block.clone()
            },
            tx: TxEnv {
                caller,
                transact_to,
                data,
                value,
                // As above, we set the gas price to 0.
                gas_price: U256::ZERO,
                gas_priority_fee: None,
                gas_limit: self.gas_limit.to(),
                ..self.env.tx.clone()
            },
        };

        EnvWithHandlerCfg::new_with_spec_id(Box::new(env), self.env.handler_cfg.spec_id)
    }
}

/// Represents the context after an execution error occurred.
#[derive(Debug, thiserror::Error)]
#[error("Execution reverted: {reason} (gas: {gas_used})")]
pub struct ExecutionErr {
    pub reverted: bool,
    pub reason: String,
    pub gas_used: u64,
    pub gas_refunded: u64,
    pub stipend: u64,
    pub logs: Vec<Log>,
    pub traces: Option<CallTraceArena>,
    pub debug: Option<DebugArena>,
    pub labels: HashMap<Address, String>,
    pub transactions: Option<BroadcastableTransactions>,
    pub state_changeset: Option<StateChangeset>,
}

#[derive(Debug, thiserror::Error)]
pub enum EvmError {
    /// Error which occurred during execution of a transaction
    #[error(transparent)]
    Execution(Box<ExecutionErr>),
    /// Error which occurred during ABI encoding/decoding
    #[error(transparent)]
    AbiError(#[from] alloy_dyn_abi::Error),
    /// Error caused which occurred due to calling the skip() cheatcode.
    #[error("Skipped")]
    SkipError,
    /// Any other error.
    #[error(transparent)]
    Eyre(#[from] eyre::Error),
}

/// The result of a deployment.
#[derive(Debug)]
pub struct DeployResult {
    /// The address of the deployed contract
    pub address: Address,
    /// The gas cost of the deployment
    pub gas_used: u64,
    /// The refunded gas
    pub gas_refunded: u64,
    /// The logs emitted during the deployment
    pub logs: Vec<Log>,
    /// The traces of the deployment
    pub traces: Option<CallTraceArena>,
    /// The debug nodes of the call
    pub debug: Option<DebugArena>,
    /// The `revm::Env` after deployment
    pub env: EnvWithHandlerCfg,
    /// The coverage info collected during the deployment
    pub coverage: Option<HitMaps>,
}

/// The result of a call.
#[derive(Debug)]
pub struct CallResult {
    pub skipped: bool,
    /// Whether the call reverted or not
    pub reverted: bool,
    /// The decoded result of the call
    pub result: DynSolValue,
    /// The gas used for the call
    pub gas_used: u64,
    /// The refunded gas for the call
    pub gas_refunded: u64,
    /// The initial gas stipend for the transaction
    pub stipend: u64,
    /// The logs emitted during the call
    pub logs: Vec<Log>,
    /// The labels assigned to addresses during the call
    pub labels: HashMap<Address, String>,
    /// The traces of the call
    pub traces: Option<CallTraceArena>,
    /// The coverage info collected during the call
    pub coverage: Option<HitMaps>,
    /// The debug nodes of the call
    pub debug: Option<DebugArena>,
    /// Scripted transactions generated from this call
    pub transactions: Option<BroadcastableTransactions>,
    /// The changeset of the state.
    ///
    /// This is only present if the changed state was not committed to the database (i.e. if you
    /// used `call` and `call_raw` not `call_committing` or `call_raw_committing`).
    pub state_changeset: Option<StateChangeset>,
    /// The `revm::Env` after the call
    pub env: EnvWithHandlerCfg,
    /// breakpoints
    pub breakpoints: Breakpoints,
}

/// The result of a raw call.
#[derive(Debug)]
pub struct RawCallResult {
    /// The status of the call
    pub exit_reason: InstructionResult,
    /// Whether the call reverted or not
    pub reverted: bool,
    /// Whether the call includes a snapshot failure
    ///
    /// This is tracked separately from revert because a snapshot failure can occur without a
    /// revert, since assert failures are stored in a global variable (ds-test legacy)
    pub has_snapshot_failure: bool,
    /// The raw result of the call
    pub result: Bytes,
    /// The gas used for the call
    pub gas_used: u64,
    /// Refunded gas
    pub gas_refunded: u64,
    /// The initial gas stipend for the transaction
    pub stipend: u64,
    /// The logs emitted during the call
    pub logs: Vec<Log>,
    /// The labels assigned to addresses during the call
    pub labels: HashMap<Address, String>,
    /// The traces of the call
    pub traces: Option<CallTraceArena>,
    /// The coverage info collected during the call
    pub coverage: Option<HitMaps>,
    /// The debug nodes of the call
    pub debug: Option<DebugArena>,
    /// Scripted transactions generated from this call
    pub transactions: Option<BroadcastableTransactions>,
    /// The changeset of the state.
    ///
    /// This is only present if the changed state was not committed to the database (i.e. if you
    /// used `call` and `call_raw` not `call_committing` or `call_raw_committing`).
    pub state_changeset: Option<StateChangeset>,
    /// The `revm::Env` after the call
    pub env: EnvWithHandlerCfg,
    /// The cheatcode states after execution
    pub cheatcodes: Option<Cheatcodes>,
    /// The raw output of the execution
    pub out: Option<Output>,
    /// The chisel state
    pub chisel_state: Option<(Vec<U256>, Vec<u8>, InstructionResult)>,
}

impl Default for RawCallResult {
    fn default() -> Self {
        Self {
            exit_reason: InstructionResult::Continue,
            reverted: false,
            has_snapshot_failure: false,
            result: Bytes::new(),
            gas_used: 0,
            gas_refunded: 0,
            stipend: 0,
            logs: Vec::new(),
            labels: HashMap::new(),
            traces: None,
            coverage: None,
            debug: None,
            transactions: None,
            state_changeset: None,
            env: EnvWithHandlerCfg::new_with_spec_id(Box::default(), SpecId::LATEST),
            cheatcodes: Default::default(),
            out: None,
            chisel_state: None,
        }
    }
}

/// Calculates the initial gas stipend for a transaction
fn calc_stipend(calldata: &[u8], spec: SpecId) -> u64 {
    let non_zero_data_cost = if SpecId::enabled(spec, SpecId::ISTANBUL) { 16 } else { 68 };
    calldata.iter().fold(21000, |sum, byte| sum + if *byte == 0 { 4 } else { non_zero_data_cost })
}

/// Converts the data aggregated in the `inspector` and `call` to a `RawCallResult`
fn convert_executed_result(
    env: EnvWithHandlerCfg,
    inspector: InspectorStack,
    result: ResultAndState,
    has_snapshot_failure: bool,
) -> eyre::Result<RawCallResult> {
    let ResultAndState { result: exec_result, state: state_changeset } = result;
    let (exit_reason, gas_refunded, gas_used, out) = match exec_result {
        ExecutionResult::Success { reason, gas_used, gas_refunded, output, .. } => {
            (reason.into(), gas_refunded, gas_used, Some(output))
        }
        ExecutionResult::Revert { gas_used, output } => {
            // Need to fetch the unused gas
            (InstructionResult::Revert, 0_u64, gas_used, Some(Output::Call(output)))
        }
        ExecutionResult::Halt { reason, gas_used } => (reason.into(), 0_u64, gas_used, None),
    };
    let stipend = calc_stipend(&env.tx.data, env.handler_cfg.spec_id);

    let result = match &out {
        Some(Output::Call(data)) => data.clone(),
        _ => Bytes::new(),
    };

    let InspectorData { logs, labels, traces, coverage, debug, cheatcodes, chisel_state } =
        inspector.collect();

    let transactions = match cheatcodes.as_ref() {
        Some(cheats) if !cheats.broadcastable_transactions.is_empty() => {
            Some(cheats.broadcastable_transactions.clone())
        }
        _ => None,
    };

    Ok(RawCallResult {
        exit_reason,
        reverted: !matches!(exit_reason, return_ok!()),
        has_snapshot_failure,
        result,
        gas_used,
        gas_refunded,
        stipend,
        logs,
        labels,
        traces,
        coverage,
        debug,
        transactions,
        state_changeset: Some(state_changeset),
        env,
        cheatcodes,
        out,
        chisel_state,
    })
}

fn convert_call_result(
    rd: Option<&RevertDecoder>,
    func: &Function,
    call_result: RawCallResult,
) -> Result<CallResult, EvmError> {
    let RawCallResult {
        result,
        exit_reason: status,
        reverted,
        gas_used,
        gas_refunded,
        stipend,
        logs,
        labels,
        traces,
        coverage,
        debug,
        transactions,
        state_changeset,
        env,
        ..
    } = call_result;

    let breakpoints = if let Some(c) = call_result.cheatcodes {
        c.breakpoints
    } else {
        std::collections::HashMap::new()
    };

    match status {
        return_ok!() => {
            let mut result = func.abi_decode_output(&result, false)?;
            let res = if result.len() == 1 {
                result.pop().unwrap()
            } else {
                // combine results into a tuple
                DynSolValue::Tuple(result)
            };
            Ok(CallResult {
                reverted,
                result: res,
                gas_used,
                gas_refunded,
                stipend,
                logs,
                labels,
                traces,
                coverage,
                debug,
                transactions,
                state_changeset,
                env,
                breakpoints,
                skipped: false,
            })
        }
        _ => {
            if &result == crate::constants::MAGIC_SKIP {
                return Err(EvmError::SkipError)
            }
            let reason = rd.unwrap_or_default().decode(&result, Some(status));
            Err(EvmError::Execution(Box::new(ExecutionErr {
                reverted,
                reason,
                gas_used,
                gas_refunded,
                stipend,
                logs,
                traces,
                debug,
                labels,
                transactions,
                state_changeset,
            })))
        }
    }
}
