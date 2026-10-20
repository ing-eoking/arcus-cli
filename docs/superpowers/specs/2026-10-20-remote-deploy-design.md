# 설계: 원격 배포 (여러 장비에 arcus-cli 상주시키고 클러스터 구성)

날짜: 2026-10-20
선행 설계: `2026-10-19-server-install-design.md` (1번 프로젝트, `arcus-cli --mcp`로 한 장비에 설치·실행)

## 배경과 목적

1번 프로젝트로 한 장비에서는 Claude(MCP)에게 말로 ARCUS를 설치·실행할 수 있게 됐다. 그러나 dev-1~3처럼
여러 장비에 클러스터를 올리려면 장비마다 들어가 arcus-cli를 설치하고 각각 시켜야 한다.

이 프로젝트는 맥의 Claude Code 한 곳에서 여러 장비를 다루게 한다.
1. 사용자가 고른 장비에 SSH로 접속해 arcus-cli 릴리즈 바이너리를 옮기고, 상주 프로세스로 띄운다.
2. 이후 모든 도구 호출을 SSH로 그 장비의 arcus-cli에 전달한다.
3. ZooKeeper 앙상블과 캐시 서버를 여러 장비에 걸쳐 구성한다.

상주 arcus-cli는 그 장비의 서버와 ZK를 자식으로 붙잡는 감독자이며, 2번 프로젝트(감시·알림)가 붙을 자리다.

## 목표

- MCP 도구로 원격 장비를 등록하고(접속 정보는 사용자에게 묻는다), 상주 arcus-cli를 설치·기동한다.
- 기존 도구(설치, ZK, setup, start/stop/status, memcached/zk 명령)를 `machine` 인자로 원격 장비에서 실행한다.
- 여러 장비에 걸친 ZK 앙상블을 띄운다(추천: 장비마다 1노드, 사용자가 자유롭게 바꿀 수 있음).
- 장비별 캐시 서버와 복제 그룹(예: master dev-1, slave dev-2)을 구성한다.

## 비목표

- 감시 알림(슬랙/Claude), 원인 분석 (2번)
- 자동 재시작 (서비스로 등록된 상주 arcus-cli는 재부팅 시 다시 뜨지만, 그 안의 서버·ZK는 자동으로 띄우지 않는다)
- 원격 장비에 빌드 도구·Java·gh를 대신 설치하기(1번과 같이 안내만 한다)
- SSH 이외의 통로(별도 포트를 여는 에이전트 프로토콜)
- Windows 원격 장비

## 반목표

- SSH 비밀번호, 키 passphrase, EE 토큰이 AI 대화·도구 결과·로그에 들어가기.
- 묻지 않고 scp/ssh로 원격 장비에 파일을 쓰거나 프로세스를 띄우기.
- 이미 떠 있는 상주 arcus-cli를 묻지 않고 교체·중지하기.
- 원격 장비에서 sudo를 묻지 않고 쓰기.

## 사용자가 정한 것

- 처음 설치도 맥에서 자동으로 한다. 사용자가 장비에 직접 들어가지 않는다.
- 옮기는 것은 GitHub 릴리즈의 완성 바이너리다(장비에서 빌드하지 않음). scp로 옮긴다.
- SSH 접속 정보는 사용자에게 묻는다. scp와 ssh 실행 전에 확인을 받는다.
- 비밀번호가 필요하면 맥에 SSH 비밀번호 입력 창을 띄운다(비밀번호는 AI를 거치지 않음). 화면이 없는 환경이면
  `ssh-copy-id`를 한 번 실행하라고 안내한다.
- 장비에 이미 상주 arcus-cli가 떠 있으면 버전을 보여주고 어떻게 할지 묻는다.
- 상주 방식(서비스 등록 / 데몬)을 사용자에게 묻는다.
- ZK는 장비마다 1노드 앙상블을 추천하되, 노드 수·배치·포트를 사용자가 원하는 대로 구성할 수 있고 기존 앙상블도 쓸 수 있다.
- 사용자가 정할 수 있는 값은 모두 도구 인자이고, 기본값은 추천값으로 제시해 확인받는다(1번과 같은 규칙).

## 설계

### 구성 요소

- **맥(로컬) MCP 서버** `arcus-cli --mcp`: 기존 그대로. 도구에 `machine` 인자가 있으면 그 장비로 전달한다.
- **원격 상주 에이전트** `arcus-cli --agent --home <dir>`: 장비마다 하나. `<home>/agent.sock`(Unix 소켓, 권한 600)으로
  같은 장비 안에서만 요청을 받는다. 서버·ZK 노드마다 1번의 감독 프로세스(`--supervise`)를 띄워 관리한다.
