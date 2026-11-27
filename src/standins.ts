// Stand-in screens, so that the page can be worked on and looked at in a browser. Nothing
// is patched. Which one shows is chosen by the address: `http://localhost:1420/#paused`.

import {
  defaultSettings,
  type Answer,
  type AppView,
  type Checking,
  type Diagnosis,
  type Refusal,
  type Row,
  type Run,
  type ServerId,
  type SiteView,
} from "./types";

/** False when the page is opened in a plain browser (`yarn dev`) instead of the app. */
export const inApp = "__TAURI_INTERNALS__" in window;

const folder: Row = { kind: "folder", label: "설치 경로", ok: true, detail: "C:\\Nexon\\Maple", message: "" };
const space: Row = { kind: "space", label: "여유 공간", ok: true, detail: "160.0 GB", message: "" };
const patchFile: Row = {
  kind: "patchFile",
  label: "패치 파일",
  ok: true,
  detail: "00421to00422.patch · 1,229 MB",
  message: "",
};
/** Where the live game's patch to 1.2.422 is published, and the test world's to 1.2.208. */
const patchAt = "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch";
const testPatchAt = "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch";
const noPatchFile: Row = { ...patchFile, ok: null, detail: "" };
/** The game is open: the line asks for it to be closed, and a dialog asks the same. */
const gameOpen: Row = {
  kind: "running",
  label: "메이플스토리를 종료한 후 실행해 주세요.",
  ok: false,
  detail: "",
  message: "메이플스토리를 종료한 후 실행해 주세요.",
};
const notRestored: Row = {
  kind: "recovery",
  label: "패치 복구",
  ok: false,
  detail: "실패",
  message: "이전 패치를 복구하지 못했습니다.",
};
const journalUnreadable: Row = {
  kind: "recovery",
  label: "패치 복구",
  ok: false,
  detail: "작업 기록 손상",
  message: "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.",
};
const leftover: Row = { kind: "leftover", label: "이전 패치 작업", ok: null, detail: "12.3 GB", message: "" };
const notWritable: Row = {
  kind: "writable",
  label: "쓰기 권한",
  ok: false,
  detail: "파일 쓰기 불가",
  message: "설치 경로에 파일을 쓸 수 없습니다.",
};
/** The line a client's page has of a PC that is no ordinary one: here a seat in a PC room. */
const atSeat: Row = { kind: "diagnosis", label: "PC 검사", ok: null, detail: "노하드 PC방 좌석", message: "" };
/**
 * The report of an ordinary PC after the check the app has made by itself, as the diagnostic's
 * library writes it for the app: the text of one of its own samples, letter for letter. The
 * line to Nexon's server is timed by that check, and the report says how fast it is.
 */
