// SPDX-License-Identifier: Unlicense
pragma solidity >=0.8.0;

import "forge-std/Test.sol";
import "./Cheats.sol";

contract WarpTest is Test {
    Cheats constant cheats = Cheats(HEVM_ADDRESS);

    function testWarp() public {
        cheats.warp(10);
        assertEq(block.timestamp, 10, "warp failed");
    }

    function testWarpFuzzed(uint128 jump) public {
        uint pre = block.timestamp;
        cheats.warp(block.timestamp + jump);
        assertEq(block.timestamp, pre + jump, "warp failed");
    }

    function testWarp2() public {
        assertEq(block.timestamp, 1);
        cheats.warp(100);
        assertEq(block.timestamp, 100);
    }
}
