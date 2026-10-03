(() => {
'use strict';
const UNIT = 10n ** 18n;
const MAX_UINT256 = (1n << 256n) - 1n;
const ADDRESS = /^0x[0-9a-fA-F]{40}$/;
const ZERO_ADDRESS = /^0x0{40}$/i;
const HASH = /^0x[0-9a-fA-F]{64}$/;

function parseNativeAmount(input) {
  const value = String(input).trim();
  if (!/^(0|[1-9]\d*)(\.\d{1,18})?$/.test(value)) {
    throw new Error('0.01처럼 입력하세요. 소수점 최대 18자리, 지수·쉼표·음수는 불가합니다.');
  }
  const [whole, fraction = ''] = value.split('.');
  const wei = BigInt(whole) * UNIT + BigInt(fraction.padEnd(18, '0'));
  if (wei <= 0n || wei > MAX_UINT256) throw new Error('후원 가능한 범위의 0보다 큰 금액을 입력해 주세요.');
  return wei;
}

function integer(value) {
  if ((typeof value === 'number' && (!Number.isSafeInteger(value) || value < 0)) || !/^(0x[0-9a-fA-F]+|\d+)$/.test(String(value))) throw new Error('올바르지 않은 네트워크 응답입니다.');
  return BigInt(value);
}

function formatWei(value) {
  const wei = integer(value);
  const fraction = (wei % UNIT).toString().padStart(18, '0').replace(/0+$/, '');
  return (wei / UNIT).toString() + (fraction ? `.${fraction}` : '');
}

function chainHex(value) {
  const chain = integer(value);
  if (chain === 0n) throw new Error('올바르지 않은 네트워크 ID입니다.');
  return `0x${chain.toString(16)}`;
}

function isAddress(value) { return typeof value === 'string' && ADDRESS.test(value) && !ZERO_ADDRESS.test(value); }
function sameAddress(a, b) { return isAddress(a) && isAddress(b) && a.toLowerCase() === b.toLowerCase(); }

function validateTransaction(payload, wallet, config, action, expectedValue = 0n) {
  const expectedTarget = action === 'withdraw' ? config.receive_address : config.factory_address;
  const data = String(payload.data).toLowerCase();
  const key = HASH.test(config.channel_key) ? config.channel_key.slice(2).toLowerCase() : '';
  const callMatches = action === 'withdraw' ? data === '0x3ccfd60b' : key && (action === 'tip' ? data === `0xfb39ef8f${key}` : action === 'claim' && data.length === 522 && data.startsWith(`0x54313918${key}${String(wallet).slice(2).toLowerCase().padStart(64, '0')}`));
  if (!callMatches || !isAddress(wallet) || !sameAddress(payload.to, expectedTarget) ||
      (payload.from !== undefined && !sameAddress(payload.from, wallet)) ||
      chainHex(payload.chain_id) !== chainHex(config.chain_id) ||
      !/^0x(?:[0-9a-fA-F]{2})*$/.test(payload.data) || integer(payload.value) !== expectedValue) {
    throw new Error('거래 정보가 요청과 다릅니다. 지갑·네트워크·금액·수신 주소를 확인해 주세요.');
  }
  return { from: wallet, to: payload.to, data: payload.data, value: `0x${expectedValue.toString(16)}`, chainId: chainHex(config.chain_id) };
}

if (typeof module !== 'undefined' && module.exports) module.exports = { parseNativeAmount, formatWei, chainHex, validateTransaction };
if (typeof document === 'undefined') return;
const page = document.querySelector('[data-web3]');
if (!page) return;
const base = page.dataset.base || '';
const element = id => document.getElementById(id);
const setText = (id, value) => { element(id).textContent = value; };
const preset = (id, key) => setText(id, element(id).dataset[key]);

function feedback(id, message, error = false) {
  const node = element(id);
  node.textContent = message;
  node.dataset.error = String(error);
}

function errorMessage(error) {
  const code = Number(error && error.code);
  if (code === 4001) return '지갑 요청을 취소했습니다.';
  if (code === 4902) return '지갑에 표시된 체인 ID의 네트워크를 추가한 뒤 다시 시도해 주세요.';
  if (code === -32002) return '지갑에서 기다리는 요청을 먼저 확인해 주세요.';
  if (code === 4900 || code === 4901) return '지갑 네트워크 연결을 확인해 주세요.';
  if (error && error.name === 'AbortError') return '서버 응답이 지연되고 있습니다. 잠시 후 다시 시도해 주세요.';
  return error && error.message ? error.message : '요청 실패: 잠시 후 다시 시도해 주세요.';
}

function localUrl(value) {
  if (typeof value !== 'string' || !value) return null;
  try {
    const url = new URL(value, window.location.origin);
    return url.origin === window.location.origin && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}

async function api(path, body) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 20000);
  try {
    const response = await fetch(`${base}${path}`, {
      method: body === undefined ? 'GET' : 'POST', credentials: 'same-origin', cache: 'no-store',
      headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body), signal: controller.signal,
    });
    const result = await response.json().catch(() => null);
    if (!response.ok) {
      const detail = result && (typeof result.error === 'string' ? result.error : result.message);
      throw new Error(detail || `요청을 완료하지 못했습니다 (${response.status}).`);
    }
    if (!result || typeof result !== 'object') throw new Error('서버 응답을 확인할 수 없습니다.');
    return result;
  } finally { clearTimeout(timer); }
}