const homeReport = String.raw`Maple Fast Patch 0.1.0 · 2026.11.27 01:36

[검사 요약]
전체 검사 결과    정상
패치 가능 여부    가능
패치 공간         충분 (D: 여유 200.0 GB · 필요 36.5 GB)
패치 유지 여부    가능

[PC 환경]
사용 장소         일반 PC
디스크 유형       PC 내장 디스크
게임 저장 위치    이 PC의 디스크
운영체제          Windows 11 Pro (26200) · 64비트
CPU               Intel Core i7-11800H · 8코어 16스레드
메모리            15.8 GB
실행 권한         일반 사용자 · UAC 활성화
WebView2          158.0.4370.33

[디스크 환경]
C: 시스템         Micron MTFDKBA512TFH · NVMe · NTFS
                  여유 79.4 / 475.6 GB
D: 게임           Samsung SSD 980 1TB · NVMe · NTFS
                  여유 200.0 / 931.5 GB
디스크 구성       C:, D: 각각 별도 디스크

[게임 설치 상태]
게임 이름         메이플스토리
설치 경로         D:\Games\Maple
현재 게임 버전    Ver 1.2.421 마이너버전(4)
최신 게임 버전    Ver 1.2.421 마이너버전(4)
설치 용량         63.4 GB
전체 파일 개수    1,225개

[게임 파일 검사]
검사 기준         파일 크기 비교
검사 대상 버전    Ver 1.2.421 마이너버전(4)
정상 파일         1,212개
변경 파일         없음
누락 파일         없음
검사 제외 파일    13개

[네트워크 환경]
연결 방식         무선 Wi-Fi
네트워크 속도     2.4 Gbps
다운로드 속도     42 MB/s
응답 지연 시간    8.3 ms

[성능 측정]
디스크 읽기 속도  2,150 MB/s
반복 읽기 속도    2,230 MB/s

[패치 환경 검사]
설치 폴더 쓰기    성공
임시 파일 삭제    성공
잔여 파일         없음

[환경 분석]
PC 유형 근거      이 PC의 NVMe 디스크에 설치
패치 판정 근거    D: 여유 200.0 GB · 설치 폴더 쓰기 성공
유지 판정 근거    노하드·복구 프로그램 없음

[검사 기록]
검사 소요 시간    0.4초
PC 정보 전송      없음`;
/**
 * The report of a seat in a PC room that reads its game over the network, with every step
 * the diagnostic has run, the ones that write included: the text of another of the library's
 * samples, under the first line it writes for the app.
 */
const seatReport = String.raw`Maple Fast Patch 0.1.0 · 2026.11.26 23:49

[검사 요약]
전체 검사 결과    주의
패치 가능 여부    가능
패치 공간         충분 (G: 여유 305.2 GB · 필요 36.5 GB)
패치 유지 여부    불가능 (재부팅·로그아웃 시 초기화)
자리 이동         재패치 필요

[PC 환경]
사용 장소         PC방
디스크 유형       노하드 (서버 디스크 방식)
게임 저장 위치    매장 서버 (네트워크)
운영체제          Windows 10 Pro (19045) · 64비트
CPU               Intel Core i5-12400F · 6코어 12스레드
메모리            31.8 GB
실행 권한         관리자 · UAC 비활성화
WebView2          158.0.4370.33

[디스크 환경]
C: 시스템         NSM 22H2-H610 232GB · Fibre Channel · NTFS
                  여유 154.0 / 200.0 GB
G: 게임           NSM Logical_G 3722GB · Fibre Channel · NTFS
                  여유 305.2 / 3,722.7 GB
D: 기타           NSM 22H2-H610 232GB · Fibre Channel · NTFS
                  여유 32.8 / 32.9 GB
디스크 구성       C:, D: 같은 디스크 · G: 별도 디스크

[게임 설치 상태]
게임 이름         메이플스토리
설치 경로         G:\Gamez\Maple
현재 게임 버전    Ver 1.2.421 마이너버전(4)
최신 게임 버전    Ver 1.2.421 마이너버전(4)
설치 용량         63.4 GB
전체 파일 개수    1,225개

[게임 파일 검사]
검사 기준         파일 내용 비교 (전체)
검사 대상 버전    Ver 1.2.421 마이너버전(4)
정상 파일         1,212개
변경 파일         없음
누락 파일         없음
검사 제외 파일    13개
검사 시간         9분 46초 · 63.3 GB

[네트워크 환경]
연결 방식         유선 LAN
네트워크 속도     1 Gbps
다운로드 속도     37 MB/s
응답 지연 시간    4.7 ms

[성능 측정]
디스크 읽기 속도  106 MB/s
반복 읽기 속도    104 MB/s

[패치 환경 검사]
설치 폴더 쓰기    성공
임시 파일 삭제    성공
잔여 파일         없음
G: 쓰기 검사      1.0 GB · 199 MB/s (처음 1,799 → 끝 105)
C: 쓰기 검사      1.0 GB · 114 MB/s
D: 쓰기 검사      1.0 GB · 112 MB/s
한도 검사         G: 4.0 GB · 138 MB/s (처음 1,291 → 끝 107)
                  4.0 GB에서 중단 · 속도 급감
메모리 변화       쓰기 중 최대 183 MB 감소

[환경 분석]
PC 유형 근거      Fibre Channel 디스크 · NSM 디스크 · PicaigreenClient.exe,
                  PicaVncSvc.exe, pmclient.exe · iScsiPrt
읽기 속도 분석    네트워크 전송 속도와 유사한 읽기 패턴
패치 판정 근거    쓰기 후반 속도가 네트워크 전송 속도와 유사
                  쓰기 중 메모리 162 MB 감소 · 한도 검사 4.0 GB에서 중단
유지 판정 근거    노하드 좌석은 재부팅·로그아웃 시 변경 내용 초기화

[검사 기록]
검사 소요 시간    12분 51초 (기본 검사 4.7초)
G: 설치 폴더      임시 파일 1.0 GB / 4.0 GB 생성 후 삭제
C: 임시 폴더      mfp-check-tmp에 1.0 GB 생성 후 삭제
D: 임시 폴더      mfp-check-tmp에 1.0 GB 생성 후 삭제
PC 정보 전송      없음`;
/**
 * What the page is told of a diagnosis that came to `report`, with the three answers the
 * library gives for it. Those are in everyday words of the library's own, which the report
 * does not have. The report is the library's as it is: its first line names the app and its
 * version, and the backend puts nothing before it.
 */
