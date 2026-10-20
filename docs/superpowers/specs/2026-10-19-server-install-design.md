# 설계: MCP로 서버 한 대 설치·실행 (`arcus-cli --mcp`)

날짜: 2026-10-19

## 배경과 목적

dev-1~3에 ARCUS 클러스터를 올리면서 ZooKeeper 설치·실행, arcus-memcached(공개판·EE)
빌드·실행, 서버용 znode 구성을 전부 손으로 했다. 실수하기 쉽고(예: `/arcus_acl_mapping/<svc>`
값이 비어 `zoo_get_children(/arcus_acl/) failed: bad arguments`로 기동 실패), 실수의 원인도
잘 드러나지 않는다.

arcus-cli가 **서버 한 대 안에서** 이 과정을 대신한다. `arcus-cli --mcp`로 실행하면 arcus-cli가
MCP 서버가 되고, Claude·Gemini·GPT 같은 AI 클라이언트가 그 도구로 소스를 받아 빌드하고,
ZooKeeper와 서버를 띄우고, 필요한 znode를 만들고, memcached·ZK 명령을 실행한다.
사용자는 AI에게 말로 요청하고, 켤 기능과 이름은 AI가 사용자에게 물어 도구 인자로 넘긴다.

이 문서는 전체 계획 중 1번 프로젝트다.

1. **서버 한 대 설치·실행 (이 문서)**
2. 감시·알림: 서버를 자식으로 띄운 arcus-cli가 종료를 감지하고, ZK watch와 함께 슬랙/Claude로
   알림과 userlog 기반 원인 분석을 보낸다. 자동 재시작은 하지 않는다(사람이 판단).
3. 원격 배포: 사용자 확인 후 scp로 arcus-cli를 옮기고 ssh로 실행한다.

(처음 계획의 4번 MCP는 1번에 합쳤다. 새 기능의 사용 방식이 MCP 자체이기 때문이다.)

1번은 2~3번의 기반이므로 아래 "뒤 프로젝트를 위한 제약"을 지켜야 한다.

## 목표

- `arcus-cli --mcp`가 stdio MCP 서버로 동작한다. Claude Code, Gemini CLI, Codex/ChatGPT 등
  stdio MCP를 지원하는 클라이언트에 등록해 쓴다.
- AI가 도구로 에디션, 버전, 기능을 고르면 소스를 받아 빌드까지 끝낸다.
- 공개판은 GitHub 릴리즈 소스를, EE는 비공개 GitHub 저장소를 받는다. EE 접근 정보가 없으면
  사용자가 등록하는 방법을 안내한다(아래 "소스 받기").
- 기존 기능도 도구로 쓴다: memcached 명령 전송, ZK 조회·생성·값 설정·비밀번호 저장.
- 빌드에 필요한 시스템 패키지가 없으면 무엇을 설치해야 하는지 알려주고 멈춘다.
- 로컬 ZooKeeper를 받아 띄우거나, 이미 있는 ZK 앙상블 주소를 받아 쓴다.
- 고른 기능에 맞는 znode를 만들고, 만든 뒤 다시 읽어 확인한다.
- 서버를 arcus-cli의 **자식 프로세스**로 띄우고, 로그는 인스턴스 폴더의 userlog에 남긴다.
- `status`, `stop`으로 상태 확인과 중지를 한다.

## 비목표

- 여러 서버 동시 설치, scp/ssh (3번)
- 감시 알림(슬랙/Claude), 원인 분석 (2번). 단, 종료 정보 기록까지는 1번에 포함한다.
- 사람용 대화형 마법사, `server` 같은 새 하위 명령 (새 기능은 MCP 도구로만 제공)
- 기존 사용법 변경. `--host`, `--zookeeper` 등은 지금처럼 동작한다.
- MCP의 resources, prompts. 도구(tools)만 제공한다.
- AI에게 znode 삭제 도구를 주는 것
- 자동 재시작, systemd 유닛 생성
- znode 삭제, 클러스터 해체, 언인스톨(로컬 인스턴스 폴더 삭제 외)
- Windows
- 여러 서버에 걸친 ZK 앙상블을 직접 띄우기 (3번 원격 배포에서 다룸. 한 서버 안의 여러 노드 앙상블은 지원)

## 반목표 (돌아가도 실패로 보는 것)

