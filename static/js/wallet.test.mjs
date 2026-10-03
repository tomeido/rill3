import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const require = createRequire(import.meta.url);
const { parseNativeAmount, formatWei, chainHex, validateTransaction } = require('./wallet.js');
const source = await readFile(new URL('./wallet.js', import.meta.url), 'utf8');
const template = await readFile(new URL('../../templates/wallet.html', import.meta.url), 'utf8');
const datasets = new Map(Array.from(template.matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/g), ([tag, id]) => [
  id, Object.fromEntries(Array.from(tag.matchAll(/\bdata-([a-z-]+)="([^"]*)"/g), ([, key, value]) => [key.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase()), value])),
]));
const wallet = `0x${'11'.repeat(20)}`;
const other = `0x${'22'.repeat(20)}`;
const factory = `0x${'33'.repeat(20)}`;
const receiver = `0x${'44'.repeat(20)}`;
const hash = `0x${'55'.repeat(32)}`;
const channelKey = `0x${'77'.repeat(32)}`;
const config = {
  enabled: true, provider: 'twitch', display_name: '테스트 방송', channel_id: 'test-channel',
  chain_id: 31337, chain_name: 'Local Test', native_symbol: 'ETH', factory_address: factory,
  receive_address: receiver, balance_wei: '1000000000000000001', owner_wallet: null,
  session_verified: false, oauth_url: '/auth/twitch',
  provider_channel_id: '12345678', channel_url: null,
  channel_key: channelKey,
};
const payload = (action, amount = '0') => ({
  to: action === 'withdraw' ? receiver : factory, from: wallet, chain_id: 31337,
  data: action === 'withdraw' ? '0x3ccfd60b' : action === 'tip' ? `0xfb39ef8f${channelKey.slice(2)}` : `0x54313918${channelKey.slice(2)}${wallet.slice(2).padStart(64, '0')}${'0'.repeat(384)}`, value: amount,
});

test('native amounts retain all 18 decimals without floating point', () => {
  assert.equal(parseNativeAmount('0.000000000000000001'), 1n);
  assert.equal(parseNativeAmount('1.000000000000000001'), 1000000000000000001n);
  assert.equal(parseNativeAmount(' 9007199254740993.5 '), 9007199254740993500000000000000000n);
  assert.equal(formatWei('1000000000000000001'), '1.000000000000000001');
  assert.equal(formatWei('1000000000000000000'), '1');
  assert.equal(formatWei('1'), '0.000000000000000001');
  assert.equal(formatWei('0'), '0');
});

test('rejects ambiguous, zero, oversized, or overprecision amounts', () => {
  for (const amount of ['', '0', '0.0', '.1', '1.', '01', '-1', '+1', '1e3', '1,000', 'Infinity', 'NaN', '1 1', '0.0000000000000000001', '9'.repeat(80)]) {
    assert.throws(() => parseNativeAmount(amount), undefined, amount);
  }
});

test('chain IDs reject unsafe numbers and non-integer values', () => {
  assert.equal(chainHex(31337), '0x7a69');
  assert.equal(chainHex('0x7A69'), '0x7a69');
  for (const value of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, '', '1e2', undefined]) assert.throws(() => chainHex(value));
});

test('transaction validates exact recipient, account, chain, amount, and calldata', () => {
  const result = validateTransaction(payload('tip', '1'), wallet, config, 'tip', 1n);
  assert.equal(result.value, '0x1');
  assert.equal(result.to, factory);
  assert.equal(result.chainId, '0x7a69');
  for (const override of [{ to: other }, { to: receiver }, { from: other }, { chain_id: 1 }, { value: '2' }, { data: '0x123' }, { data: null }]) {
    assert.throws(() => validateTransaction({ ...payload('tip', '1'), ...override }, wallet, config, 'tip', 1n));
  }
  assert.equal(validateTransaction(payload('withdraw'), wallet, config, 'withdraw').to, receiver);
  assert.throws(() => validateTransaction(payload('claim'), wallet, config, 'withdraw'));
  assert.throws(() => validateTransaction(payload('claim', '1'), wallet, config, 'claim'));
});