const diagnosed = (report: string, answers: Answer[]): Diagnosis => ({
  working: false,
  checking: false,
  canCheck: true,
  answers,
  report,
});
const atHome = diagnosed(homeReport, [
  { title: "클라이언트 패치", answer: "가능", reason: "여유 공간 충분(200 GB)", tone: "good" },
  { title: "PC 종류", answer: "일반 PC", reason: "게임이 이 PC의 디스크에 설치됨", tone: "" },
  { title: "재부팅 후", answer: "패치 유지", reason: "이 PC의 디스크에 저장됨", tone: "good" },
]);
/** Every file of the game was read for the seat's report: no check of them is left to offer. */
const seatDiagnosis: Diagnosis = {
  ...diagnosed(seatReport, [
    { title: "클라이언트 패치", answer: "가능", reason: "매장 서버 공간이 부족하면 중단", tone: "caution" },
    {
      title: "PC 종류",
      answer: "노하드 PC방 좌석",
      reason: "게임 디스크를 매장 서버에서 네트워크로 받아 사용",
      tone: "",
    },
    { title: "재부팅 후", answer: "패치 초기화", reason: "재부팅·자리 이동 시 다시 패치 필요", tone: "caution" },
  ]),
  canCheck: false,
};
const building: Run = {
  asking: false,
  step: "rebuild",
  done: 6_442_450_944,
  total: 15_032_385_536,
  worked: 6_442_450_944,
  activity: "생성 중: Map001.wz",
  failure: null,
};
/** A game folder with the patch to 1.2.422 on the server and nothing in the way. */
const withPatch: SiteView = {
  server: "live",
  rev: 1,
  title: "메이플스토리",
  current: "Ver 1.2.421",
  latest: "Ver 1.2.422",
  patchAddress: patchAt,
  checkedAt: Date.now(),
  lamp: { open: true, extended: false, until: null, season: true },
  rows: [folder, space, patchFile],
  customDir: false,
  halfPatched: false,
  discardAsks: false,
  phase: "ready",
  run: null,
  percent: 0,
  status: "새 클라이언트 패치 있음",
  statusBad: false,
  busy: false,
  summary: null,
  button: { kind: "download", enabled: true, primary: true },
  locked: null,
  diagnosis: atHome,
};
/** The game at a seat in a PC room, up to date. Its page says in a line what kind of PC this is. */
const seated: Partial<SiteView> = {
  current: "Ver 1.2.421 마이너버전(4)",
  latest: "Ver 1.2.421 마이너버전(4)",
  rows: [{ ...folder, detail: "G:\\Gamez\\Maple" }, { ...space, detail: "305.2 GB" }, noPatchFile, atSeat],
  percent: 100,
  status: "최신 버전",
  button: { kind: "launch", enabled: true, primary: true },
  diagnosis: seatDiagnosis,
};
const scripted: Record<string, Partial<SiteView>> = {
  patch: {},
  seat: seated,
  // The game's files are being checked at the seat. How far that is, `browserChecking` says.
  filecheck: { ...seated, diagnosis: { ...seatDiagnosis, checking: true } },
  // The first check of the PC is at work: nothing has been found to show yet.
  diagnosing: { diagnosis: { working: true, checking: false, canCheck: false, answers: [], report: "" } },
  // A minor patch within the version that is installed.
  minor: {
    current: "Ver 1.2.421 마이너버전(3)",
    latest: "Ver 1.2.421 마이너버전(4)",
    patchAddress: "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch",
    rows: [folder, space, { ...patchFile, detail: "03to04.patch · 187 MB" }],
    status: "새 마이너 패치 있음",
  },
  checking: {
    latest: null,
    checkedAt: null,
    rows: [folder, space, noPatchFile],
    status: "최신 버전 확인 중",
    busy: true,
    button: { kind: "download", enabled: false, primary: false },
  },
  uptodate: {
    latest: "Ver 1.2.421",
    rows: [folder, space, noPatchFile],
    percent: 100,
    status: "최신 버전",
    button: { kind: "launch", enabled: true, primary: true },
  },
  outdated: { status: "최신 버전으로 업데이트해 주세요." },
  picked: { customDir: true, rows: [{ ...folder, detail: "D:\\Games\\MapleStory" }, space, patchFile] },
  maintenance: { lamp: { open: false, extended: false, until: "10:00", season: true } },
  extended: { lamp: { open: false, extended: true, until: "12:00", season: true } },
  blocked: {
    rows: [folder, gameOpen, space, patchFile],
    status: "패치 불가",
    statusBad: true,
    button: { kind: "download", enabled: true, primary: false },
  },
  // The patch file is on the PC, and a run of it needs more room than the drive has by the
  // way of patching that is chosen.
  noroom: {
    rows: [
      folder,
      {
        ...space,
        ok: false,
        detail: "C: 필요 34.6 GB · 여유 15.8 GB",
        message: "디스크 공간이 부족합니다.\nC: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)",
      },
      leftover,
      patchFile,
    ],
    percent: 12,
    status: "패치 불가",
    statusBad: true,
    button: { kind: "resume", enabled: true, primary: false },
  },
  // 즉시 패치 is chosen on a drive that is not NTFS: no run begins there that way.
  notntfs: {
    rows: [
      folder,
      space,
      {
        kind: "way",
        label: "패치 방식",
        ok: false,
        detail: "즉시 패치 불가",
        message: "즉시 패치는 NTFS 드라이브에서만 사용할 수 있습니다.",
      },
      patchFile,
    ],
    status: "패치 불가",
    statusBad: true,
    button: { kind: "download", enabled: true, primary: false },
  },
  leftover: {
    rows: [folder, space, leftover, patchFile],
    percent: 41,
    status: "중단된 패치 있음",
    button: { kind: "resume", enabled: true, primary: true },
  },
  tight: {
    rows: [folder, gameOpen, space, notRestored, leftover, patchFile],
    percent: 41,
    status: "패치 불가",
    statusBad: true,
    button: { kind: "resume", enabled: true, primary: false },
  },
  // Every line a ready screen can have at once, at a seat in a PC room: nine with the one
  // where the way of patching is chosen.
  full: {
    rows: [folder, gameOpen, notWritable, space, notRestored, leftover, patchFile, atSeat],
    percent: 41,
    status: "패치 불가",
    statusBad: true,
    button: { kind: "resume", enabled: true, primary: false },
    diagnosis: seatDiagnosis,
  },
  halfpatched: {
    current: null,
    rows: [folder, space, leftover, patchFile],
    halfPatched: true,
    discardAsks: true,
    percent: 41,
    status: "중단된 패치 있음",
    button: { kind: "resume", enabled: true, primary: true },
  },
  // An apply was cut off and the folder is being put back as it was: nothing is said of the
  // folder until it is.
  puttingback: {
    current: null,
    latest: null,
    patchAddress: "",
    checkedAt: null,
    rows: [folder, space, leftover, noPatchFile],
    status: "이전 패치 복구 대기",
    busy: true,
    button: { kind: "checkAgain", enabled: false, primary: false },
  },
  // An apply was cut off, its record cannot be read and the folder could not be put back
  // without it: nothing to go on with, and what the run left can only be removed as it is.
  journal: {
    current: null,
    latest: null,
    patchAddress: "",
    checkedAt: null,
    rows: [folder, space, journalUnreadable, leftover, noPatchFile],
    discardAsks: true,
    status: "작업 기록 손상",
    statusBad: true,
    button: { kind: "checkAgain", enabled: true, primary: false },
  },
  notinstalled: {
    current: null,
    latest: null,
    patchAddress: "",
    checkedAt: null,
    lamp: null,
    rows: [{ ...folder, ok: false, detail: "찾을 수 없음", message: "메이플스토리 설치 경로를 찾을 수 없습니다." }],
    percent: null,
    status: "설치 경로 미확인",
    statusBad: true,
    button: { kind: "install", enabled: true, primary: true },
    diagnosis: null,
  },
  failedcheck: {
    rows: [folder, space, noPatchFile],
    status: "서버 요청 실패",
    statusBad: true,
    button: { kind: "checkAgain", enabled: true, primary: false },
  },
  running: {
    phase: "running",
    run: building,
    percent: 61.5,
    status: "새 게임 파일 생성 중",
    button: { kind: "pause", enabled: true, primary: false },
  },
  downloading: {
    phase: "running",
    run: { ...building, step: "download", done: 515_396_075, total: 1_288_490_189, activity: "다운로드 중: 00421to00422.patch" },
    percent: 12,
    status: "패치 파일 다운로드 중",
    button: { kind: "pause", enabled: true, primary: false },
  },
  // The window was closed on the run: the user is asked whether it is to go on.
  closing: {
    phase: "running",
    run: { ...building, asking: true },
    percent: 61.5,
    status: "새 게임 파일 생성 중",
    button: { kind: "pause", enabled: true, primary: false },
  },
  stopping: {
    phase: "stopping",
    run: building,
    percent: 61.5,
    status: "일시정지 대기",
    busy: true,
    button: { kind: "pause", enabled: false, primary: false },
  },
  paused: {
    phase: "paused",
    run: building,
    percent: 61.5,
    status: "일시정지",
    button: { kind: "resume", enabled: true, primary: true },
  },
  failed: {
    phase: "failed",
    run: {
      ...building,
      failure: { id: 1, title: "패치에 실패했습니다.", message: "새 게임 파일 검사에 실패했습니다.", details: ["Map001.wz"] },
    },
    percent: 61.5,
    status: "패치 실패",
    statusBad: true,
    button: { kind: "retry", enabled: true, primary: true },
  },
  refused: {
    phase: "failed",
    run: {
      ...building,
      failure: {
        id: 1,
        title: "디스크 공간이 부족합니다.",
        message: "C: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)",
        details: [],
      },
    },
    percent: 45,
    status: "패치 불가",
    statusBad: true,
    button: { kind: "retry", enabled: true, primary: true },
  },
  done: {
    phase: "done",
    current: "Ver 1.2.422",
    patchAddress: "https://maplestory.dn.nexoncdn.co.kr/Patch/00423/00422to00423.patch",
    run: { ...building, activity: "" },
    rows: [folder, space, noPatchFile],
    percent: 100,
    status: "패치 완료",
    summary: "소요 시간 1분 35초 · 변경 파일 718개 · 추가 패치 없음",
    button: { kind: "launch", enabled: true, primary: true },
  },
};
const testFolder: Row = { ...folder, detail: "C:\\Nexon\\MapleT" };
/**
 * The test world beside the live game, installed, with the patch to 1.2.208 on its server.
 * Nothing is said of its server: nothing is known to be in the way.
 */
