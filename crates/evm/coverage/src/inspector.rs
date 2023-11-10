use crate::{HitMap, HitMaps};
use alloy_primitives::Bytes;
use revm::{
    interpreter::{InstructionResult, Interpreter},
    Database, EvmContext, Inspector,
};

#[derive(Clone, Default, Debug)]
pub struct CoverageCollector {
    /// Maps that track instruction hit data.
    pub maps: HitMaps,
}

impl<DB: Database> Inspector<DB> for CoverageCollector {
    #[inline]
    fn initialize_interp(&mut self, interpreter: &mut Interpreter, _: &mut EvmContext<'_, DB>) {
        let hash = interpreter.contract.hash;
        self.maps.entry(hash).or_insert_with(|| {
            HitMap::new(Bytes::copy_from_slice(
                interpreter.contract.bytecode.original_bytecode_slice(),
            ))
        });
    }

    #[inline]
    fn step(&mut self, interpreter: &mut Interpreter, _: &mut EvmContext<'_, DB>) {
        let hash = interpreter.contract.hash;
        self.maps.entry(hash).and_modify(|map| map.hit(interpreter.program_counter()));
    }
}