- **프로세스 구조 (사용자 결정)**: 에이전트 → 서버·ZK마다 감독 프로세스 → memcached / java(ZK).
  - 감독 프로세스가 자식을 `waitpid`로 기다려 종료 코드·시그널을 기록한다(1번과 같음).
  - 에이전트는 Linux에서 `PR_SET_CHILD_SUBREAPER`를 설정한다. 감독 프로세스가 죽으면 서버가 에이전트에 입양되고,
    에이전트가 `waitpid`로 종료 코드까지 기록한다. 서버는 내리지 않는다.
  - 에이전트가 재시작되면 감독 프로세스는 그대로 살아 있고(service 모드는 `KillMode=process`), 새 에이전트가
    pid 파일로 다시 찾아 관리를 이어간다.
  - 감독 프로세스와 에이전트가 모두 죽은 경우, 그리고 macOS(subreaper 없음)에서 감독 프로세스가 죽은 경우에는
    서버가 init/launchd로 넘어간다. 다시 뜬 에이전트가 Linux는 pidfd, macOS는 kqueue `EVFILT_PROC`/`NOTE_EXIT`로
    죽음을 감지하고 "종료됨, 종료 코드 모름"으로 기록한다(알려진 한계).
  - 어떤 경우에도 감독 프로세스나 에이전트 문제로 서버·ZK를 일부러 내리지 않는다.
- **원격 도구 실행** `arcus-cli --call <tool> --home <dir>`: SSH로 실행되는 일회성 프로세스. stdin으로 JSON 인자를
  받아 같은 도구 로직을 실행하고 결과(ToolResult JSON 한 줄)를 stdout으로 낸다. 서버·ZK 시작/중지는 에이전트
  소켓에 요청한다. 맥 MCP는 결과를 그대로 AI에게 돌려준다.

로컬 장비도 같은 구조를 쓸 수 있게 한다: 에이전트가 떠 있으면 `arcus_start` 등은 에이전트에 맡기고, 없으면
1번처럼 `--supervise` 프로세스를 띄운다. (내 결정: 기존 사용 방식을 깨지 않기 위해)

### 장비 등록과 SSH

- `~/.ssh/config`에 이미 그 장비의 접속 설정이 있으면 접속 정보를 묻지 않고 그 설정을 그대로 쓴다(사용자 결정).
  - `arcus_host_add {name}`만으로 등록할 수 있다. `name`이 `~/.ssh/config`의 `Host` 별칭(와일드카드 아닌 항목,
    `Include`된 파일 포함)과 같으면 ssh에 별칭을 그대로 넘기고, `ssh -G <name>`으로 해석된 호스트·사용자·포트를
    결과에 보여준다. hosts.toml에는 "ssh config 별칭"이라고만 기록한다.
  - `arcus_host_list_ssh_config {}`: `~/.ssh/config`의 별칭 목록을 돌려줘 AI가 장비 선택을 제안할 수 있게 한다.
- ssh config에 없으면 `arcus_host_add {name, host, user, port?: 22, identity_file?}`로 접속 정보를 받아
  `~/.arcus-cli/hosts.toml`에 저장한다. 비밀번호는 저장하지 않는다. 이름은 사용자에게 묻는다(예: dev-1).
- 공통 SSH 옵션(아래)은 ssh config 설정 위에 덧붙인다. 사용자의 `ProxyJump`, `IdentityFile` 등은 그대로 적용된다.
- 모든 SSH/scp는 시스템 `ssh`/`scp`를 실행한다. 공통 옵션:
  - `ControlMaster=auto`, `ControlPath=~/.arcus-cli/ssh/%C`, `ControlPersist=10m`: 장비마다 한 번 인증하면 재사용.
  - `BatchMode`는 쓰지 않고, 비밀번호가 필요하면 `SSH_ASKPASS=<arcus-cli askpass 헬퍼>`,
    `SSH_ASKPASS_REQUIRE=force`로 macOS 입력 창(`osascript`의 숨김 입력 대화상자)을 띄운다. 헬퍼는
    `arcus-cli --askpass <prompt>`로 구현해 입력값을 ssh에만 넘긴다.
  - 화면이 없으면(`DISPLAY`·macOS GUI 세션 없음) 창을 띄우지 않고, 키 인증 실패 시 `ssh-copy-id user@host`를
    한 번 실행하라는 오류를 돌려준다.
  - 호스트 키를 처음 보면 지문을 결과에 보여주고 사용자 확인 후 `accept_host_key: true`로 다시 부르게 한다
    (`StrictHostKeyChecking=accept-new`는 그 확인 뒤에만 쓴다).
- `arcus_host_check {name}`: 접속, `uname -sm`, 원격 홈, 이미 설치된 arcus-cli 버전과 에이전트 실행 여부를 돌려준다.