- 이미 있는 znode의 값을 묻지 않고 덮어쓰기. 공용 클러스터를 망가뜨릴 수 있다.
- EE 접근 토큰을 사용자 동의 없이 디스크에 평문 저장하거나, 로그·화면에 노출하기.
- 묻지 않고 sudo를 쓰거나 시스템 경로(`/usr/local` 등)에 설치하기.
- 빌드 실패 원인을 숨기기(로그 위치와 마지막 오류 줄을 반드시 보여준다).
- EE 토큰이나 비밀번호가 AI 대화를 거쳐 오가게 만들기(대화 기록에 남는다).
- MCP 모드에서 stdout에 프로토콜 외의 내용을 쓰기(클라이언트 연결이 깨진다).

## 사용자가 정한 것

- 실행 방식: 각 서버에 arcus-cli를 두고 그 서버에서 실행한다. SSH 없이 완결되어야 한다.
- 서버 실행: arcus-cli가 **자식 프로세스로** 띄운다(2번에서 `waitpid`로 종료 코드·시그널을 얻기 위함).
- 로그: userlog 확장(`userlog_logger`)을 쓰고, arcus-cli가 관리하는 폴더에 남긴다.
- 기능 선택: **ZK 연동, SASL, 복제**를 기본 추천으로 먼저 보여준다. 나머지(persistence,
  sticky item, migration, 엔진 선택)는 사용자가 원할 때만 펼쳐 보여준다.
- EE 소스를 못 받으면 계정을 물어보고, 빌드까지 실행해 준다.
- 사용 방식: `arcus-cli --mcp`로 MCP 서버가 된다. 기존 사용법은 그대로 두고, 겹치는 기능은
  기존 코드를 재사용해 도구로 제공한다.

## 설계

### MCP 서버

- `arcus-cli --mcp [--home <dir>]`. 다른 옵션과 함께 쓰면 오류. (내 결정: 도구 하나하나는 인자로 대상을 받는다)
- 전송: stdio, 줄 단위 JSON-RPC 2.0. MCP `initialize`, `tools/list`, `tools/call`, `ping`과
  알림(`notifications/initialized`)을 처리한다. 기존 코드가 동기식이라 MCP SDK(`rmcp`, tokio 기반)
  대신 직접 구현한다. (내 결정)
- stdout은 프로토콜 전용이다. 진단 메시지는 stderr, 자식 프로세스 출력은 로그 파일로 보낸다.
- 도구 결과는 사람이 읽는 텍스트와 함께 `structuredContent`(JSON)를 담는다. 실패는 `isError: true`와
  원인·다음 행동(예: "`service_code` 인자가 필요합니다. 사용자에게 서비스 코드를 물어보세요")을 담는다.

### 도구

새 기능:

| 도구 | 하는 일 |
|---|---|
| `arcus_list_versions` | 에디션별 설치 가능한 버전 목록(최신순) |
| `arcus_list_features` | 에디션별 기능 목록. 추천(ZK 연동, SASL, 복제)과 고급을 구분해 돌려준다 |
| `arcus_check_prerequisites` | 빌드 도구 점검, 빠진 것과 OS별 설치 명령 |
| `arcus_ee_login` | gh 브라우저 로그인을 시작하고 일회용 코드와 주소를 돌려준다 |
| `arcus_install` | 소스 받기와 빌드를 백그라운드 작업으로 시작하고 작업 id를 돌려준다 |
| `arcus_job_status` | 작업 진행 단계, 완료·실패, 실패 시 로그 경로와 마지막 줄 |
| `arcus_zk_start` / `arcus_zk_stop` | 로컬 단일 노드 ZooKeeper |
| `arcus_setup` | 인스턴스 생성과 znode 계획·적용·확인 |
| `arcus_start` / `arcus_stop` / `arcus_status` | 서버 기동·중지·상태 |

기존 기능 재사용:

| 도구 | 재사용하는 코드 |
|---|---|
| `memcached_command` | `request::split`으로 요청을 나누고 `Transport`로 보낸 뒤 응답을 모아 돌려준다(TCP·Unix) |
| `zk_ls`, `zk_get`, `zk_create`, `zk_set` | `zk` 모듈의 조회·생성·값 설정 |
| `zk_password` | `zk` 모듈의 SCRAM-SHA-256 저장(`<path>/authPassword`). 비밀번호는 인자로 받지 않고 임의로 만들어 `~/.arcus-cli/secrets/` 아래 권한 600 파일에 저장한 뒤 경로만 돌려준다 (내 결정) |

