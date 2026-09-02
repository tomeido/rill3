# Codex용 RILL3 구현 지시서

아래 내용을 새 저장소의 첫 요청으로 그대로 사용한다. 이 요청에서는 **M0와 M1만 구현**하게 하고, 온체인 후원과 TTS는 인터페이스/문서까지만 만든다.

---

당신은 수석 Rust 백엔드 엔지니어, 보안 엔지니어, 스마트 컨트랙트 설계자 역할을 함께 맡는다.

프로젝트 이름은 **RILL3(릴쓰리)**다.

## 제품 정의

RILL3는 Twitch, YouTube, CHZZK 등 외부 라이브 방송을 공식 API와 공식 임베드로 모으고, 이후 비수탁형 온체인 후원과 OBS용 TTS/영상 알림을 제공할 Rust-first 플랫폼이다.

핵심 원칙:

1. MVP에서 영상 바이트를 프록시하거나 재송출하지 않는다.
2. 비공개 API, HLS 추출, 웹 화면 스크래핑을 하지 않는다.
3. 임베드가 명확하지 않은 플랫폼은 링크 카드만 제공한다.
4. 시청자 수가 늘어도 외부 플랫폼 API 호출 수가 늘면 안 된다.
5. 기본 페이지는 Rust SSR이며 대형 SPA를 만들지 않는다.
6. 홈에서는 iframe을 하나도 만들지 않는다.
7. 지갑과 플레이어 SDK는 사용자 동작 시 지연 로딩한다.
8. 마이크로서비스, Kubernetes, Kafka, Redis를 넣지 않는다.
9. PostgreSQL을 DB와 간단한 작업 큐/lock/notify에 활용한다.
10. 모든 외부 이벤트 처리는 idempotent해야 한다.

## 기술 선택

- Rust stable, edition 2024
- Tokio
- Axum + Tower
- Askama SSR
- HTMX와 최소한의 vanilla TypeScript
- PostgreSQL + SQLx
- reqwest + serde
- tracing
- clap subcommands
- utoipa/OpenAPI
- Docker Compose + Caddy
- rustfmt, clippy `-D warnings`, cargo test
- SQLx offline metadata
- 단위/통합 테스트
- 임베드 E2E 검증이 필요할 때만 Playwright

특정 crate 버전은 공식 최신 stable 호환 조합을 조사하고 정확히 pin한다. Cargo.lock을 커밋한다.

## 저장소 목표 구조

```text
rill3/
├─ apps/rill3/
├─ crates/domain/
├─ crates/db/
├─ crates/auth/
├─ crates/providers/
├─ crates/payments/
├─ crates/alerts/
├─ crates/moderation/
├─ crates/telemetry/
├─ templates/
├─ static/
├─ migrations/
├─ contracts/
├─ deploy/
├─ docs/adr/
├─ tests/
├─ .env.example
├─ justfile
└─ README.md
```

한 바이너리에 다음 subcommand를 둔다.

```text
rill3 server
rill3 worker
rill3 indexer
```

이번 작업에서는 `server`와 provider sync를 위한 `worker`만 동작시키고, `indexer`는 컴파일 가능한 stub으로 둔다.

## 먼저 해야 할 일

코드를 쓰기 전에 아래 문서를 생성한다.

- `PLAN.md`
- `docs/ARCHITECTURE.md`
- `docs/SECURITY.md`
- `docs/PROVIDER_POLICY.md`
- `docs/adr/0001-rust-ssr-over-spa.md`
- `docs/adr/0002-postgres-before-redis.md`
- `docs/adr/0003-official-embed-or-link-only.md`

`PLAN.md`에는 이번 작업에서 수정할 파일, 단계, 검증 명령, 아직 구현하지 않을 범위를 명시한다.

## M0 구현

1. Rust workspace와 crate 경계를 만든다.
2. `GET /health/live`, `GET /health/ready`를 구현한다.
3. PostgreSQL migration과 연결 상태 확인을 만든다.
4. Caddy와 Docker Compose를 만든다.
5. `.env.example`에 필요한 설정 이름만 넣고 비밀값은 넣지 않는다.
6. GitHub Actions 또는 동등한 CI:
   - cargo fmt --check
   - cargo clippy --workspace --all-targets -- -D warnings
   - cargo test --workspace
   - migration/SQLx 검증
7. Askama 기반 기본 레이아웃과 직접 작성한 작은 CSS를 만든다.
8. 외부 웹폰트와 UI 프레임워크를 사용하지 않는다.
9. 구조화 로그와 request ID를 넣는다.
10. graceful shutdown을 구현한다.

## M1 도메인과 DB

다음 모델과 migration을 구현한다.