if (page.dataset.web3 === 'register') {
  const providerHelp = {
    twitch: 'Twitch 숫자 사용자 ID (예: 12345678)를 입력하세요. 이름·URL은 지원하지 않습니다.',
    youtube: 'UC로 시작하는 YouTube 채널 ID를 입력하세요. @핸들·동영상 ID·URL은 지원하지 않습니다.',
    chzzk: '치지직 채널 URL 마지막의 영문·숫자 32자리 ID만 입력하세요.',
  };
  const updateHelp = () => { setText('channel-id-help', providerHelp[element('provider').value]); };
  element('provider').addEventListener('change', updateHelp);
  updateHelp();
  let submitting = false;
  element('registration-form').addEventListener('submit', async event => {
    event.preventDefault();
    if (submitting) return;
    submitting = true;
    element('register-submit').disabled = true;
    feedback('register-status', '채널과 후원 주소를 등록하고 있습니다.');
    try {
      const result = await api('/api/registrations', {
        provider: element('provider').value,
        provider_channel_id: element('provider-channel-id').value.trim(),
        display_name: element('display-name').value.trim(),
      });
      const destination = localUrl(result.url);
      if (!destination) throw new Error('후원 페이지 주소를 확인하지 못했습니다. 등록 채널에서 확인해 주세요.');
      feedback('register-status', '등록되었습니다. 후원 페이지로 이동합니다.');
      window.location.assign(destination);
    } catch (error) { feedback('register-status', errorMessage(error), true); }
    finally { submitting = false; element('register-submit').disabled = false; }
  });
  return;
}

const channelPath = `/api/channels/${encodeURIComponent(page.dataset.channelId)}`;
const ethereum = window.ethereum;
let config = null;
let account = null;
let busy = false;
let transaction = null;
let accountRevision = 0;

function ready() { return config && config.enabled === true && isAddress(config.receive_address) && isAddress(config.factory_address); }
function pending() { return transaction && !transaction.settled; }
function renderActions() {
  const available = ready() && !busy && !pending();
  element('connect-wallet').disabled = !ready() || busy || !ethereum;
  element('tip-submit').disabled = !available || !ethereum;
  element('tip-amount').disabled = !ready() || busy || !!pending();
  element('claim-wallet').disabled = !available || !ethereum || !config.session_verified || isAddress(config.owner_wallet) || (isAddress(config.reserved_wallet) && account && !sameAddress(account, config.reserved_wallet));
  element('withdraw-funds').disabled = !available || !ethereum || !sameAddress(account, config.owner_wallet) || integer(config.balance_wei) === 0n;
  element('copy-address').disabled = !ready();
  element('check-transaction').disabled = busy || !pending();
  element('logout-channel').disabled = busy;
  setText('connected-wallet', account || '연결된 지갑 없음');
  setText('connect-wallet', account ? '지갑 연결 확인' : '지갑 연결');
}