재사용을 위해 기존 코드는 출력 대상을 바꿀 수 있게 고친다. 지금은 `Transport`와 ZK 명령이 결과를
바로 stdout에 쓰는데, 쓰는 대상을 인자로 받게 해서 기존 REPL은 stdout을, MCP 도구는 버퍼를 넘긴다.
기존 REPL 동작은 바뀌지 않는다.

`memcached_command`의 응답 수집: 요청을 보낸 뒤 마지막 바이트로부터 일정 시간(기본 300ms, 인자로
조정) 더 오는 게 없으면 끝난 것으로 본다. 최대 대기 시간과 최대 크기를 둔다. (내 결정)

### 대화형 질문을 도구 인자로 바꾸는 규칙

- 사용자가 정할 수 있는 값은 모두 도구 인자로 노출한다(ZK 노드 개수와 포트, 서버 포트, 메모리, 서비스 코드,
  그룹 이름, 버전 등). 기본값이 있는 인자는 "추천값"이다. 도구 설명에 추천값을 적고, AI가 사용자에게
  추천값을 제시해 확인받은 뒤 넘기도록 지시한다. (사용자 결정)

- 사람에게 묻던 값(에디션, 버전, 기능, 포트, 메모리, ZK 주소, 서비스 코드, ACL 그룹, 복제 그룹 등)은
  모두 도구 인자다. 이름(서비스 코드·ACL 그룹·복제 그룹)은 기본값이 없는 필수 인자이고,
  빠지면 "사용자에게 물어보라"는 오류를 돌려준다. 도구 설명(description)에도 같은 지시를 적는다.
- 확인이 필요한 쓰기는 두 번 부르는 방식으로 한다.
  - `arcus_setup`은 `apply`가 없으면(기본 false) 만들 znode 목록과 기존 값과의 차이만 돌려준다.
    `apply: true`일 때만 쓴다.
  - 값이 다른 기존 znode는 `overwrite`에 경로를 명시한 것만 덮어쓴다. 기본은 그대로 둔다.
  - `arcus_stop`은 SIGTERM만 보낸다. 시간이 지나도 살아 있으면 그렇다고 돌려주고, `force: true`로
    다시 부를 때만 SIGKILL을 보낸다.
- 오래 걸리는 일(다운로드, 빌드, JRE 받기)은 작업 id를 돌려주고 `arcus_job_status`로 확인한다.
  작업은 MCP 서버와 분리된 백그라운드 프로세스로 돌려서, 클라이언트가 끊겨도 계속된다.

### 작업 폴더

기본 위치는 `~/.arcus-cli/`이고 `--mcp --home <dir>`로 바꿀 수 있다. (내 결정)

```
~/.arcus-cli/
  downloads/                       받은 tar 캐시 (체크섬 검증 후 보관)
  builds/<edition>-<version>-<features>/   빌드 산출물 설치 경로(prefix): deps + memcached
  zookeeper/<version>/             압축 푼 Apache ZooKeeper
  zk/                              로컬 ZK 설정(zoo.cfg)과 데이터, 로그
  jobs/<id>/                       백그라운드 작업 상태(status.json)와 로그
  gh/                              gh 바이너리 (시스템에 gh가 없을 때만)
  instances/<name>/
    instance.toml                  선택 내용 (에디션, 버전, 기능, 포트, 메모리, ZK 주소, 서비스 코드…)
    ARCUSlog/                      userlog (서버 작업 디렉터리를 이 폴더로 지정해서 생김)
    supervisor.pid, server.pid
    last_exit.json                 마지막 종료 정보 (시각, 종료 코드, 시그널)
    build.log, run.log             빌드/표준출력 로그
```

같은 에디션·버전·기능 조합의 빌드는 재사용한다.

### 기능 선택과 빌드 옵션

`arcus_list_features`가 추천 3가지를 먼저(`recommended: true`) 돌려준다. 도구 설명에
"추천 기능부터 사용자에게 제안하고, 고급 기능은 사용자가 원할 때만 제안하라"고 적는다.

