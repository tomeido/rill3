# 방송 등록, 후원 주소, 방송인 출금

`/register`에서 Twitch 숫자 사용자 ID, YouTube `UC…` 채널 ID 또는 CHZZK 채널 ID를 등록합니다. 같은 플랫폼·채널 ID를 다시 등록해도 기존 채널이나 수령 지갑을 변경하지 않습니다. 표시 이름과 기존 `verification_state`는 소유권 증명이 아닙니다.

`/support/{channel UUID}`에서 채널 전용 컨트랙트 주소, 네트워크, 보관된 ETH와 수령 지갑을 확인합니다. 후원자는 브라우저 지갑으로 후원 거래에 직접 서명합니다. 후원금은 채널 전용 vault에 보관됩니다. 방송인은 공식 플랫폼 OAuth 인증 → 수령 지갑 서명 → 소유권 등록 거래 → 출금 거래 순서로 본인 지갑에 옮깁니다. 최초 소유권 등록과 출금에는 해당 네트워크의 가스비가 필요합니다.

## 배포 설정

체인과 플랫폼 인증을 설정하지 않으면 등록과 조회 화면은 제공하지만 송금 버튼과 후원 주소는 활성화하지 않습니다. 임의의 지갑 주소를 생성하거나 서버에 후원용 개인키를 보관하지 않습니다.

1. [컨트랙트 안내](../contracts/README.md)에 따라 테스트 EVM 네트워크에 `BroadcastVaultFactory`를 배포합니다. 전용 attestor 주소를 constructor에 전달합니다. 배포 전에 로컬 컨트랙트 테스트를 실행합니다.
2. `RILL3_CHAIN_ID`, `RILL3_CHAIN_NAME`, `RILL3_RPC_URL`, `RILL3_VAULT_FACTORY`, `RILL3_FACTORY_CODE_HASH`, `RILL3_ATTESTOR_PRIVATE_KEY`를 함께 설정합니다. 체인 이름에는 테스트넷 여부를 명확히 표시합니다. 기본값 `RILL3_CHAIN_ID=0`은 결제를 비활성화합니다.
3. 공식 플랫폼 개발자 설정에 다음 콜백을 등록합니다. `<origin>`은 `RILL3_PUBLIC_ORIGIN`, `<base>`는 `RILL3_BASE_PATH`입니다.

| 플랫폼 | 인증 정보 | 등록할 콜백 |
| --- | --- | --- |
| Twitch | `TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET` | `<origin><base>/auth/twitch/callback` |
| YouTube | `YOUTUBE_CLIENT_ID`, `YOUTUBE_CLIENT_SECRET` | `<origin><base>/auth/youtube/callback` |
| CHZZK | `CHZZK_CLIENT_ID`, `CHZZK_CLIENT_SECRET` | `<origin><base>/auth/chzzk/callback` |

4. YouTube는 Google OAuth 앱과 YouTube Data API를 활성화합니다. 테스트 상태의 OAuth 앱은 허용된 테스트 계정을 사용합니다. 등록한 실제 채널 계정으로 인증을 확인합니다.
5. 서버를 시작하고 채널을 등록합니다. 지갑에서 **표시된 동일 체인**을 선택해 테스트 ETH로 후원, 다른 계정의 인증 거부, 본인 인증, 소유권 등록, 출금을 검증합니다.

서버는 RPC chain ID, 배포된 factory runtime 코드 해시, attestor 주소를 검사합니다. 한 번 부여한 채널 주소와 체인/factory는 DB에 고정되며 설정 변경으로 다른 수령 주소를 조용히 발급하지 않습니다. 기존 배포를 다른 체인으로 이전하는 기능은 제공하지 않습니다.

이 구현은 Ethereum(1), Sepolia(11155111), Base(8453), Base Sepolia(84532), 로컬 Anvil(31337)의 ETH만 허용합니다. 표준 EVM `CREATE2`와 일반 개인키 지갑의 `personal_sign`을 사용합니다. Abstract/EraVM용 컴파일·주소 계산과 Abstract Global Wallet의 스마트 계정 서명은 검증하지 않았습니다. 기본 체인을 임의로 활성화하지 않으며, 배포 가이드의 표준 EVM 테스트넷에서 먼저 검증해야 합니다. 스마트 컨트랙트 지갑의 ERC-1271 서명, ERC-20 후원, 지갑 변경·복구, 자동 정산, 후원 알림 및 공개 장부 인덱싱은 포함하지 않습니다.

## 자금과 인증 경계