function renderConfig() {
  setText('channel-name', config.display_name || '채널 후원');
  setText('canonical-channel-id', config.provider_channel_id || '확인 불가');
  const link = element('canonical-channel-link');
  link.hidden = true;
  try {
    const url = new URL(config.channel_url);
    const host = { youtube: 'www.youtube.com', chzzk: 'chzzk.naver.com', twitch: 'www.twitch.tv' }[config.provider];
    if (url.protocol === 'https:' && url.hostname === host && !url.username && !url.password && !url.port) { link.href = url.href; link.hidden = false; }
  } catch { /* Missing or invalid public channel URL stays hidden. */ }
  setText('channel-provider', { twitch: 'TWITCH', youtube: 'YOUTUBE', chzzk: '치지직' }[config.provider] || config.provider || 'CHANNEL SUPPORT');
  const enabled = ready();
  const symbol = config.native_symbol || '';
  setText('network-name', enabled ? `${config.chain_name} · 체인 ID ${config.chain_id}` : '후원 설정 대기');
  setText('receive-address', enabled ? config.receive_address : '네트워크 설정 후 주소를 확인할 수 있습니다.');
  setText('channel-balance', enabled ? `${formatWei(config.balance_wei)} ${symbol}` : '—');
  setText('tip-symbol', enabled ? `(${symbol})` : '');
  setText('network-help', enabled ? `${config.chain_name} 네트워크의 ${symbol}만 지원합니다. 다른 네트워크나 ERC-20 토큰을 보내지 마세요.` : '네트워크와 후원 계약이 설정되면 주소가 활성화됩니다.');
  const claimed = isAddress(config.owner_wallet);
  setText('ownership-label', claimed ? '수령 지갑 연결됨' : isAddress(config.reserved_wallet) ? '등록 거래 대기' : '방송인 인증 전');
  preset('balance-help', claimed ? 'claimed' : 'pending');
  const oauthUrl = localUrl(config.oauth_url);
  element('verify-channel').hidden = !enabled || !!config.session_verified || claimed || !oauthUrl;
  if (oauthUrl) element('verify-channel').href = oauthUrl;
  element('logout-channel').hidden = !config.session_verified;
  preset('verification-status', config.session_verified ? 'verified' : claimed ? 'claimed' : oauthUrl ? 'available' : 'unavailable');
  setText('claim-help', claimed ? `등록된 수령 지갑: ${config.owner_wallet}` : isAddress(config.reserved_wallet) ? `예약된 수령 지갑: ${config.reserved_wallet}. 이 지갑으로 등록을 완료하세요.` : element('claim-help').dataset.unclaimed);
  if (!ethereum) preset('connected-help', 'unavailable');
  renderActions();
}

async function refresh() {
  config = await api(`${channelPath}/web3`);
  renderConfig();
  if (!ready()) feedback('wallet-status', config.reason || '현재 후원이 설정되지 않았습니다. 운영자에게 문의해 주세요.', true);
  return config;
}

function walletProvider() {
  if (!ethereum || typeof ethereum.request !== 'function') throw new Error('이더리움 호환 지갑이 필요합니다.');
  return ethereum;
}

function setAccount(accounts) {
  const next = Array.isArray(accounts) && isAddress(accounts[0]) ? accounts[0] : null;
  if (!sameAddress(next, account) && (next || account)) accountRevision += 1;
  account = next;
  renderActions();
}

async function connect() {
  const accounts = await walletProvider().request({ method: 'eth_requestAccounts' });
  setAccount(accounts);
  if (!account) throw new Error('연결할 지갑 주소를 선택해 주세요.');
  return account;
}