| 기능 | configure | 비고 |
|---|---|---|
| ZK 연동 | `--enable-zk-integration --with-zookeeper=<prefix>` | 기본 켬 |
| SASL | `--enable-sasl` | 켜면 ACL znode 구성 단계가 추가됨 |
| 복제 | `--enable-replication` | EE에서만 보임. 켜면 그룹 구성 단계가 추가됨 |

고급 기능(`recommended: false`):

| 기능 | configure | 에디션 |
|---|---|---|
| persistence | `--enable-persistence` | 공통 |
| sticky item | `--enable-sticky-item` | 공통 |
| migration | `--enable-migration` | EE |
| 엔진 | `--enable-squall-engine` / `--enable-gust-engine` | EE |

모든 빌드에 `--prefix=<build prefix> --with-libevent=<build prefix>`를 붙인다.
옵션 이름은 두 저장소의 `configure.ac`에서 확인했다(2026-10-19 기준).

### 소스 받기

- **공개판**: `naver/arcus-memcached` GitHub 릴리즈의 소스 tar
  (`arcus-memcached-<version>.tar.gz`). `make dist` 결과물이라 `configure`와 `deps/`가 이미
  들어 있다(autorun 불필요). 체크섬은 GitHub API가 주는 파일별 sha256으로 검증한다.
  버전 기본값은 최신 릴리즈이고, userlog·SASL auxprop이 들어간 **1.15.0 이상**만 보여준다.
- **EE**: 비공개 저장소 `jam2in/arcus-memcached-EE`의 GitHub 릴리즈 소스 tar.
  예: `https://github.com/jam2in/arcus-memcached-EE/releases/download/0.12.1-E/arcus-memcached-EE-0.12.1.tar.gz`
  (태그 `<version>-E`, 파일 `arcus-memcached-EE-<version>.tar.gz`). 버전 목록과 기본값(최신)은 릴리즈 목록에서 가져온다.
  GitHub 계정이 있어야 받을 수 있다.
  1. 토큰을 `GITHUB_TOKEN` → `gh auth token` 순으로 찾는다.
  2. 없거나 401/403/404이면 도구는 오류와 함께 `arcus_ee_login`을 부르라고 안내한다. 비밀 값을
     AI 대화로 받지 않기 위해 브라우저 로그인을 쓴다. jam2in 조직에 앱을 등록할 권한이 없으므로
     GitHub가 만든 GitHub CLI(`gh`)의 로그인을 빌려 쓴다. (사용자 결정)
     - `arcus_ee_login`은 `gh auth login --hostname github.com --web --git-protocol https`를
       백그라운드 작업으로 실행하고, gh가 출력한 일회용 코드와 `https://github.com/login/device`를
       돌려준다. AI는 이를 사용자에게 보여주고, 사용자는 EE 권한이 있는 계정으로 브라우저에서 승인한다.
       완료 여부는 `arcus_job_status`로 확인한다.
     - 토큰은 gh가 자기 저장소(키체인 등)에 보관한다. arcus-cli는 토큰을 따로 저장하지 않는다.
     - gh가 없으면 GitHub 공식 릴리즈의 gh 바이너리 tar를 `~/.arcus-cli/gh/`에 받아 쓴다(체크섬 검증,
       시스템에 설치하지 않음).
     - 로그인했는데도 EE 저장소가 안 보이면(조직의 OAuth 앱 접근 제한 등) 그 사실과 `GITHUB_TOKEN`
       환경 변수를 MCP 클라이언트 설정에 넣는 방법을 안내한다.
  3. 비공개 저장소의 `releases/download/...` 주소는 토큰으로 바로 받을 수 없다. 대신 API로 받는다.
     `GET /repos/jam2in/arcus-memcached-EE/releases/tags/<tag>`로 asset id를 얻은 뒤,
     `GET /repos/jam2in/arcus-memcached-EE/releases/assets/<id>`를 `Accept: application/octet-stream`으로 요청한다.
  4. 토큰은 도구 결과, 로그, 오류 메시지 어디에도 나오지 않는다.
- 받은 파일은 `downloads/`에 두고, 다시 쓸 때는 받지 않는다.

### 빌드

1. 사전 점검: gcc(또는 cc), make, libtool, autoconf/automake(git 소스일 때), perl 등.
   빠진 게 있으면 OS에 맞는 설치 명령(yum/dnf, apt, brew)을 보여주고 멈춘다. 직접 설치하지 않는다.