test('calldata binds each operation to its selector, channel, and owner', () => {
  assert.equal(validateTransaction(payload('claim'), wallet, config, 'claim').data, payload('claim').data);
  assert.throws(() => validateTransaction({ ...payload('tip', '1'), data: `0xfb39ef8f${'88'.repeat(32)}` }, wallet, config, 'tip', 1n));
  assert.throws(() => validateTransaction({ ...payload('tip', '1'), data: payload('tip').data.replace('fb39ef8f', '54313918') }, wallet, config, 'tip', 1n));
  assert.throws(() => validateTransaction({ ...payload('claim'), data: payload('claim').data.replace(wallet.slice(2), other.slice(2)) }, wallet, config, 'claim'));
  assert.throws(() => validateTransaction({ ...payload('claim'), data: payload('claim').data + '00' }, wallet, config, 'claim'));
  assert.throws(() => validateTransaction({ ...payload('withdraw'), data: '0x3ccfd60b00' }, wallet, config, 'withdraw'));
});

async function harness(options = {}) {
  const elements = new Map();
  const requests = [];
  const rpc = [];
  const state = { account: wallet, chain: '0x7a69' };
  const events = {};
  function element(id) {
    if (!elements.has(id)) elements.set(id, {
      textContent: '', value: '', disabled: false, hidden: false, dataset: { ...datasets.get(id) },
      addEventListener(name, callback) { this[name] = callback; },
    });
    return elements.get(id);
  }
  const ethereum = {
    on(name, callback) { events[name] = callback; },
    async request(request) {
      rpc.push(request);
      if (options.rpc) {
        const override = await options.rpc(request, state, events);
        if (override !== undefined) return override;
      }
      switch (request.method) {
        case 'eth_accounts': case 'eth_requestAccounts': return [state.account];
        case 'eth_chainId': return state.chain;
        case 'wallet_switchEthereumChain': state.chain = request.params[0].chainId; return null;
        case 'eth_sendTransaction': return hash;
        case 'eth_getTransactionReceipt': return { transactionHash: hash, blockNumber: '0x10', status: '0x1' };
        case 'eth_blockNumber': return '0x11';
        case 'personal_sign': return `0x${'66'.repeat(65)}`;
        default: throw new Error(`Unexpected RPC ${request.method}`);
      }
    },
  };
  runInNewContext(source, {
    document: { querySelector: () => ({ dataset: { web3: 'wallet', base: '/rill3', channelId: 'test-channel' } }), getElementById: element },
    window: { ethereum, location: { origin: 'https://rill3.test' } },
    navigator: { clipboard: { writeText: async () => {} } },
    URL, TextEncoder, AbortController,
    setTimeout: (fn, ms) => ms === 3000 ? setTimeout(fn, 0) : setTimeout(fn, ms), clearTimeout,
    fetch: async (url, request) => {
      requests.push({ url, ...request });
      const body = request.body ? JSON.parse(request.body) : null;
      if (options.fetch) await options.fetch(url, body, state, events);
      const result = url.endsWith('/web3') ? { ...config, ...options.config } :
        url.endsWith('/wallet-challenge') ? { message: 'RILL3 수령 지갑 인증\nnonce: unique' } :
        url.endsWith('/tip') ? payload('tip', body.amount_wei) :
        url.endsWith('/claim') ? payload('claim') : payload('withdraw');
      return { ok: true, json: async () => result };
    },
  });
  async function idle() {
    for (let count = 0; count < 100; count += 1) {
      await new Promise(resolve => setTimeout(resolve, 2));
      if (!element('connect-wallet').disabled) return;
    }
    throw new Error('wallet action did not settle');
  }
  await idle();
  return { element, rpc, requests, state, idle, events };
}

test('one wei tip sends exact amount and only reports completion after receipt confirmations', async () => {
  const app = await harness();
  app.element('tip-amount').value = '0.000000000000000001';
  app.element('tip-form').submit({ preventDefault() {} });
  await app.idle();
  const tip = app.requests.find(request => request.url.endsWith('/tip'));
  assert.equal(JSON.parse(tip.body).amount_wei, '1');
  assert.equal(tip.credentials, 'same-origin');
  assert.equal(tip.headers['Content-Type'], 'application/json');
  const send = app.rpc.find(request => request.method === 'eth_sendTransaction');
  assert.equal(send.params[0].value, '0x1');
  assert.equal(send.params[0].to, factory);
  assert.match(app.element('transaction-status').textContent, /2개 이상의 블록 확인/);
});

test('account change while preparing transaction prevents sending', async () => {
  const app = await harness({ fetch: async (url, _body, state) => { if (url.endsWith('/tip')) state.account = other; } });
  app.element('tip-amount').value = '1';
  app.element('tip-form').submit({ preventDefault() {} });
  await app.idle();
  assert.equal(app.rpc.some(request => request.method === 'eth_sendTransaction'), false);
  assert.match(app.element('wallet-status').textContent, /계정이 변경/);
});

