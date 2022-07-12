// SPDX-License-Identifier: Unlicense
pragma solidity >=0.8.0;

import "forge-std/Test.sol";
import "./Cheats.sol";

contract FfiTest is Test {
    Cheats constant cheats = Cheats(HEVM_ADDRESS);

    function testFfi() public {
        string[] memory inputs = new string[](3);
        inputs[0] = "bash";
        inputs[1] = "-c";
        inputs[2] = "echo -n 0x0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000966666920776f726b730000000000000000000000000000000000000000000000";

        bytes memory res = cheats.ffi(inputs);
        (string memory output) = abi.decode(res, (string));
        assertEq(output, "ffi works", "ffi failed");
    }
}