const testWithPatch: SiteView = {
  ...withPatch,
  server: "test",
  title: "메이플스토리 테스트월드",
  current: "Ver 1.2.207",
  latest: "Ver 1.2.208",
  patchAddress: testPatchAt,
  lamp: null,
  rows: [testFolder, space, { ...patchFile, detail: "01207to01208.patch · 1,229 MB" }],
};
/** The test world on a PC that does not have it, which is most: there is no figure and nothing to say of it. */
const testNotInstalled: SiteView = {
  ...testWithPatch,
  ...scripted.notinstalled,
  rows: [
    { ...folder, ok: false, detail: "찾을 수 없음", message: "메이플스토리 테스트월드 설치 경로를 찾을 수 없습니다." },
  ],
};
/** Screens on which the test world has something to say: what each page says, and whose run is at work. */
const worlds: Record<string, SiteView[]> = {
  // A run is at work for the live game, which keeps the test world's from starting.
  locked: [{ ...withPatch, ...scripted.running }, { ...testWithPatch, locked: "live" }],
  // Nothing proves which version the test world's folder is at: nothing is offered for it.
  unproven: [
    withPatch,
    {
      ...testWithPatch,
      current: null,
      patchAddress: "",
      checkedAt: null,
      rows: [testFolder, space, noPatchFile],
      status: "현재 버전 확인 불가",
      statusBad: true,
      button: { kind: "checkAgain", enabled: true, primary: false },
    },
  ],
  // The test world is closed between two of its operating periods.
  offseason: [
    withPatch,
    {
      ...testWithPatch,
      current: "Ver 1.2.208",
      patchAddress: "https://maplestory.dn.nexoncdn.co.kr/PatchT/01209/01208to01209.patch",
      lamp: { open: false, extended: false, until: null, season: false },
      rows: [testFolder, space, noPatchFile],
      percent: 100,
      status: "최신 버전",
      button: { kind: "launch", enabled: true, primary: true },
    },
  ],
  // A maintenance of the test world, as its board announces it.
  testmaintenance: [
    withPatch,
    { ...testWithPatch, lamp: { open: false, extended: false, until: "16:10", season: true } },
  ],
  // A run was left unfinished in the test world, so its page is the one the app opens on.
  unfinished: [withPatch, { ...testWithPatch, ...scripted.leftover, rows: [testFolder, space, leftover, patchFile] }],
  // The test world is open: its line asks for it to be closed, by its own name, which is the
  // longer of the two.
  testopen: [
    withPatch,
    {
      ...testWithPatch,
      rows: [
        testFolder,
        {
          ...gameOpen,
          label: "메이플스토리 테스트월드를 종료한 후 실행해 주세요.",
          message: "메이플스토리 테스트월드를 종료한 후 실행해 주세요.",
        },
        space,
        { ...patchFile, detail: "01207to01208.patch · 1,229 MB" },
      ],
      status: "패치 불가",
      statusBad: true,
      button: { kind: "download", enabled: true, primary: false },
    },
  ],
};
/**
 * What the address asks for: one of the screens above, and after an `@` the page that is
 * open, when that is not the one the app would open on: `#running@settings`, `#patch@test`,
 * `#seat@diagnosis`. `#outdated` is the screen of an app that is too old. The app always has
 * both clients; on the screens of the live game alone, the test world is not installed.
 */