test('account change during ownership signature prevents claim submission', async () => {
  const app = await harness({
    config: { session_verified: true },
    rpc: async (request, state) => { if (request.method === 'personal_sign') state.account = other; },
  });
  app.element('claim-wallet').click();
  await app.idle();
  const signature = app.rpc.find(request => request.method === 'personal_sign');
  assert.equal(Buffer.from(signature.params[0].slice(2), 'hex').toString(), 'RILL3 수령 지갑 인증\nnonce: unique');
  assert.equal(app.requests.some(request => request.url.endsWith('/claim')), false);
  assert.equal(app.rpc.some(request => request.method === 'eth_sendTransaction'), false);
});

test('submitted transaction stays pending and blocks duplicates without receipt', async () => {
  const app = await harness({ rpc: async request => request.method === 'eth_getTransactionReceipt' ? null : undefined });
  app.element('tip-amount').value = '1';
  app.element('tip-form').submit({ preventDefault() {} });
  await app.idle();
  assert.equal(app.element('tip-submit').disabled, true);
  assert.equal(app.element('check-transaction').disabled, false);
  assert.equal(app.element('transaction-hash').textContent, hash);
  assert.match(app.element('transaction-status').textContent, /확인 대기/);
  app.element('tip-form').submit({ preventDefault() {} });
  assert.equal(app.rpc.filter(request => request.method === 'eth_sendTransaction').length, 1);
});

test('reverted receipt is reported as failed', async () => {
  const app = await harness({ rpc: async request => request.method === 'eth_getTransactionReceipt' ? { blockNumber: '0x10', status: '0x0' } : undefined });
  app.element('tip-amount').value = '1';
  app.element('tip-form').submit({ preventDefault() {} });
  await app.idle();
  assert.match(app.element('transaction-status').textContent, /실패/);
  assert.equal(app.element('wallet-status').dataset.error, 'true');
});

test('withdrawal requires the registered wallet before preparing a transfer', async () => {
  const app = await harness({ config: { owner_wallet: other } });
  assert.equal(app.element('withdraw-funds').disabled, true);
  app.element('withdraw-funds').click();
  await app.idle();
  assert.equal(app.requests.some(request => request.url.endsWith('/withdraw')), false);
  assert.equal(app.rpc.some(request => request.method === 'eth_sendTransaction'), false);
});

test('canonical platform identity is visible and only known HTTPS platform links are allowed', async () => {
  assert.match(template, /등록자가 입력한 이름/);
  const app = await harness({ config: { provider: 'youtube', provider_channel_id: 'UC123', channel_url: 'https://www.youtube.com/channel/UC123' } });
  assert.equal(app.element('canonical-channel-id').textContent, 'UC123');
  assert.equal(app.element('canonical-channel-link').hidden, false);
  assert.equal(app.element('canonical-channel-link').href, 'https://www.youtube.com/channel/UC123');
  for (const channel_url of ['https://www.youtube.com.evil.test/channel/UC123', 'javascript:alert(1)', 'https://user@www.youtube.com/channel/UC123']) {
    const invalid = await harness({ config: { provider: 'youtube', channel_url } });
    assert.equal(invalid.element('canonical-channel-link').hidden, true);
  }
});

test('reserved wallet is shown and a different account cannot request a challenge', async () => {
  const app = await harness({ config: { session_verified: true, reserved_wallet: other } });
  assert.equal(app.element('claim-wallet').disabled, true);
  assert.match(app.element('claim-help').textContent, new RegExp(other));
  app.element('claim-wallet').click();
  await app.idle();
  assert.equal(app.requests.some(request => request.url.endsWith('/wallet-challenge')), false);
});

test('canceling the on-chain claim preserves the reserved wallet and permanent-lock explanation', async () => {
  assert.match(template, /수령 지갑은 변경할 수 없습니다/);
  assert.match(template, /등록 거래를 취소해도 같은 지갑/);
  const app = await harness({
    config: { session_verified: true },
    rpc: async request => { if (request.method === 'eth_sendTransaction') throw { code: 4001 }; },
  });
  app.element('claim-wallet').click();
  await app.idle();
  assert.match(app.element('claim-help').textContent, new RegExp(wallet));
  assert.match(app.element('ownership-label').textContent, /등록 거래 대기/);
  app.events.accountsChanged([other]);
  assert.equal(app.element('claim-wallet').disabled, true);
});

test('wallet script remains below the 20 KiB JavaScript budget', () => {
  assert.ok(Buffer.byteLength(source) < 20 * 1024);
});
