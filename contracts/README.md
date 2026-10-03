# Broadcaster donation vaults

`BroadcastVaultFactory` assigns a deterministic native-ETH receiving contract for
each canonical provider/channel key using standard EVM CREATE2. `deployAndTip`
deploys the vault if needed and forwards the donation atomically. A counterfactual
address can also hold ETH before deployment; `claim` deploys it when necessary.

After official provider-account authentication and a wallet signature challenge,
the server's attestor signs the first owner's claim. The contract requires the
claim transaction to come from that exact wallet, rejects expired/invalid proofs,
and sets the owner once. `withdraw()` pays the entire balance only to the owner.
The factory has no withdrawal, upgrade, or owner-reset method. Gas is paid by the
wallet submitting the transaction; platform fees are zero.

Only native ETH is supported. Do not send tokens or NFTs: this version has no token
recovery. An unclaimed vault may hold funds indefinitely if broadcaster ownership
cannot be verified. The attestor is trusted before the first claim; a compromised
attestor can misassign an unclaimed vault. Already claimed vaults cannot be
reassigned by the attestor. Owner-wallet key loss is not recoverable in this version.

## Signed claim encoding

```text
channelKey = keccak256("RILL3_CHANNEL_V1\0" || provider || "\0" || canonicalProviderId)
payload    = keccak256(abi.encode(chainId, factory, channelKey, owner, deadline))
digest     = keccak256("\x19Ethereum Signed Message:\n32" || payload)
signature  = r[32] || s[32] || v[1]  (low-s, v = 27 or 28)
```

Channel keys are per platform channel, not mutable display names or creator slugs.
Claim replay is prevented by the irreversible owner assignment. Chain ID and
factory binding prevent cross-chain and cross-deployment use.

## Build and test

Install [Foundry](https://getfoundry.sh/introduction/installation/), then run:

```sh
forge fmt --root contracts --check
forge test --root contracts
cargo test -p rill3-payments
```

The compiler is pinned to Solidity 0.8.28 with Paris EVM output. Tests cover
pre-verification deposits, counterfactual funding, unauthorized withdrawal,
one-time claiming, signed field binding, expiration, arbitrary donation sizes,
reentrant withdrawals, and a stateful conservation-of-funds invariant.

## Deployment and server configuration

Use an explicitly selected standard EVM development/test network first. There is
no automatic default or production deployment. A possible test network is Base
Sepolia (chain ID 84532, ETH), subject to confirming the operator's RPC. Abstract's
native zkVM deployment/address derivation is not treated as interchangeable with
standard EVM CREATE2; Abstract compatibility has not been validated here.

Deploy the factory with the intended attestor's public address. The attestor private
key is a server identity-signing key, separate from the wallet paying deployment
gas. Use a hardware or encrypted Foundry deployment account rather than putting
a production private key on the command line:

```sh
forge create --root contracts src/BroadcastVaultFactory.sol:BroadcastVaultFactory \
  --rpc-url "$RILL3_RPC_URL" --account deployer --broadcast \
  --constructor-args "$RILL3_ATTESTOR_ADDRESS"
```

Independently verify the deployed source, compiler settings, constructor attestor,
and runtime bytecode. Pin the hash of the reviewed deployed runtime, not creation
bytecode. The immutable attestor is embedded in runtime code, so the hash changes
when the attestor changes. `cast code <factory> --rpc-url <rpc>` returns runtime
bytecode; `cast keccak <runtime-bytecode>` computes the required hash.

Configure the server with all of:

```text
RILL3_CHAIN_ID=84532
RILL3_CHAIN_NAME=Base Sepolia
RILL3_RPC_URL=https://your-trusted-rpc.example
RILL3_VAULT_FACTORY=0x...reviewed deployed factory...
RILL3_FACTORY_CODE_HASH=0x...keccak256 of reviewed runtime...
RILL3_ATTESTOR_PRIVATE_KEY=...separate protected identity-attestor key...
```

The server checks the chain, exact runtime hash, and attestor before using a vault.
Preserve the per-channel deployment binding: changing a factory changes its
addresses and does not migrate or recover funds from old vaults. Local tests are
not an external smart-contract security audit.

## Rust-to-Solidity interoperability test

Start a fresh local-only `anvil --host 127.0.0.1 --port 18545`. Deploy the factory
using its unlocked local account and the **public test-only** attestor address
`0x7e5f4552091a69125d5dfcb7b8c2659029395bdf` (private scalar 1). Never use this
attestor on a public network.

```sh
forge create --root contracts src/BroadcastVaultFactory.sol:BroadcastVaultFactory \
  --rpc-url http://127.0.0.1:18545 --unlocked \
  --from 0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266 --broadcast \
  --constructor-args 0x7e5f4552091a69125d5dfcb7b8c2659029395bdf
```

Set `RILL3_TEST_EVM_RPC=http://127.0.0.1:18545`, `RILL3_TEST_EVM_FACTORY` to the
deployed address, and `RILL3_TEST_EVM_CODE_HASH` to its runtime hash. Then run:

```sh
cargo test -p rill3-payments --test local_evm -- --ignored
```

This opt-in test refuses non-loopback URLs and any chain other than 31337. It
funds a channel, signs a claim in Rust, submits generated ABI calldata through
Anvil's unlocked test wallet, checks the on-chain owner, and withdraws the balance.

Protocol references: [CREATE2](https://eips.ethereum.org/EIPS/eip-1014),
[EIP-191](https://eips.ethereum.org/EIPS/eip-191), and
[wallet challenge domain/nonce guidance](https://eips.ethereum.org/EIPS/eip-4361).