const [asked, askedPage] = window.location.hash.slice(1).split("@");
const browserSites = worlds[asked] ?? [{ ...withPatch, ...scripted[asked] }, testNotInstalled];
const atWork = (site: SiteView) => site.phase === "starting" || site.phase === "running" || site.phase === "stopping";
export const browserView: AppView = {
  sites: browserSites,
  // On the screen that says a drive does not take the immediate way, that way is chosen.
  settings: { ...defaultSettings, immediate: asked === "notntfs" },
  update: asked === "outdated" ? { latest: "0.2.0", required: true, message: null } : null,
  running: browserSites.find(atWork)?.server ?? null,
};
/** The page a stand-in screen has open, when its address names one. The app opens where it sees fit. */
export const opensOn: ServerId | "settings" | "diagnosis" | null =
  !inApp && (askedPage === "live" || askedPage === "test" || askedPage === "settings" || askedPage === "diagnosis")
    ? askedPage
    : null;
export const browserReport = "Maple Fast Patch 측정 결과";
/**
 * How far the check of the game's files is on the screen `#filecheck`. Nothing is read in
 * the browser, so it stays there: 2.1 GB of the 5.8 GB there are to read.
 */
export const browserChecking: Checking | null =
  asked === "filecheck" ? { server: "live", done: 2_254_857_830, total: 6_227_702_579 } : null;
