# 설계: 서버 한 대 설치·실행 (`arcus-cli server`)

날짜: 2026-10-19

## 배경과 목적

dev-1~3에 ARCUS 클러스터를 올리면서 ZooKeeper 설치·실행, arcus-memcached(공개판·EE)
빌드·실행, 서버용 znode 구성을 전부 손으로 했다. 실수하기 쉽고(예: `/arcus_acl_mapping/<svc>`
값이 비어 `zoo_get_children(/arcus_acl/) failed: bad arguments`로 기동 실패), 실수의 원인도
잘 드러나지 않는다.

arcus-cli가 **서버 한 대 안에서** 이 과정을 대신한다. 사용자는 켤 기능을 고르고, arcus-cli가
소스를 받아 빌드하고, ZooKeeper와 서버를 띄우고, 필요한 znode를 만든다.

이 문서는 전체 계획 중 1번 프로젝트다.

1. **서버 한 대 설치·실행 (이 문서)**
2. 감시·알림: 서버를 자식으로 띄운 arcus-cli가 종료를 감지하고, ZK watch와 함께 슬랙/Claude로
   알림과 userlog 기반 원인 분석을 보낸다. 자동 재시작은 하지 않는다(사람이 판단).
3. 원격 배포: 사용자 확인 후 scp로 arcus-cli를 옮기고 ssh로 실행한다.
4. MCP: 위 기능을 Claude가 쓰도록 노출한다.

1번은 2~4번의 기반이므로 아래 "뒤 프로젝트를 위한 제약"을 지켜야 한다.

## 목표

- 대화형 마법사(`arcus-cli server install`)로 에디션, 버전, 기능을 고르면 빌드까지 끝난다.
- 같은 선택을 플래그로도 줄 수 있다(비대화형).
- 공개판은 GitHub 릴리즈 소스를, EE는 비공개 GitHub 저장소를 받는다. EE 접근이 실패하면
  계정을 물어보고 다시 시도한다.
- 빌드에 필요한 시스템 패키지가 없으면 무엇을 설치해야 하는지 알려주고 멈춘다.
- 로컬 ZooKeeper를 받아 띄우거나, 이미 있는 ZK 앙상블 주소를 받아 쓴다.
- 고른 기능에 맞는 znode를 만들고, 만든 뒤 다시 읽어 확인한다.
- 서버를 arcus-cli의 **자식 프로세스**로 띄우고, 로그는 인스턴스 폴더의 userlog에 남긴다.
- `status`, `stop`으로 상태 확인과 중지를 한다.

## 비목표

- 여러 서버 동시 설치, scp/ssh (3번)
- 감시 알림(슬랙/Claude), 원인 분석 (2번). 단, 종료 정보 기록까지는 1번에 포함한다.
- MCP (4번)
- 자동 재시작, systemd 유닛 생성
- znode 삭제, 클러스터 해체, 언인스톨(로컬 인스턴스 폴더 삭제 외)
- Windows
- 여러 노드로 구성된 ZK 앙상블을 직접 띄우기 (로컬 단일 노드만 띄움. 앙상블은 기존 것을 주소로 사용)

## 반목표 (돌아가도 실패로 보는 것)

- 이미 있는 znode의 값을 묻지 않고 덮어쓰기. 공용 클러스터를 망가뜨릴 수 있다.
- EE 접근 토큰을 사용자 동의 없이 디스크에 평문 저장하거나, 로그·화면에 노출하기.
- 묻지 않고 sudo를 쓰거나 시스템 경로(`/usr/local` 등)에 설치하기.
- 빌드 실패 원인을 숨기기(로그 위치와 마지막 오류 줄을 반드시 보여준다).

## 사용자가 정한 것

- 실행 방식: 각 서버에 arcus-cli를 두고 그 서버에서 실행한다. SSH 없이 완결되어야 한다.
- 서버 실행: arcus-cli가 **자식 프로세스로** 띄운다(2번에서 `waitpid`로 종료 코드·시그널을 얻기 위함).
- 로그: userlog 확장(`userlog_logger`)을 쓰고, arcus-cli가 관리하는 폴더에 남긴다.
- 기능 선택: **ZK 연동, SASL, 복제**를 기본 추천으로 먼저 보여준다. 나머지(persistence,
  sticky item, migration, 엔진 선택)는 사용자가 원할 때만 펼쳐 보여준다.
- EE 소스를 못 받으면 계정을 물어보고, 빌드까지 실행해 준다.

## 설계

### 명령 구성