- creators
- external_channels
- live_sessions
- provider_events

필수 unique key:

- creators.slug
- `(provider, provider_channel_id)`
- `(provider, external_event_id)`

금지:

- provider raw payload를 무기한 저장
- 임의 URL을 신뢰하고 서버에서 fetch
- DB enum 때문에 migration이 경직되는 구조

## Provider 추상화

다음 의미의 trait를 만든다. 필요하면 타입 이름은 개선하되 기능을 유지한다.

```rust
#[async_trait::async_trait]
pub trait StreamProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn embed_capability(&self) -> EmbedCapability;
    async fn verify_channel(
        &self,
        input: VerifyChannelInput,
    ) -> Result<VerifiedChannel, ProviderError>;
    async fn fetch_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError>;
    async fn reconcile_many(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError>;
    fn external_url(&self, channel: &ExternalChannel) -> url::Url;
    fn embed_descriptor(&self, live: &LiveState) -> Option<EmbedDescriptor>;
}
```

구현체:

### MockProvider
- 테스트에서 online/offline/중복/역순 이벤트를 재현
- 네트워크 없이 통합 테스트 가능

### TwitchProvider
- 공식 Helix API
- `stream.online`, `stream.offline` EventSub webhook endpoint
- HMAC 서명과 timestamp/replay 검증
- app access token cache
- 등록 채널 reconciliation
- rate-limit header와 429 backoff
- embed descriptor에 channel과 parent 요구사항 표현
- 실제 secret이 없을 때 앱은 시작되되 provider는 disabled 상태로 표시

### YouTubeProvider
이번 마일스톤에서는:
- channel ID와 현재 live URL 수동 등록
- WebSub challenge/callback endpoint
- OAuth와 `liveBroadcasts`는 trait/interface와 TODO 문서까지만
- public search API 반복 polling을 구현하지 않음
- 공식 iframe descriptor에 video ID와 origin을 표현

### ChzzkProvider
- 공식 CHZZK Developers API만 사용
- live status adapter
- 30~60초 기본 간격과 jitter
- 429/5xx 지수 백오프
- 공식 임베드가 확실하지 않으므로 `LinkOnly`
- secret이 없으면 disabled

### LinkOnlyProvider
- 허용된 `https` URL만 보관
- URL을 서버가 fetch하지 않음
- 직접 입력한 상태는 만료 시 offline/unknown
- arbitrary iframe HTML 금지

## 웹 페이지

### `GET /`
- live creators 우선
- 카드: 이름, provider, title, category, thumbnail, started_at
- iframe 0개
- 이미지 lazy loading
- 빈 상태, provider 장애 상태
- DB 쿼리 N+1 금지

### `GET /c/{slug}`
- creator와 현재 channel
- `OfficialEmbed`이면 provider별 안전한 embed descriptor로 iframe 생성
- `LinkOnly`면 외부 보기 버튼
- Twitch `parent`, YouTube `origin`을 config에서 생성하며 사용자 입력을 쓰지 않음
- 후원 패널은 “준비 중” placeholder만 둠
- iframe 위에 overlay를 올리지 않음

### `GET /live.json`
- 정규화된 live snapshot
- ETag
- `Cache-Control: public, max-age=10, stale-while-revalidate=60`
- 외부 API를 요청 경로에서 호출하지 않음

### 임시 개발용 creator/channel 관리
- production에서 자동 노출되지 않는 관리자 CLI 또는 seed fixture
- 인증 없는 공개 write API는 만들지 않음

## Worker

- DB에 등록된 채널을 provider별 batch로 가져옴
- advisory lock으로 중복 scheduler 방지
- interval + jitter
- provider별 timeout
- partial failure 허용
- 마지막 정상 상태와 stale 상태 구분
- 상태 변화가 있을 때만 live_sessions 갱신
- 동일 이벤트가 여러 번 들어와도 한 번 처리
- SIGTERM graceful shutdown

## 성능 조건

- 홈 HTML 압축 전에도 불필요하게 크지 않게 유지
- 공통 CSS 25KB 이하
- 기본 JS 20KB 이하
- 홈 iframe 0개
- creator 페이지 iframe 최대 1개
- 외부 웹폰트 0개
- 시청 HTTP 요청 중 외부 provider API 호출 0회
- 기본 DB pool 최대 20
- pagination 없이 무한 전체 목록을 내려주지 않음

간단한 부하 테스트 스크립트 또는 명령을 README에 넣는다.

## 보안 조건