2. 의존성: 소스에 포함된 `deps/install.sh <build prefix>`로 libevent, arcus-zookeeper C 클라이언트
   (3.5.9-p4), cyrus-sasl을 prefix에 설치한다. 이 스크립트는 기능과 상관없이 셋 다 빌드하며,
   로그를 `deps/install.log`에 쓰므로 그 내용을 `build.log`에 합친다.
3. git 소스면 `config/autorun.sh`, 그다음 `configure <옵션>`, `make`, `make install`.
4. 모든 출력은 `build.log`로 보낸다. `arcus_job_status`는 진행 단계만 보여주고, 실패하면 로그 경로와 마지막 오류 줄을 돌려준다.

### ZooKeeper

- **로컬로 띄우기**: Apache ZooKeeper 바이너리 tar를 받아 SHA-512를 검증하고 `zookeeper/<version>/`에 푼다.
  노드 개수와 노드별 client/peer/election 포트를 지정할 수 있다(추천: 3노드, client 2181~2183,
  peer 2888~2890, election 3888~3890; 1노드면 standalone). 노드마다 `zk/node<i>/`에 zoo.cfg, data(myid),
  로그, pid를 두고 각각 백그라운드 프로세스로 실행한다. 다 뜬 뒤 앙상블 주소(`127.0.0.1:<client>,...`)를 돌려준다.
  Java가 없으면 설치 방법을 오류로 돌려준다. `download_jre: true`로 부르면 JRE tar를 받아 그걸로 실행한다.
- **기존 앙상블 쓰기**: `host:port,...`를 받아 연결만 확인한다.

### znode 구성

ZK 연동을 켰을 때 `arcus_setup`이 만든다. 기존 `zookeeper` crate와 ZK 모드의 SCRAM 비밀번호 함수를 재사용한다.

일반 클러스터 (루트 `/arcus`):
- `/arcus/cache_list/<svc>`
- `/arcus/cache_server_mapping/<ip>:<port>/<svc>`
- `/arcus/cache_server_log`

복제 클러스터 (EE, 루트 `/arcus_repl`):
- `/arcus_repl/cache_list/<svc>`
- `/arcus_repl/cache_server_mapping/<ip>:<port>/<svc>^<group>^<repl ip>:<repl port>`
- `/arcus_repl/group_list/<svc>/<group>`
- `/arcus_repl/cache_server_log`

SASL (`sasl_auxprop.c`, EE `docs/administration/sasl.md` 기준):
- `/arcus_acl_mapping/<svc>`: 값 = ACL 그룹 이름 (**비어 있으면 서버가 기동하지 못한다**)
- `/arcus_acl/<group>/<user>`: 값 = 권한
- `/arcus_acl/<group>/<user>/authPassword`: SCRAM-SHA-256 형식 비밀번호

서버는 자기 매핑을 `ip:port` → `hostname:port` → `ip` 순서로 찾고, 자기 IP는 ZK에 연결할 때
OS가 고른 로컬 주소로 정한다(`ARCUS_CACHE_PUBLIC_IP`로 덮어쓸 수 있음). `setup`도 같은 방식으로
IP를 계산해 매핑을 만든다. 로컬 ZK(127.0.0.1)에 붙으면 IP가 127.0.0.1이 된다.

migration(EE 고급 옵션)은 `<root>/cloud_stat/<svc>/...`를 쓴다. 서버가 미리 있기를 기대하는
노드는 빌더가 `arcus_zk.c`에서 확인해 목록에 넣는다.

SASL 구성 방식 (사용자 결정):
- **로컬에 새로 띄운 ZK**: ACL 그룹과 user, 비밀번호 znode를 ZK ACL 없이 만든다(개발용).
- **이미 있는 앙상블**: `/arcus_acl_mapping/<svc>`만 사용자가 말한 기존 그룹에 연결한다.
  `/arcus_acl/<group>` 아래 user는 만들거나 바꾸지 않는다. 그룹이 없으면 알리고 멈춘다.

