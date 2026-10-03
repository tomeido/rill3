# rill3-payments

Channel donation vault and wallet-signature boundary for RILL3.

Each canonical provider/channel pair has a domain-separated 32-byte key. A deployed
`BroadcastVaultFactory` derives a deterministic receiving contract address for that
key. Native ETH can arrive before the broadcaster claims the address. After
provider-account verification and a separate wallet challenge, the server attests
the first owner. The owner submits the claim and withdrawals from their own wallet.
The contract transfers the whole native balance only to that owner. There is no
platform withdrawal method, fee, owner replacement, or upgrade.

`VaultClient` reads chain state and constructs transaction payloads; it cannot send
transactions. Before showing a usable address, it checks the configured chain ID,
factory runtime bytecode hash, and immutable attestor. Amounts stay exact 256-bit
integers, with decimal strings for display and hex quantities for wallet requests.
Only native ETH is supported; ERC-20 tokens and NFTs sent to a vault cannot be
recovered by this version.

`verify_wallet_signature` verifies EIP-191 EOA signatures over an exact challenge.
The HTTP/auth layer is responsible for origin, purpose, channel, chain, expiry,
single-use nonce consumption, and authenticated platform-account ownership. Smart
contract wallets requiring ERC-1271 signature verification are not supported yet.
The existing discovery `verification_state` is not proof of broadcaster ownership.

`AttestationSigner` is an identity oracle: compromise before a channel's first
claim can assign the wrong owner. It cannot reassign or drain an already claimed
vault. Keep its key outside source control and separate from operational wallets.
Donations to an unclaimed vault are locked until an owner completes verification;
this is a new escrow-style flow, distinct from the original immediate-routing plan.

See [contract setup and tests](../../contracts/README.md). No contract deployment,
production chain credentials, OAuth credentials, indexing, or finality ledger are
automatically provided by this crate. Messages and media never go on-chain.
