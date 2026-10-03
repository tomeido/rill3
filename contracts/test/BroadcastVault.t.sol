// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {BroadcastVault} from "../src/BroadcastVault.sol";
import {BroadcastVaultFactory} from "../src/BroadcastVaultFactory.sol";

interface Vm {
    function addr(uint256 key) external returns (address);
    function sign(uint256 key, bytes32 digest) external returns (uint8, bytes32, bytes32);
    function deal(address account, uint256 balance) external;
    function prank(address account) external;
    function expectRevert(bytes4 selector) external;
    function chainId(uint256 id) external;
    function warp(uint256 timestamp) external;
}

contract BroadcastVaultTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    uint256 private constant SIGNER = 0xA11CE;
    address private owner;
    BroadcastVaultFactory private factory;
    bytes32 private constant KEY = keccak256("twitch:123");

    function setUp() public {
        factory = new BroadcastVaultFactory(vm.addr(SIGNER));
        owner = vm.addr(0xB0B);
        vm.deal(address(this), 100 ether);
    }

    function signature(bytes32 key, address claimant, uint256 deadline) private returns (bytes memory) {
        bytes32 payload = keccak256(abi.encode(block.chainid, address(factory), key, claimant, deadline));
        bytes32 digest = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", payload));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SIGNER, digest);
        return abi.encodePacked(r, s, v);
    }

    function claimOwner() private returns (BroadcastVault vault) {
        uint256 deadline = block.timestamp + 300;
        bytes memory proof = signature(KEY, owner, deadline);
        vm.prank(owner);
        vault = BroadcastVault(payable(factory.claim(KEY, owner, deadline, proof)));
    }

    function testDonationBeforeClaimAndWithdrawal() public {
        address predicted = factory.predictVault(KEY);
        address deployed = factory.deployAndTip{value: 2 ether}(KEY);
        require(predicted == deployed, "determinism");
        require(deployed.balance == 2 ether, "deposit");
        vm.expectRevert(BroadcastVault.Unauthorized.selector);
        BroadcastVault(payable(deployed)).withdraw();
        BroadcastVault vault = claimOwner();
        vm.prank(owner);
        vault.withdraw();
        require(owner.balance == 2 ether && deployed.balance == 0, "withdrawal");
        require(address(factory).balance == 0, "no factory balance");
    }

    function testPrefundedCounterfactualAddress() public {
        address predicted = factory.predictVault(KEY);
        (bool sent,) = payable(predicted).call{value: 1 ether}("");
        require(sent, "prefund");
        BroadcastVault vault = claimOwner();
        vm.prank(owner);
        vault.withdraw();
        require(owner.balance == 1 ether, "counterfactual funds recovered");
    }

    function testCannotClaimTwiceOrChangeOwner() public {
        BroadcastVault vault = claimOwner();
        uint256 deadline = block.timestamp + 300;
        bytes memory proof = signature(KEY, owner, deadline);
        vm.prank(owner);
        vm.expectRevert(BroadcastVault.AlreadyClaimed.selector);
        factory.claim(KEY, owner, deadline, proof);
        vm.expectRevert(BroadcastVault.Unauthorized.selector);
        vault.claim(address(this));
    }

    function testSignatureCannotChangeChannelWalletOrChain() public {
        uint256 deadline = block.timestamp + 300;
        bytes memory proof = signature(KEY, owner, deadline);
        vm.prank(owner);
        vm.expectRevert(BroadcastVaultFactory.InvalidSignature.selector);
        factory.claim(bytes32(uint256(KEY) + 1), owner, deadline, proof);
        vm.expectRevert(BroadcastVaultFactory.InvalidSignature.selector);
        factory.claim(KEY, address(this), deadline, proof);
        vm.chainId(block.chainid + 1);
        vm.prank(owner);
        vm.expectRevert(BroadcastVaultFactory.InvalidSignature.selector);
        factory.claim(KEY, owner, deadline, proof);
    }

    function testExpiredAndWrongCallerRejected() public {
        uint256 deadline = block.timestamp + 300;
        bytes memory proof = signature(KEY, owner, deadline);
        vm.expectRevert(BroadcastVaultFactory.WrongClaimant.selector);
        factory.claim(KEY, owner, deadline, proof);
        vm.warp(deadline);
        vm.prank(owner);
        vm.expectRevert(BroadcastVaultFactory.ExpiredAttestation.selector);
        factory.claim(KEY, owner, deadline, proof);
    }

    function testProofCannotBeReplayedAtAnotherFactory() public {
        uint256 deadline = block.timestamp + 300;
        bytes memory proof = signature(KEY, owner, deadline);
        BroadcastVaultFactory other = new BroadcastVaultFactory(vm.addr(SIGNER));
        vm.prank(owner);
        vm.expectRevert(BroadcastVaultFactory.InvalidSignature.selector);
        other.claim(KEY, owner, deadline, proof);
    }

    function testMalformedSignaturesRejected() public {
        uint256 deadline = block.timestamp + 300;
        vm.prank(owner);
        vm.expectRevert(BroadcastVaultFactory.InvalidSignature.selector);
        factory.claim(KEY, owner, deadline, hex"00");
        bytes memory proof = signature(KEY, owner, deadline);
        proof[64] = bytes1(uint8(29));
        vm.prank(owner);
        vm.expectRevert(BroadcastVaultFactory.InvalidSignature.selector);
        factory.claim(KEY, owner, deadline, proof);
    }

    function testReentrantOwnerCannotWithdrawTwice() public {
        ReentrantOwner recipient = new ReentrantOwner();
        uint256 deadline = block.timestamp + 300;
        bytes memory proof = signature(KEY, address(recipient), deadline);
        recipient.claim(factory, KEY, deadline, proof);
        address vault = factory.deployAndTip{value: 1 ether}(KEY);
        recipient.withdraw();
        require(address(recipient).balance == 1 ether && vault.balance == 0, "single transfer");
        require(recipient.reentryBlocked(), "reentry rejected");
    }

    function testFuzzOnlyOwnerReceivesAllDonations(uint96 amount) public {
        vm.deal(address(this), uint256(amount));
        BroadcastVault vault = claimOwner();
        factory.deployAndTip{value: amount}(KEY);
        vm.expectRevert(BroadcastVault.Unauthorized.selector);
        vault.withdraw();
        vm.prank(owner);
        vault.withdraw();
        require(owner.balance == uint256(amount), "owner gets all");
        require(address(vault).balance == 0 && address(factory).balance == 0, "zero remainder");
    }
}