```
arcus-cli server install          # 마법사: 에디션/버전/기능 선택 → 소스 → 빌드
arcus-cli server zk start|stop    # 로컬 ZooKeeper (단일 노드)
arcus-cli server setup <name>     # 인스턴스 생성: 포트·메모리·ZK 주소·서비스 코드 → znode 구성
arcus-cli server start <name>     # 서버 기동 (arcus-cli 감독 프로세스가 자식으로 실행)
arcus-cli server stop <name>
arcus-cli server status [<name>]
```

기존 사용법(`arcus-cli --host ...`, `--zookeeper`)은 그대로 둔다. `server` 하위 명령이
있을 때만 새 기능으로 들어간다. 명령 이름과 단계 분할은 빌더 재량이다. 다만 마법사 한
번으로 install→setup→start까지 이어서 진행할 수 있어야 한다.

### 작업 폴더

기본 위치는 `~/.arcus-cli/`이고 `--home`으로 바꿀 수 있다. (내 결정)

```
~/.arcus-cli/
  downloads/                       받은 tar 캐시 (체크섬 검증 후 보관)
  builds/<edition>-<version>-<features>/   빌드 산출물 설치 경로(prefix): deps + memcached
  zookeeper/<version>/             압축 푼 Apache ZooKeeper
  zk/                              로컬 ZK 설정(zoo.cfg)과 데이터, 로그
  instances/<name>/
    instance.toml                  선택 내용 (에디션, 버전, 기능, 포트, 메모리, ZK 주소, 서비스 코드…)
    ARCUSlog/                      userlog (서버 작업 디렉터리를 이 폴더로 지정해서 생김)
    supervisor.pid, server.pid
    last_exit.json                 마지막 종료 정보 (시각, 종료 코드, 시그널)
    build.log, run.log             빌드/표준출력 로그
```

같은 에디션·버전·기능 조합의 빌드는 재사용한다.

### 기능 선택과 빌드 옵션

마법사 첫 화면은 추천 3가지만 보여준다.

| 기능 | configure | 비고 |
|---|---|---|
| ZK 연동 | `--enable-zk-integration --with-zookeeper=<prefix>` | 기본 켬 |
| SASL | `--enable-sasl` | 켜면 ACL znode 구성 단계가 추가됨 |
| 복제 | `--enable-replication` | EE에서만 보임. 켜면 그룹 구성 단계가 추가됨 |

"고급 옵션 보기"를 고르면 다음을 추가로 보여준다.

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
  1. 이미 있는 자격 증명(`GITHUB_TOKEN`, `gh auth token`) 순으로 시도한다.
  2. 없거나 401/403/404이면 GitHub 사용자명과 개인 접근 토큰(PAT, 해당 저장소 읽기 권한)을 묻는다.
     토큰 입력은 화면에 표시하지 않는다. GitHub는 비밀번호 인증을 받지 않으므로 PAT가 필요하다고 안내한다.
  3. 비공개 저장소의 `releases/download/...` 주소는 토큰으로 바로 받을 수 없다. 대신 API로 받는다.
     `GET /repos/jam2in/arcus-memcached-EE/releases/tags/<tag>`로 asset id를 얻은 뒤,
     `GET /repos/jam2in/arcus-memcached-EE/releases/assets/<id>`를 `Accept: application/octet-stream`으로 요청한다.
  4. 받은 토큰을 저장할지는 묻고, 동의할 때만 저장한다(권한 600 파일).
- 받은 파일은 `downloads/`에 두고, 다시 쓸 때는 받지 않는다.

### 빌드

1. 사전 점검: gcc(또는 cc), make, libtool, autoconf/automake(git 소스일 때), perl 등.
   빠진 게 있으면 OS에 맞는 설치 명령(yum/dnf, apt, brew)을 보여주고 멈춘다. 직접 설치하지 않는다.
2. 의존성: 소스에 포함된 `deps/install.sh <build prefix>`로 libevent, arcus-zookeeper C 클라이언트
   (3.5.9-p4), cyrus-sasl을 prefix에 설치한다. 이 스크립트는 기능과 상관없이 셋 다 빌드하며,
   로그를 `deps/install.log`에 쓰므로 그 내용을 `build.log`에 합친다.
3. git 소스면 `config/autorun.sh`, 그다음 `configure <옵션>`, `make`, `make install`.
4. 모든 출력은 `build.log`로 보낸다. 화면에는 단계별 진행만 보여준다. 실패하면 로그 경로와 마지막 오류 줄을 보여준다.

### ZooKeeper

- **로컬로 띄우기**: Apache ZooKeeper 바이너리 tar를 받아 SHA-512를 검증하고 `zookeeper/<version>/`에 푼다.
  `zk/zoo.cfg`(clientPort 기본 2181, dataDir은 `zk/data`)를 만들고, 자식 프로세스로 실행한다.
  Java가 없으면 설치 방법을 안내하고, 원하면 JRE tar를 받아 그걸로 실행한다(받을지 먼저 묻는다).