- 후원은 ETH만 지원합니다. 다른 체인이나 토큰을 이 주소로 보내면 이 화면으로 회수할 수 없습니다. 인증 전 후원금도 채널 컨트랙트에 남으므로, 방송인이 인증하지 않는 경우 자동 환불이나 운영자 회수는 없습니다.
- 서버는 **최초 방송인 확인을 증명하는 attestor 개인키**를 사용합니다. 이 키와 OAuth 검증은 최초 소유자 지정의 신뢰 경계입니다. 키가 탈취되면 아직 소유자가 없는 채널이 위험하므로 별도 비밀 저장소에서 관리해야 합니다. 수령 지갑 키와 배포자 키를 이 용도로 재사용하지 마세요.
- 최초로 연결한 지갑은 변경할 수 없습니다. DB는 서명 검증 뒤 지갑을 예약하며 거래를 취소하더라도 같은 지갑으로 다시 서명·제출해야 합니다. 컨트랙트는 소유자 등록 후 운영자와 attestor를 포함한 제삼자의 출금을 허용하지 않습니다.
- OAuth state는 브라우저의 HttpOnly 쿠키에 묶여 단 한 번 사용됩니다. 공식 API가 반환하는 채널 ID가 등록 채널과 정확히 같아야 합니다. 액세스 토큰은 검증하는 동안만 사용하고 DB나 로그에 저장하지 않습니다.
- 방송인 세션은 15분, 지갑 서명 요청은 5분 후 만료됩니다. 서명은 사이트·채널·체인·factory·수령 지갑·nonce에 묶이며 DB에서 원자적으로 소비됩니다. 최초 claim 서명에는 체인·factory·채널·소유자·만료가 포함됩니다.
- 상태 변경 요청은 정확한 Origin, JSON body 크기 제한, 프로세스별 요청 제한을 적용합니다. 인증·후원 API는 `Cache-Control: no-store`를 사용합니다. 공개 서비스의 다중 인스턴스 운영에서는 프록시에서도 IP별 제한을 설정합니다.
- 화면은 거래 해시를 받았다는 이유로 출금 완료를 표시하지 않고 영수증의 성공 및 후속 블록을 확인합니다. 체인 재구성에 대한 최종성 보장이나 회계 장부의 대체는 아닙니다.

## HTTP API

모든 경로 앞에 설정된 `RILL3_BASE_PATH`를 붙입니다. POST에는 `Content-Type: application/json`과 정확한 `Origin`을 전달합니다.

| 경로 | 동작 |
| --- | --- |
| `POST /api/registrations` | `{provider, provider_channel_id, display_name}` → `{channel_id, url}` |
| `GET /api/channels/{id}/web3` | 후원 상태, 체인, 전용 주소, 온체인 잔액, 인증 상태 |
| `POST /api/channels/{id}/tip` | `{wallet, amount_wei}` → 후원 거래 payload |
| `GET /auth/{provider}/start?channel={id}` | 공식 플랫폼 인증 시작 |
| `GET /auth/{provider}/callback` | 일회용 state와 공식 채널 소유권 검증 |
| `POST /api/channels/{id}/wallet-challenge` | 인증 세션 + `{wallet}` → 서명할 message |
| `POST /api/channels/{id}/claim` | `{wallet, signature}` → 최초 소유자 등록 거래 payload |
| `POST /api/channels/{id}/withdraw` | `{wallet}` → 현재 소유자에게 전액 출금하는 거래 payload |
| `POST /api/auth/logout` | 방송인 세션과 남은 challenge 폐기 |

거래 payload는 `{from, to, data, value, chain_id}`입니다. 서버는 거래를 대신 전송하지 않습니다. 소유권 등록 후 출금 권한은 온체인 소유자 지갑에 있으므로 OAuth 세션이 만료되어도 해당 지갑으로 출금할 수 있습니다.

## 검증

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
RILL3_TEST_DATABASE_URL=postgres://... cargo test --workspace --all-targets --locked -- --test-threads=1
node --test static/js/wallet.test.mjs
cd contracts && forge test
```

명시적으로 로컬 Anvil 연동까지 실행하려면 [컨트랙트 로컬 테스트 안내](../contracts/README.md)에 따라 factory를 배포하고 `RILL3_TEST_EVM_RPC`, `RILL3_TEST_EVM_FACTORY`, `RILL3_TEST_EVM_CODE_HASH`를 설정한 뒤 아래 명령을 실행합니다.

```sh
cargo test -p rill3 web3::evm_tests::local_evm_registration_donation_owner_claim_and_withdrawal -- --ignored
cargo test -p rill3-payments --test local_evm -- --ignored
```

HTTP 통합 테스트는 로컬 DB에만 방송인 인증 세션 fixture를 만들고 실제 지갑 서명·온체인 거래·출금 잔액을 검증합니다. 실제 플랫폼 OAuth 승인을 대체하지 않습니다.

DB 검증에는 운영 DB가 아닌 폐기 가능한 테스트 DB를 사용합니다. OAuth 테스트는 로컬 mock API로 잘못된 인증 응답과 사용자 식별을 검증하며 실제 플랫폼 앱의 승인·동의 화면은 별도 테스트 계정으로 확인해야 합니다.
