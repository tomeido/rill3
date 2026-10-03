# Browser JavaScript

Discovery and creator pages remain server-rendered without wallet JavaScript.
`wallet.js` loads only on registration and support pages, uses the injected
EIP-1193 wallet, and has no CDN or third-party runtime dependency. Transactions
are submitted by the user's wallet and receipts are checked before completion.

Run browser logic tests with `node --test static/js/wallet.test.mjs`.