- **기존 앙상블 쓰기**: `host:port,...`를 받아 연결만 확인한다.

### znode 구성

ZK 연동을 켰을 때 `setup`이 만든다. 기존 `zookeeper` crate와 ZK 모드의 SCRAM 비밀번호 함수를 재사용한다.

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

이름은 사용자에게 묻는다 (사용자 결정): 서비스 코드, ACL 그룹 이름, 복제 그룹 이름 모두
마법사가 묻고, 플래그로도 줄 수 있다. 임의로 정하지 않는다.

복제 범위 (사용자 결정): 인스턴스 하나가 노드 하나다. `setup`은 이 노드를 사용자가 정한 복제
그룹에 등록만 한다. 짝 노드는 다른 서버나 같은 서버에서 `setup`을 한 번 더 실행해 붙인다.

규칙:
- 없는 znode만 만든다. 이미 있으면 값을 비교하고, 다르면 차이를 보여주고 묻는다. 기본 답은 "그대로 둠"이다.
- 다 만든 뒤 다시 읽어 확인한다. 빈 매핑 값이나 매핑 개수가 1이 아닌 경우를 오류로 알린다.
- 실제로 쓰기 전에 만들 목록을 보여주고 확인을 받는다. `--dry-run`이면 목록만 보여준다.

위 경로는 `arcus_zk.c`(공개판·EE)와 `sasl_auxprop.c`에서 확인했다. 그러나 부모 노드가 미리
있어야 하는지, cache_list znode의 값, 권한 값의 형식은 빌더가 코드와 기존 ARCUS 스크립트로
최종 확인한다.

### 서버 실행

- `start`는 백그라운드 감독 프로세스(`arcus-cli server supervise <name>`, 내부용)를 띄우고 바로 돌아온다.
  감독 프로세스가 memcached를 **자식**으로 실행한다.
- 자식의 작업 디렉터리는 `instances/<name>/`이다. 그래서 userlog가 `instances/<name>/ARCUSlog/`에 남는다.
- 실행 인자는 instance.toml에서 만든다: `-E <default_engine.so>`, `-X <userlog_logger.so>`,
  `-p <port>`, `-m <mem>`, `-z <ensemble>`, SASL이면 `-S`. 정확한 옵션은 빌더가
  `docs/administration/commandline_args.md`로 확인한다.
- 감독 프로세스는 `waitpid`로 종료를 기다린다. 종료되면 `last_exit.json`(시각, 종료 코드, 시그널)을
  쓰고 끝난다. 다시 띄우지 않는다.
- `stop`은 서버에 SIGTERM을 보내고, 일정 시간 뒤에도 살아 있으면 알린다. SIGKILL은 묻고 보낸다.
- `status`는 pid 생존 여부, 포트 응답(`version` 명령), 마지막 종료 정보를 보여준다.

### 뒤 프로젝트를 위한 제약

- 마법사의 모든 질문은 같은 의미의 플래그가 있어야 한다. 3번(원격)과 4번(MCP)은 비대화형으로 실행한다.
- 감독 프로세스 구조는 2번에서 알림을 붙일 자리다. 종료 처리를 한 곳에 모아둔다.
- 사람이 읽는 출력과 별도로 `--json` 출력을 둔다(`status`부터). 4번에서 쓴다.

## 제약

- 대상 OS: Linux(RHEL9 aarch64, Ubuntu x86_64), macOS(Intel·Apple Silicon). root 권한 없이 동작한다.
- 기존 코드는 동기식이다(tokio 없음). 새 의존성도 동기식으로 고른다(예: HTTP는 `ureq`, 압축은 `tar`+`flate2`, 체크섬은 `sha2`).
- Rust 2024 edition, 기존 모듈 구조와 스타일을 따른다.
- `src/zk/mod.rs`의 SCRAM 계산, digest ID, NoAuth 시 자격 증명 재시도를 재사용한다.

## 빌더 재량

- 마법사 UI 라이브러리, 진행 표시 방식
- 명령 이름의 세부, 단계 분할
- instance.toml 스키마
- 기본 버전(ZooKeeper 서버 버전 포함), 기본 포트, 기본 메모리
- 테스트 방식. 단, znode 구성 로직과 configure 옵션 조합은 단위 테스트한다. 실제 빌드와 기동은 로컬 통합 테스트로 확인한다.

## 열린 질문

없음. 빌더 점검에서 나온 질문(EE 소스 위치, SASL 구성 방식, 복제 범위)은 위에 반영했다.