이름은 사용자에게 묻는다 (사용자 결정): 서비스 코드, ACL 그룹 이름, 복제 그룹 이름은 필수 인자다.
AI가 사용자에게 물어 넘긴다. 임의로 정하지 않는다. SASL user의 비밀번호는 AI 대화를 거치지
않게 한다. 로컬 ZK일 때 arcus-cli가 임의 비밀번호를 만들어 `instances/<name>/sasl_password`
(권한 600)에 저장하고, 도구 결과에는 그 파일 경로만 돌려준다. (내 결정)

복제 범위 (사용자 결정): 인스턴스 하나가 노드 하나다. `setup`은 이 노드를 사용자가 정한 복제
그룹에 등록만 한다. 짝 노드는 다른 서버나 같은 서버에서 `setup`을 한 번 더 실행해 붙인다.

규칙:
- 없는 znode만 만든다. 이미 있고 값이 다르면 `overwrite`에 명시된 경로만 덮어쓴다.
- 다 만든 뒤 다시 읽어 확인한다. 빈 매핑 값이나 매핑 개수가 1이 아닌 경우를 오류로 알린다.
- `apply: true` 없이는 목록과 차이만 돌려준다.

위 경로는 `arcus_zk.c`(공개판·EE)와 `sasl_auxprop.c`에서 확인했다. 그러나 부모 노드가 미리
있어야 하는지, cache_list znode의 값, 권한 값의 형식은 빌더가 코드와 기존 ARCUS 스크립트로
최종 확인한다.

### 서버 실행

- `arcus_start`는 백그라운드 감독 프로세스(숨김 옵션 `arcus-cli --supervise <name>`, 내부용)를 띄우고 바로 돌아온다.
  감독 프로세스가 memcached를 **자식**으로 실행한다.
- 자식의 작업 디렉터리는 `instances/<name>/`이다. 그래서 userlog가 `instances/<name>/ARCUSlog/`에 남는다.
- 실행 인자는 instance.toml에서 만든다: `-E <default_engine.so>`, `-X <userlog_logger.so>`,
  `-p <port>`, `-m <mem>`, `-z <ensemble>`, SASL이면 `-S`. 정확한 옵션은 빌더가
  `docs/administration/commandline_args.md`로 확인한다.
- 감독 프로세스는 `waitpid`로 종료를 기다린다. 종료되면 `last_exit.json`(시각, 종료 코드, 시그널)을
  쓰고 끝난다. 다시 띄우지 않는다.
- `arcus_stop`은 서버에 SIGTERM을 보내고, 일정 시간 뒤에도 살아 있으면 알린다. SIGKILL은 `force: true`일 때만.
- `arcus_status`는 pid 생존 여부(남은 pid 파일이 다른 프로세스를 가리키는 경우 제외), 포트 응답(`version` 명령), 마지막 종료 정보를 돌려준다.

### 뒤 프로젝트를 위한 제약

- 감독 프로세스 구조는 2번에서 알림을 붙일 자리다. 종료 처리를 한 곳에 모아둔다.
- 도구 로직은 MCP 프로토콜 처리와 분리해, 3번(원격)에서 같은 로직을 원격 호출로 쓸 수 있게 한다.

## 제약

- 대상 OS: Linux(RHEL9 aarch64, Ubuntu x86_64), macOS(Intel·Apple Silicon). root 권한 없이 동작한다.
- 기존 코드는 동기식이다(tokio 없음). 새 의존성도 동기식으로 고른다(예: HTTP는 `ureq`, 압축은 `tar`+`flate2`, 체크섬은 `sha2`).
- Rust 2024 edition, 기존 모듈 구조와 스타일을 따른다.
- `src/zk/mod.rs`의 SCRAM 계산, digest ID, NoAuth 시 자격 증명 재시도를 재사용한다.

## 빌더 재량

- 도구 인자 이름의 세부, 도구 설명 문구
- MCP 프로토콜 버전 협상의 세부(클라이언트가 보낸 버전 중 지원하는 것을 고른다)
- instance.toml 스키마
- 기본 버전(ZooKeeper 서버 버전 포함), 기본 포트, 기본 메모리
- 테스트 방식. 단, znode 구성 로직과 configure 옵션 조합은 단위 테스트한다. 실제 빌드와 기동은 로컬 통합 테스트로 확인한다.

## 열린 질문

없음. 빌더 점검에서 나온 질문(EE 소스 위치, SASL 구성 방식, 복제 범위)은 위에 반영했다.
