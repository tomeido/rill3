// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {BroadcastVault} from "./BroadcastVault.sol";

/// @notice Deterministic channel inboxes with a one-time identity-attested claim.
/// @dev The attestor is trusted to verify the platform account before the first claim.
contract BroadcastVaultFactory {
    error InvalidAttestor();
    error InvalidSignature();
    error ExpiredAttestation();
    error WrongClaimant();
    error TransferFailed();

    // secp256k1 curve order / 2; rejects malleable signatures.
    uint256 private constant HALF_ORDER = 0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0;

    address public immutable attestor;

    event VaultCreated(bytes32 indexed channelKey, address indexed vault);
    event Tipped(bytes32 indexed channelKey, address indexed donor, address vault, uint256 amount);

    constructor(address trustedAttestor) {
        if (trustedAttestor == address(0)) revert InvalidAttestor();
        attestor = trustedAttestor;
    }

    function predictVault(bytes32 key) public view returns (address) {
        bytes32 initHash = keccak256(abi.encodePacked(type(BroadcastVault).creationCode, abi.encode(key)));
        return address(uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), address(this), key, initHash)))));
    }

    function deployAndTip(bytes32 key) external payable returns (address vault) {
        vault = _deploy(key);
        (bool success,) = payable(vault).call{value: msg.value}("");
        if (!success) revert TransferFailed();
        emit Tipped(key, msg.sender, vault, msg.value);
    }

    /// @dev Replay is prevented by the vault's irreversible owner assignment.
    /// The chain, factory, channel, wallet, and deadline are all signed.
    function claim(bytes32 key, address claimant, uint256 deadline, bytes calldata signature)
        external
        returns (address vault)
    {
        if (claimant == address(0) || msg.sender != claimant) revert WrongClaimant();
        if (block.timestamp >= deadline) revert ExpiredAttestation();
        bytes32 payload = keccak256(abi.encode(block.chainid, address(this), key, claimant, deadline));
        bytes32 digest = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", payload));
        if (_recover(digest, signature) != attestor) revert InvalidSignature();
        vault = _deploy(key);
        BroadcastVault(payable(vault)).claim(claimant);
    }

    function _deploy(bytes32 key) private returns (address vault) {
        vault = predictVault(key);
        if (vault.code.length == 0) {
            vault = address(new BroadcastVault{salt: key}(key));
            emit VaultCreated(key, vault);
        }
    }

    function _recover(bytes32 digest, bytes calldata signature) private pure returns (address) {
        if (signature.length != 65) revert InvalidSignature();
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := calldataload(signature.offset)
            s := calldataload(add(signature.offset, 32))
            v := byte(0, calldataload(add(signature.offset, 64)))
        }
        if (uint256(s) > HALF_ORDER || (v != 27 && v != 28)) revert InvalidSignature();
        address recovered = ecrecover(digest, v, r, s);
        if (recovered == address(0)) revert InvalidSignature();
        return recovered;
    }
}