### 상주 arcus-cli 설치 (`arcus_host_install`)

인자: `name`, `mode: "service" | "daemon"` (추천: service), `replace?: false`, `apply?: false`.
1. `arcus_host_check`로 OS/CPU와 기존 설치를 확인한다. 이미 에이전트가 떠 있고 `replace`가 아니면 버전과 함께
   멈추고 사용자에게 물으라고 돌려준다.
2. 맞는 릴리즈 바이너리를 맥에서 받는다: `ing-eoking/arcus-cli` 릴리즈의
   `arcus-cli-<tag>-<target>.tar.gz`와 `.sha256`(release.yml이 만드는 형식), 체크섬 검증,
   `~/.arcus-cli/dist/<tag>/<target>/arcus-cli`에 캐시. 대상 버전 기본값은 맥에서 실행 중인 arcus-cli와 같은 태그
   (없으면 최신 릴리즈).
3. `apply` 없이는 할 일 목록(옮길 파일, 원격 경로, 상주 방식, 실행할 명령)만 돌려준다.
4. `apply: true`면 scp로 `~/.arcus-cli/bin/arcus-cli`에 옮기고, 상주 방식대로 띄운다:
   - service, Linux: `~/.config/systemd/user/arcus-cli-agent.service`(사용자 유닛)를 쓰고
     `systemctl --user enable --now`. 유닛에 `KillMode=process`를 넣어 에이전트를 내려도 감독 프로세스·서버가 남게 한다.
     재부팅 후·로그아웃 후에도 살아 있으려면 linger가 필요하다: `loginctl show-user $USER -p Linger`가 no이면
     `loginctl enable-linger`를 시도한다. 권한이 없으면(SSH 세션에서 흔함) 설치를 멈추고, linger 없이 서비스로
     두면 접속을 끊은 뒤 꺼진다는 사실과 함께 "관리자에게 linger 요청" 또는 "daemon 모드로 설치" 중 고르라고
     돌려준다(사용자 결정). 직접 sudo 하지 않는다.
   - service, macOS 원격: `~/Library/LaunchAgents/com.arcus-cli.agent.plist` + `launchctl bootstrap`.
   - daemon: `nohup ~/.arcus-cli/bin/arcus-cli --agent --home ~/.arcus-cli >agent.log 2>&1 &`
     (`setsid`로 SSH 세션과 분리).
   - 교체(`replace: true`)면 기존 에이전트를 멈추기 전에, 그 에이전트가 관리하는 서버·ZK 목록을 보여주고
     계속할지 묻는다. 감독 프로세스가 서버·ZK를 붙잡고 있으므로 에이전트만 교체되고 서버·ZK는 계속 돈다.
5. 끝나면 `arcus_host_check`로 에이전트가 떴는지 확인한다.

### 원격 도구 실행

- 기존 도구에 `machine?: <장비 이름>` 인자를 추가한다. 없으면 지금처럼 로컬에서 실행한다.
- 원격이면 맥 MCP가 `ssh <장비> ~/.arcus-cli/bin/arcus-cli --call <tool> --home ~/.arcus-cli`를 실행하고 JSON 인자를
  stdin으로 넘긴다. 결과 JSON을 받아 장비 이름을 붙여 돌려준다.
- 오래 걸리는 작업(빌드, ZK 다운로드, EE 로그인)은 원격 장비의 job으로 돌고, `arcus_job_status {machine, id}`로 본다.
- EE 로그인(`arcus_ee_login {machine}`)은 원격 장비의 gh가 일회용 코드를 내고, 승인은 맥 브라우저에서 한다.
  키체인이 없는 원격 장비에서는 gh가 토큰을 `~/.config/gh/hosts.yml`(권한 600)에 평문 저장한다. 그래서 원격 로그인은
  이 사실을 먼저 돌려주고, `accept_plaintext_token: true`로 다시 불렀을 때만 진행한다(사용자 결정).

### 확인이 필요한 범위 (사용자 결정)

- plan/apply 두 번 호출은 상주 arcus-cli 설치·교체(`arcus_host_install`)와 처음 보는 호스트 키 수락에만 둔다.
- 장비 접속 정보(`arcus_host_add`)는 AI가 사용자에게 물어 넘긴다.
- 그 밖의 원격 호출(`arcus_install`, `arcus_zk_start`, `arcus_setup`, `arcus_start` 등)은 로컬과 같은 규칙을 따른다:
  추천값을 제시해 확인받고 호출하며, 기존 apply/overwrite/force 게이트는 그대로다.
- 장비 선택 인자 이름은 `machine`으로 한다(`memcached_command`에 이미 서버 주소용 `host`가 있음).

