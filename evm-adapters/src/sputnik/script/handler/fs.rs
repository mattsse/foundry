//! Filesystem manipulation operations for solidity.

use crate::sputnik::script::handler::ScriptStackExecutor;
use ethers_core::types::H160;
use sputnik::{backend::Backend, executor::stack::PrecompileSet, Capture, ExitReason, ExitSucceed};
use std::{collections::HashMap, convert::Infallible, fs::File};

impl<'a, 'b, Back: Backend, Pre: PrecompileSet + 'b> ScriptStackExecutor<'a, 'b, Back, Pre> {
    pub(crate) fn on_fs_call(
        &mut self,
        call: ForgeFsCalls,
        caller: H160,
    ) -> Capture<(ExitReason, Vec<u8>), Infallible> {
        let mut res = Vec::new();
        match call {
            ForgeFsCalls::Create(path) => {}
            ForgeFsCalls::Write(call) => {}
        }

        Capture::Exit((ExitReason::Succeed(ExitSucceed::Stopped), res))
    }
}

/// Manages the state of the solidity `Fs` lib
#[derive(Debug, Default)]
pub struct FsManager {
    /// tracks all open files
    files: HashMap<usize, File>,
    /// counter used to determine the next file id
    file_ctn: usize,
}

ethers::contract::abigen!(
    ForgeFs,
    r#"[
            struct File { uint256 id; string path;}
            create(string)(File)
            write(File, string)(uint256)
    ]"#,
);