contract ReentrantOwner {
    BroadcastVault private vault;
    bool public reentryBlocked;

    function claim(BroadcastVaultFactory factory, bytes32 key, uint256 deadline, bytes memory proof) external {
        vault = BroadcastVault(payable(factory.claim(key, address(this), deadline, proof)));
    }

    function withdraw() external {
        vault.withdraw();
    }

    receive() external payable {
        (bool success,) = address(vault).call(abi.encodeCall(vault.withdraw, ()));
        reentryBlocked = !success;
    }
}

/// Stateful accounting invariant across arbitrary donation/withdrawal sequences.
contract BroadcastVaultInvariantTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    VaultHandler private handler;

    function setUp() public {
        handler = new VaultHandler();
    }

    function targetContracts() public view returns (address[] memory targets) {
        targets = new address[](1);
        targets[0] = address(handler);
    }

    function invariantAllValueRemainsWithVaultOrOwner() public view {
        require(handler.deposited() == handler.withdrawn() + address(handler.vault()).balance, "accounting");
        require(address(handler).balance == handler.withdrawn(), "owner holds withdrawn funds");
        require(address(handler.factory()).balance == 0, "factory custody");
        require(handler.vault().owner() == address(handler), "owner invariant");
    }
}

contract VaultHandler {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    BroadcastVaultFactory public factory;
    BroadcastVault public vault;
    bytes32 private constant KEY = keccak256("invariant-channel");
    uint256 public deposited;
    uint256 public withdrawn;

    constructor() {
        uint256 signer = 0xA11CE;
        factory = new BroadcastVaultFactory(vm.addr(signer));
        uint256 deadline = block.timestamp + 1000000;
        bytes32 payload = keccak256(abi.encode(block.chainid, address(factory), KEY, address(this), deadline));
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(signer, keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", payload)));
        vault = BroadcastVault(payable(factory.claim(KEY, address(this), deadline, abi.encodePacked(r, s, v))));
    }

    function deposit(uint96 amount) external {
        vm.deal(address(this), address(this).balance + uint256(amount));
        deposited += amount;
        factory.deployAndTip{value: amount}(KEY);
    }

    function withdraw() external {
        withdrawn += address(vault).balance;
        vault.withdraw();
    }

    receive() external payable {}
}