async function ensureChain() {
  const target = chainHex(config.chain_id);
  if (chainHex(await walletProvider().request({ method: 'eth_chainId' })) !== target) {
    feedback('wallet-status', `${config.chain_name} 네트워크 전환을 지갑에서 승인해 주세요.`);
    await ethereum.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: target }] });
  }
  if (chainHex(await ethereum.request({ method: 'eth_chainId' })) !== target) throw new Error('설정된 후원 네트워크로 전환되지 않았습니다.');
}

async function assertAccount(expected, revision) {
  const accounts = await walletProvider().request({ method: 'eth_accounts' });
  setAccount(accounts);
  if (!sameAddress(account, expected) || revision !== accountRevision) throw new Error('지갑 계정이 변경되었습니다. 다시 시작해 주세요.');
}

async function prepare() {
  await refresh();
  if (!ready()) throw new Error(config.reason || '후원 설정이 완료되지 않았습니다.');
  const wallet = await connect();
  const revision = accountRevision;
  await ensureChain();
  await assertAccount(wallet, revision);
  return { wallet, revision };
}

async function run(action) {
  if (busy) return;
  busy = true;
  renderActions();
  feedback('wallet-status', '요청을 확인하고 있습니다.');
  try { await action(); }
  catch (error) { feedback('wallet-status', errorMessage(error), true); }
  finally { busy = false; renderActions(); }
}

async function waitForReceipt() {
  const tx = transaction;
  for (let attempt = 0; attempt < 30; attempt += 1) {
    if (chainHex(await walletProvider().request({ method: 'eth_chainId' })) !== tx.chain) {
      throw new Error('거래는 제출되었습니다. 해당 네트워크로 돌아온 뒤 거래 확인을 다시 눌러 주세요.');
    }
    const receipt = await ethereum.request({ method: 'eth_getTransactionReceipt', params: [tx.hash] });
    if (receipt && receipt.blockNumber && receipt.status !== undefined) {
      if (integer(receipt.status) === 0n) {
        tx.settled = true;
        preset('transaction-status', 'failed');
        feedback('wallet-status', '거래가 실패했습니다. 지갑에서 실패 사유를 확인해 주세요.', true);
        await refresh();
        return;
      }
      if (integer(receipt.status) !== 1n) throw new Error('거래 영수증의 상태를 확인할 수 없습니다.');
      const block = integer(await ethereum.request({ method: 'eth_blockNumber' }));
      const confirmations = block - integer(receipt.blockNumber) + 1n;
      if (confirmations >= 2n) {
        tx.settled = true;
        preset('transaction-status', 'confirmed');
        feedback('wallet-status', `${tx.label} 거래가 확인되었습니다.`);
        await refresh();
        return;
      }
      preset('transaction-status', 'mined');
    }
    await new Promise(resolve => setTimeout(resolve, 3000));
  }
  preset('transaction-status', 'pending');
  feedback('wallet-status', '거래가 제출되었으며 블록 확인을 기다리고 있습니다.');
}

async function send(payload, context, action, value = 0n) {
  const tx = validateTransaction(payload, context.wallet, config, action, value);
  await ensureChain();
  await assertAccount(context.wallet, context.revision);
  feedback('wallet-status', '지갑에서 거래 내용을 확인하고 승인해 주세요.');
  const hash = await ethereum.request({ method: 'eth_sendTransaction', params: [tx] });
  if (!HASH.test(hash)) throw new Error('해시 확인 실패: 다시 보내기 전에 지갑의 거래 내역을 확인해 주세요.');
  const label = { tip: '후원', claim: '수령 지갑 연결', withdraw: '출금' }[action];
  transaction = { hash, chain: tx.chainId, settled: false, label };
  element('transaction').hidden = false;
  setText('transaction-hash', hash);
  preset('transaction-status', 'submitted');
  feedback('wallet-status', '거래가 제출되었습니다. 거래 해시가 아래에 표시됩니다.');
  try { await waitForReceipt(); }
  catch (error) {
    preset('transaction-status', 'unavailable');
    throw error;
  }
}

element('connect-wallet').addEventListener('click', () => run(async () => {
  await prepare();
  feedback('wallet-status', '지갑이 연결되었습니다. 후원 또는 방송인 지갑 연결을 진행할 수 있습니다.');
}));

