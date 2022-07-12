// SPDX-License-Identifier: Unlicense
pragma solidity >=0.8.0;

import "forge-std/Test.sol";
import "./Cheats.sol";

contract RollTest is Test {
    Cheats constant cheats = Cheats(HEVM_ADDRESS);

    function testRoll() public {
        cheats.roll(10);
        assertEq(block.number, 10, "roll failed");
    }

    function testRollFuzzed(uint128 jump) public {
        uint pre = block.number;
        cheats.roll(block.number + jump);
        assertEq(block.number, pre + jump, "roll failed");
    }

    function testRollHash() public {
        assertEq(blockhash(block.number), 0x0, "initial block hash is incorrect");

        cheats.roll(5);
        bytes32 hash = blockhash(5);
        assertTrue(blockhash(4) != 0x0, "new block hash is incorrect");

        cheats.roll(10);
        assertTrue(blockhash(5) != blockhash(10), "block hash collision");

        cheats.roll(5);
        assertEq(blockhash(5), hash, "block 5 changed hash");
    }
}