- CSP를 설정하고 `frame-src`를 provider allowlist로 제한
- `frame-ancestors`, HSTS, X-Content-Type-Options, Referrer-Policy
- webhook body size 제한
- request timeout
- secrets 로그 금지
- OAuth/API token DB 평문 저장 금지; 이번 마일스톤에서 저장이 필요하면 암호화 abstraction을 먼저 설계
- arbitrary URL fetch/redirect follow 금지
- SSRF 테스트
- duplicate webhook 테스트
- timestamp replay 테스트

## 테스트 시나리오

최소한 다음 테스트를 작성한다.

1. mock provider가 online → offline으로 바뀐다.
2. 같은 external event 두 번 처리해도 세션이 하나다.
3. offline 이벤트가 늦게 도착해 더 최신 online을 덮지 않는다.
4. 유효하지 않은 Twitch signature가 거절된다.
5. 오래된 webhook timestamp가 거절된다.
6. LinkOnly에 `javascript:`, `file:`, 사설 IP 형태 URL이 거절된다.
7. 홈 렌더에 iframe이 없다.
8. creator 페이지의 Twitch embed에 신뢰된 parent만 들어간다.
9. YouTube embed에 신뢰된 origin만 들어간다.
10. `/live.json`이 외부 네트워크를 호출하지 않는다.
11. provider 429가 전체 worker를 죽이지 않는다.
12. DB 장애 시 readiness만 실패하고 liveness는 유지된다.

## 이번 작업에서 구현하지 말 것

- Solidity 실제 컨트랙트
- 지갑 연결
- 결제
- TTS
- OBS overlay
- 영상 업로드
- 자체 스트리밍
- 통합 채팅
- Redis/NATS/Kafka
- Kubernetes
- 멀티체인
- NFT/토큰/DAO
- 공개 회원가입/관리자 패널

다만 `payments`, `alerts`, `indexer` crate에는 향후 경계를 설명하는 README와 컴파일 가능한 최소 타입만 둘 수 있다.

## 완료 보고 형식

작업을 마치면 다음 순서로 보고한다.

1. 구현한 수직 기능
2. 변경 파일 트리
3. 실행 방법
4. 테스트와 실제 결과
5. 성능 예산 결과
6. 보안 검증 결과
7. 외부 API secret이 없어 검증하지 못한 부분
8. 알려진 위험과 다음 한 단계

테스트를 실행하지 않고 “통과할 것”이라고 말하지 않는다. 막힌 부분은 숨기지 말고 최소 재현과 원인을 적는다.

---

## 다음 마일스톤 요청문

### M2 요청

`RILL3_MASTER_PLAN.md`의 M2만 구현하라. 기존 아키텍처를 읽고 wallet nonce login, creator studio, external OAuth verification, payout wallet EIP-712 verification, rotating OBS key를 수직 기능으로 완성하라. nonce에는 domain, chain ID, purpose, expiry를 넣고 사용 후 폐기하라. UI는 Askama/HTMX를 유지하고 새로운 SPA를 만들지 마라. 작업 전 PLAN.md를 갱신하고 테스트 후 결과를 보고하라.

### M3 요청

`RILL3_MASTER_PLAN.md`의 M3만 구현하라. Abstract testnet과 local Anvil을 대상으로 non-upgradeable CreatorRegistryV1/TipRouterV1, tip intent API, lazy wallet island, Alloy indexer, reorg/idempotency, public ledger를 구현하라. 컨트랙트는 정상 흐름에서 잔액을 보관하지 않고 수수료는 0%다. Foundry unit/fuzz/invariant test를 실제 실행하라. 메시지 원문이나 URL을 온체인에 기록하지 마라.

### M4 요청

`RILL3_MASTER_PLAN.md`의 M4만 구현하라. PostgreSQL `FOR UPDATE SKIP LOCKED` job queue, moderation, TTS provider trait, mock provider, one production provider adapter, object storage, OBS SSE overlay, ACK/skip/mute/test alert를 구현하라. 결제 확정 전 TTS를 생성하지 말고, alert state transition을 모두 idempotent하게 만들어라.

### M5 요청

`RILL3_MASTER_PLAN.md`의 M5만 구현하라. YouTube/Twitch Clip 공식 URL parser와 embed validation, start/end/duration rule, allow/block list, OBS playback을 추가하라. arbitrary iframe, arbitrary server-side URL fetch, 영상 다운로드/재호스팅은 금지한다. 한 오버레이에서 자동재생 플레이어는 한 번에 하나만 허용한다.

### M6 요청

`RILL3_MASTER_PLAN.md`의 M6만 구현하라. load test, accessibility, rate limiting, backup/restore, provider outage state, privacy/terms/moderation 운영 문서, contract review checklist를 완성하라. 실제 측정값을 문서화하고 목표 미달을 숨기지 마라.