element('tip-form').addEventListener('submit', event => {
  event.preventDefault();
  if (pending()) return;
  run(async () => {
    const amount = parseNativeAmount(element('tip-amount').value);
    const context = await prepare();
    const payload = await api(`${channelPath}/tip`, { amount_wei: amount.toString(), wallet: context.wallet });
    await send(payload, context, 'tip', amount);
  });
});

element('claim-wallet').addEventListener('click', () => {
  if (pending()) return;
  run(async () => {
    const context = await prepare();
    if (!config.session_verified || isAddress(config.owner_wallet)) throw new Error('이 채널의 본인 인증과 수령 지갑 연결 상태를 확인해 주세요.');
    if (isAddress(config.reserved_wallet) && !sameAddress(context.wallet, config.reserved_wallet)) throw new Error('예약된 수령 지갑으로 연결해 주세요.');
    const challenge = await api(`${channelPath}/wallet-challenge`, { wallet: context.wallet });
    if (typeof challenge.message !== 'string' || !challenge.message) throw new Error('지갑 서명 요청을 확인할 수 없습니다.');
    const message = `0x${Array.from(new TextEncoder().encode(challenge.message), byte => byte.toString(16).padStart(2, '0')).join('')}`;
    feedback('wallet-status', '수령 지갑을 인증하기 위한 메시지를 지갑에서 확인하고 서명해 주세요.');
    await assertAccount(context.wallet, context.revision);
    const signature = await ethereum.request({ method: 'personal_sign', params: [message, context.wallet] });
    await assertAccount(context.wallet, context.revision);
    const payload = await api(`${channelPath}/claim`, { wallet: context.wallet, signature });
    config.reserved_wallet = context.wallet;
    renderConfig();
    await send(payload, context, 'claim');
  });
});

element('withdraw-funds').addEventListener('click', () => {
  if (pending()) return;
  run(async () => {
    const context = await prepare();
    if (!sameAddress(context.wallet, config.owner_wallet)) throw new Error('등록된 수령 지갑으로 연결해야 출금할 수 있습니다.');
    const payload = await api(`${channelPath}/withdraw`, { wallet: context.wallet });
    await send(payload, context, 'withdraw');
  });
});

element('check-transaction').addEventListener('click', () => run(async () => {
  if (!pending()) return;
  if (chainHex(await walletProvider().request({ method: 'eth_chainId' })) !== transaction.chain) {
    await ethereum.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: transaction.chain }] });
  }
  await waitForReceipt();
}));

element('copy-address').addEventListener('click', async () => {
  if (!ready()) return;
  try { await navigator.clipboard.writeText(config.receive_address); feedback('wallet-status', '후원 주소를 복사했습니다. 네트워크도 함께 확인해 주세요.'); }
  catch { feedback('wallet-status', '주소를 길게 누르거나 선택하여 직접 복사해 주세요.', true); }
});

element('logout-channel').addEventListener('click', () => run(async () => {
  await api('/api/auth/logout', {});
  await refresh();
  feedback('wallet-status', '방송 계정 인증에서 로그아웃했습니다.');
}));

if (ethereum && typeof ethereum.on === 'function') {
  ethereum.on('accountsChanged', accounts => {
    setAccount(accounts);
    if (config) feedback('wallet-status', '지갑 계정이 변경되었습니다. 연결된 주소를 확인해 주세요.');
  });
  ethereum.on('chainChanged', () => {
    if (!busy && config) feedback('wallet-status', '지갑 네트워크가 변경되었습니다. 거래 전 후원 네트워크를 다시 확인합니다.');
  });
  ethereum.on('disconnect', () => { setAccount([]); feedback('wallet-status', '지갑 연결이 끊어졌습니다. 다시 연결해 주세요.', true); });
}

run(async () => {
  await refresh();
  if (ethereum) {
    try { setAccount(await ethereum.request({ method: 'eth_accounts' })); }
    catch { setAccount([]); }
  }
  if (ready()) feedback('wallet-status', '');
});
})();
