// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice A channel's native-ETH inbox. Its verified owner can withdraw only to itself.
/// @dev The factory can set the owner once; there is no administrator withdrawal or upgrade.
contract BroadcastVault {
    error Unauthorized();
    error AlreadyClaimed();
    error TransferFailed();
    error ReentrantCall();

    address public immutable factory;
    bytes32 public immutable channelKey;
    address public owner;
    bool private withdrawing;

    event Received(address indexed sender, uint256 amount);
    event Claimed(address indexed owner);
    event Withdrawn(address indexed owner, uint256 amount);

    constructor(bytes32 key) {
        factory = msg.sender;
        channelKey = key;
    }

    receive() external payable {
        emit Received(msg.sender, msg.value);
    }

    function claim(address claimant) external {
        if (msg.sender != factory || claimant == address(0)) revert Unauthorized();
        if (owner != address(0)) revert AlreadyClaimed();
        owner = claimant;
        emit Claimed(claimant);
    }

    function withdraw() external {
        if (msg.sender != owner) revert Unauthorized();
        if (withdrawing) revert ReentrantCall();
        withdrawing = true;
        uint256 amount = address(this).balance;
        (bool success,) = payable(owner).call{value: amount}("");
        if (!success) revert TransferFailed();
        withdrawing = false;
        emit Withdrawn(owner, amount);
    }
}