/** What the page is to ask before the game's files are checked, as the backend has it ask. */
export const browserAsksToCheck = "게임 파일을 검사하시겠습니까?\n약 1분이 걸립니다.";
/** 패치 후 게임 자동 실행 as the browser last had it set. */
let autoStartWas = false;
/**
 * What the page is to ask once settings are set in the browser. On the screen `#rights` the
 * app runs without administrator rights: switching 패치 후 게임 자동 실행 on asks whether it
 * is to start again with them, as the backend has the page ask.
 */
export function browserAsks(autoStart: boolean) {
  const switchedOn = autoStart && !autoStartWas;
  autoStartWas = autoStart;
  return asked === "rights" && switchedOn
    ? "게임 자동 실행에는 관리자 권한이 필요합니다.\n관리자 권한으로 다시 시작하시겠습니까?"
    : null;
}
/** A start in the browser begins nothing. With something in the way it is turned down for that, as in the app. */
export function browserStart(server: ServerId) {
  const rows = browserSites.find((site) => site.server === server)?.rows ?? [];
  const messages = rows.filter((row) => row.ok === false).map((row) => row.message);
  const refusal: Refusal = { kind: "blocked", messages };
  return messages.length > 0 ? Promise.reject<void>(refusal) : Promise.resolve();
}
/**
 * A start by address in the browser begins nothing either. The address is held against a
 * likeness of the backend's rule, rough and for looking at only: each answer the page can
 * get is there to be seen.
 */
export function browserStartAt(server: ServerId, address: string) {
  const turnedDown = (kind: Refusal["kind"], message: string) => {
    const refusal: Refusal = { kind, messages: [message] };
    return Promise.reject<void>(refusal);
  };
  const parts = /^https?:\/\/([a-z0-9.-]+)(\/[A-Za-z0-9._/-]+\.patch)$/.exec(address.trim());
  if (!parts) return turnedDown("address", "패치 주소가 아닙니다.");
  const [, host, path] = parts;
  if (!/(^|\.)(nexoncdn\.co\.kr|nexon\.com)$/.test(host)) return turnedDown("address", "넥슨 패치 서버 주소가 아닙니다.");
  const own = host === "maplestory.dn.nexoncdn.co.kr" && path.startsWith(server === "live" ? "/Patch/" : "/PatchT/");
  if (!own) {
    const game = browserSites.find((site) => site.server === server)?.title ?? "";
    return turnedDown("ask", `${game} 패치 주소가 아닙니다.\n이 주소로 패치하시겠습니까?`);
  }
  return browserStart(server);
}
// Another address is another screen.
if (!inApp) window.addEventListener("hashchange", () => window.location.reload());