### 여러 장비 ZK 앙상블 (`arcus_cluster_zk_plan` / `arcus_cluster_zk_start`)

- 인자: `nodes: [{machine, client_port, peer_port, election_port, address?}]`(추천: 등록된 각 장비에 1노드씩, 포트
  2181/2888/3888), `version?`.
- `address`는 다른 노드가 이 노드에 붙을 주소다. 추천값은 그 장비의 사설 IP(`hostname -I`의 첫 값)이고 사용자가 바꿀 수 있다.
- ZK 코드는 지금 `127.0.0.1`과 홈당 앙상블 하나를 가정한다. 노드별 bind/peer 주소를 받도록 일반화하고,
  이미 로컬 앙상블이 떠 있는 장비는 그 사실을 알리고 멈춘다.
- 각 장비에 `zoo.cfg`(같은 `server.N=<address>:<peer>:<election>` 목록, 자기 `myid`)를 쓰고, 각 장비의 에이전트가
  노드를 띄운다. 모든 노드가 서비스 상태(`srvr` Mode)가 될 때까지 기다린다.
- 실패하면 어떤 노드가 어떤 이유로 실패했는지(로그 끝부분, 연결 거부 등) 장비별로 돌려준다. 보안 그룹 등으로
  peer 포트가 막혀 quorum이 안 생기는 경우를 구분해 알린다.
- `_plan`은 쓰기 없이 구성안만 보여준다(추천값 포함). `_start`는 `apply`처럼 확인된 구성으로만 실행한다.
- 결과로 앙상블 주소(`addr1:port,addr2:port,...`)를 돌려준다. 이후 `arcus_setup {machine, zk: <이 주소>}`에 쓴다.
- 기존 앙상블을 쓰면 이 단계를 건너뛰고 주소만 넘긴다.

### 여러 장비 캐시 서버

- 장비마다 `arcus_install {machine,...}` → `arcus_setup {machine, ...}` → `arcus_start {machine, name}` 순서다.
  같은 빌드가 여러 장비에 필요하면 장비마다 빌드한다(빌드 산출물 복사는 하지 않음, 내 결정).
- 복제 그룹: 같은 서비스 코드·그룹 이름으로 장비별 인스턴스를 setup하면 된다(1번의 "인스턴스 하나 = 노드 하나").
  `repl_listen` 추천값은 그 장비 주소 + `port+9000`.
- `arcus_cluster_status {names?}`: 등록된 장비들에서 `arcus_status`를 모아 한 번에 보여준다(장비 병렬 조회).

## 제약

- 1번의 전역 제약을 그대로 따른다(동기식, stdout은 JSON-RPC 전용, 비밀은 결과·로그에 없음, structuredContent는 객체).
- 원격 장비 대상: Linux(RHEL9 aarch64, Ubuntu x86_64)를 우선 지원한다. macOS 원격은 가능하면 지원한다.
- 맥(로컬)에 `ssh`/`scp`가 있어야 한다(macOS 기본 포함).
- 바이너리가 자기 릴리즈 태그를 알아야 한다: release.yml 빌드 때 `GITHUB_REF_NAME`을 컴파일 시점 상수로 넣고
  (`ARCUS_CLI_TAG`), 로컬 빌드는 `dev`로 둔다. `--call` 전에 맥과 원격의 태그를 비교해 다르면 알린다.
- 릴리즈 파일 형식: `arcus-cli-<tag>-<target>.tar.gz`(안에 `arcus-cli-<tag>-<target>/arcus-cli`)와 `.sha256`.
  target은 `uname -sm`으로 고른다(Linux x86_64/aarch64 → `*-unknown-linux-gnu`, Darwin → `*-apple-darwin`).
- 원격 바이너리는 릴리즈 태그가 있어야 받는다. 이 기능을 쓰려면 MCP 기능이 들어간 커밋에 태그를 붙여 릴리즈해야
  한다(현재는 `feat/server-install` 미병합). 개발 중에는 `--binary <로컬 경로>`로 직접 만든 Linux 바이너리를 옮길 수 있게 한다. (내 결정)
- Linux 바이너리는 glibc 동적 링크(ubuntu-22.04 빌드)라 RHEL9·Ubuntu 22.04 이상에서 돈다.

## 빌더 재량

- 에이전트 소켓 프로토콜(줄 단위 JSON 권장)과 에이전트 내부 구조(스레드 / waitpid 처리)
- 도구 이름의 세부, `--call` 출력 형식의 세부
- askpass 대화상자 문구
- 병렬 조회 방식

## 열린 질문

없음. 빌더 점검 질문(프로세스 구조, linger, 확인 범위, 원격 EE 토큰)은 위에 반영했다.
