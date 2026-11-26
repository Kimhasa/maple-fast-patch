// What the backend tells the page and what the page asks of it. The backend decides what
// the screen says; the page draws it.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Which client: the live game, or the test world. */
export type ServerId = "live" | "test";

export type Step = "download" | "verifyPatch" | "verifyInstall" | "trial" | "rebuild" | "apply";

export type Settings = {
  /** Apply a patch as soon as one is found, without waiting for the button. */
  autoPatch: boolean;
  /** Say so when the game's maintenance has begun, has been extended or is over. */
  maintenanceAlert: boolean;
  /** Start the game by itself once a patch is done and the game can be played. */
  autoStart: boolean;
  /** Close this app when it has started the game. */
  exitAfterStart: boolean;
  /** Closing the window only puts it away; the app stays at work next to the clock. */
  background: boolean;
  /** The way of patching that was chosen: every run replaces game files as it builds them. */
  immediate: boolean;
};

type Update = {
  /** The newer version, such as "0.2.0". */
  latest: string;
  /** The version running is older than the oldest one that is still to be used. */
  required: boolean;
  /** A line that goes with it, as plain text. */
  message: string | null;
};

/**
 * Whether the game can be played right now, as far as Nexon says. Of a client whose
 * maintenance Nexon's service does not list, as the test world's, there is none while
 * nothing is known to be in the way.
 */
type Lamp = {
  /** No maintenance is underway. */
  open: boolean;
  /** The maintenance underway has been extended. */
  extended: boolean;
  /** When the maintenance underway is planned to end, as a clock shows it; `null` once that time has passed. */
  until: string | null;
  /** The game is within one of its operating periods. Between two of them the test world is closed altogether. */
  season: boolean;
};

/** One line of the ready screen. The page goes by `kind` for what the line offers beside its words. */
export type Row = {
  /** A line of kind `way` is shown on the page's own line for the choice of the way of patching. */
  kind: "folder" | "running" | "writable" | "space" | "recovery" | "way" | "leftover" | "patchFile";
  label: string;
  /** A check that passed or did not; `null` for a line that is no check. */
  ok: boolean | null;
  detail: string;
  /** For a check that did not pass: what is wrong, as the sentence a dialog shows. */
  message: string;
};

/** What the screen says once of a run that failed or was turned down. */
type Failure = { id: number; title: string; message: string | null; details: string[] };

/** The run that is on a client's screen: the one at work, or one that has ended and was not put away. */
type Run = {
  /** The address the user gave for its patch; `null` for the patch the app found. */
  source: string | null;
  label: string | null;
  /** The stop was asked for by closing the window; the app goes once the run has stopped. */
  leaving: boolean;
  /**
   * The window was closed on the run, and the user is to be asked whether it is to go on
   * with the app next to the clock. Until the backend has their answer.
   */
  asking: boolean;
  percent: number;
  step: Step | null;
  /** Bytes done and total within the step, and how many of them this run worked for itself. */
  done: number;
  total: number;
  worked: number;
  /** What the run is working on, as in "생성 중: Map001.wz". */
  activity: string;
  failure: Failure | null;
};

export type ButtonKind =
  | "install"
  | "download"
  | "resume"
  | "retry"
  | "checkAgain"
  | "launch"
  | "gameRunning"
  | "pause";

/** Everything the screen of one client says, as it is to be drawn. */
export type SiteView = {
  server: ServerId;
  /** Counts what was said of this client. What comes with a higher number is newer. */
  rev: number;
  /** What the game is called. */
  title: string;
  current: string | null;
  latest: string | null;
  /**
   * The address the user is given to begin from when they state where the patch is: that of
   * the patch the app found, or where the next one would be. Empty where neither is known.
   */
  patchAddress: string;
  /** When the server last answered a check, in milliseconds since 1970. */
  checkedAt: number | null;
  lamp: Lamp | null;
  /** What the ready screen lists, in order. */
  rows: Row[];
  customDir: boolean;
  halfPatched: boolean;
  /**
   * Removing what a run left cannot be made good by the app: the folder is partly new, or
   * the record of an apply that was cut off cannot be read. The user is asked first.
   */
  discardAsks: boolean;
  phase: "ready" | "starting" | "running" | "stopping" | "paused" | "failed" | "done";
  run: Run | null;
  /** What the big figure shows; `null` draws "-". */
  percent: number | null;
  /** The footer's word, whether it is bad news, and whether something is being waited for. */
  status: string;
  statusBad: boolean;
  busy: boolean;
  /** The line under a finished run. */
  summary: string | null;
  button: { kind: ButtonKind; enabled: boolean; primary: boolean };
  /** Whose run keeps this one from starting. */
  locked: ServerId | null;
};

/** What is not of one client: what the user has set, word of a newer version, and whose run is at work. */
export type AppWord = { settings: Settings; update: Update | null; running: ServerId | null };

export type AppView = AppWord & { sites: SiteView[] };

/** Where a run is, as it says up to a dozen times a second. */
export type Progress = {
  server: ServerId;
  percent: number;
  step: Step | null;
  status: string;
  done: number;
  total: number;
  worked: number;
  activity: string;
};

/**
 * Why a run was not begun. The first of `messages` is the one to lead with. With `address`
 * the address the user gave is not one a patch is fetched from, or no patch is there. With
 * `ask` it is Nexon's and not among the game's own patch files: the one message is the
 * question, and the run waits for the user's yes.
 */
export type Refusal = {
  kind: "blocked" | "busy" | "outdated" | "nothing" | "address" | "ask";
  messages: string[];
};

/** False when the page is opened in a plain browser (`yarn dev`) instead of the app. */
const inApp = "__TAURI_INTERNALS__" in window;

/** What the settings are before the backend has said what the user set. */
export const defaultSettings: Settings = {
  autoPatch: false,
  maintenanceAlert: true,
  autoStart: false,
  exitAfterStart: false,
  background: false,
  immediate: false,
};

/**
 * Stand-in screens, so that the page can be worked on and looked at in a browser. Nothing
 * is patched. Which one shows is chosen by the address: `http://localhost:1420/#paused`.
 */
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
const gameOpen: Row = {
  kind: "running",
  label: "관련 프로그램 실행 중",
  ok: false,
  detail: "MapleStory.exe",
  message: "게임 관련 프로그램이 실행 중입니다.\n해당 프로그램을 종료해 주세요.",
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
const building: Run = {
  source: null,
  label: "1.2.422",
  leaving: false,
  asking: false,
  percent: 61.5,
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
  current: "1.2.421",
  latest: "1.2.422",
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
};
const scripted: Record<string, Partial<SiteView>> = {
  patch: {},
  // A minor patch within the version that is installed.
  minor: {
    current: "1.2.421 (3)",
    latest: "1.2.421 (4)",
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
    latest: "1.2.421",
    rows: [folder, space, noPatchFile],
    percent: 100,
    status: "최신 버전",
    button: { kind: "launch", enabled: true, primary: true },
  },
  outdated: {
    status: "최신 버전으로 업데이트해 주세요.",
    statusBad: true,
    button: { kind: "download", enabled: false, primary: false },
  },
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
    run: { ...building, percent: 12, step: "download", done: 515_396_075, total: 1_288_490_189, activity: "다운로드 중: 00421to00422.patch" },
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
      percent: 45,
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
    current: "1.2.422",
    patchAddress: "https://maplestory.dn.nexoncdn.co.kr/Patch/00423/00422to00423.patch",
    run: { ...building, percent: 100, activity: "" },
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
  current: "1.2.207",
  latest: "1.2.208",
  patchAddress: testPatchAt,
  lamp: null,
  rows: [testFolder, space, { ...patchFile, detail: "01207to01208.patch · 1,229 MB" }],
};
/** The test world on a PC that does not have it, which is most: there is no figure and nothing to say of it. */
const testNotInstalled: SiteView = {
  ...testWithPatch,
  current: null,
  latest: null,
  patchAddress: "",
  checkedAt: null,
  rows: [
    { ...folder, ok: false, detail: "찾을 수 없음", message: "메이플스토리 테스트월드 설치 경로를 찾을 수 없습니다." },
  ],
  percent: null,
  status: "설치 경로 미확인",
  statusBad: true,
  button: { kind: "install", enabled: true, primary: true },
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
      current: "1.2.208",
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
};
/**
 * What the address asks for: one of the screens above, and after an `@` the page that is
 * open, when that is not the one the app would open on: `#running@settings`, `#patch@test`.
 * `#outdated` is the screen of an app that is too old. The app always has both clients; on
 * the screens of the live game alone, the test world is not installed.
 */
const [asked, askedPage] = window.location.hash.slice(1).split("@");
const browserSites = worlds[asked] ?? [{ ...withPatch, ...scripted[asked] }, testNotInstalled];
const atWork = (site: SiteView) => site.phase === "starting" || site.phase === "running" || site.phase === "stopping";
const browserView: AppView = {
  sites: browserSites,
  // On the screen that says a drive does not take the immediate way, that way is chosen.
  settings: { ...defaultSettings, immediate: asked === "notntfs" },
  update: asked === "outdated" ? { latest: "0.2.0", required: true, message: null } : null,
  running: browserSites.find(atWork)?.server ?? null,
};
/** The page a stand-in screen has open, when its address names one. The app opens where it sees fit. */
export const opensOn: ServerId | "settings" | null =
  !inApp && (askedPage === "live" || askedPage === "test" || askedPage === "settings") ? askedPage : null;
const browserReport = "Maple Fast Patch 측정 결과";
/** A start in the browser begins nothing. With something in the way it is turned down for that, as in the app. */
function browserStart(server: ServerId) {
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
function browserStartAt(server: ServerId, address: string) {
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

/** Everything the screen shows, as it stands: for the start, and for a window that was put away. */
export const takeSnapshot = () => (inApp ? invoke<AppView>("snapshot") : Promise.resolve(browserView));
export const setSettings = (settings: Settings) =>
  inApp ? invoke<void>("set_settings", { settings }) : Promise.resolve();
/** Starts a patch of what the screen of `server` offers. Rejects with a `Refusal` when none is begun. */
export const startPatch = (server: ServerId) =>
  inApp ? invoke<void>("start_patch", { server }) : browserStart(server);
/**
 * Starts a patch for `server` of the patch at `address`, which the user gave. Whether a patch
 * is fetched from there is the backend's to decide. Rejects with a `Refusal` when no run is
 * begun: as `ask` when the user is to be asked first.
 */
export const startPatchAt = (server: ServerId, address: string) =>
  inApp ? invoke<void>("start_patch_at", { server, address }) : browserStartAt(server, address);
/**
 * Starts the patch at the address the user was just asked about: they said yes. Which
 * address that is, the backend knows.
 */
export const confirmPatchAddress = (server: ServerId) =>
  inApp ? invoke<void>("confirm_patch_address", { server }) : browserStart(server);
export const stopPatch = (server: ServerId) => (inApp ? invoke<void>("stop_patch", { server }) : Promise.resolve());
/**
 * Answers the question the backend has the page ask when the window is closed on a run at
 * work. With `background` the run goes on and the app is next to the clock for the time
 * being, with `quit` the run is stopped and the app goes, and `stay` leaves everything as it
 * is: the question was put aside.
 */
export const answerClose = (answer: "background" | "quit" | "stay") =>
  inApp ? invoke<void>("answer_close", { answer }) : Promise.resolve();
/** Puts the run that ended away and shows the ready screen again. */
export const dismissRun = (server: ServerId) =>
  inApp ? invoke<void>("dismiss_run", { server }) : Promise.resolve();
/** Has the server asked again at once, after a check that failed. */
export const askAgain = (server: ServerId) => (inApp ? invoke<void>("check_again", { server }) : Promise.resolve());
/**
 * Deletes what a paused or interrupted run left in the install folder. `confirmed` says that
 * the user was asked and said yes, where they had to be asked. How it is deleted, the backend
 * decides by what the folder is.
 */
export const discardLeftovers = (server: ServerId, confirmed: boolean) =>
  inApp ? invoke<void>("discard_leftovers", { server, confirmed }) : Promise.resolve();
export const launchGame = (server: ServerId) => (inApp ? invoke<void>("launch_game", { server }) : Promise.resolve());
/** Opens the official page that offers the game's installer. */
export const openDownloadPage = (server: ServerId) =>
  inApp ? invoke<void>("open_download_page", { server }) : Promise.resolve();
/** Opens the page a newer version of this app is downloaded from. Which page that is, the backend knows. */
export const openReleasePage = () => (inApp ? invoke<void>("open_release_page") : Promise.resolve());
/** Lets the user pick the install folder of `server`, in a dialog the backend opens, and uses what they pick. */
export const pickInstallDir = (server: ServerId) =>
  inApp ? invoke<void>("pick_install_dir", { server }) : Promise.resolve();
/** Goes back from a folder the user picked to the one that is found automatically. */
export const resetInstallDir = (server: ServerId) =>
  inApp ? invoke<void>("reset_install_dir", { server }) : Promise.resolve();
/**
 * How the last run that finished went, as text to copy: the time each step took and what
 * kind of PC it ran on. `null` when no run has finished since the app was started.
 */
export const runReport = (server: ServerId) =>
  inApp ? invoke<string | null>("run_report", { server }) : Promise.resolve<string | null>(browserReport);

/**
 * Puts `text` on the clipboard and resolves to whether it got there. Where the WebView
 * does not let the page write to the clipboard, the text is copied the older way, out of
 * a text box that is selected for the moment.
 */
export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const focused = document.activeElement;
    const box = document.createElement("textarea");
    box.value = text;
    box.style.position = "fixed";
    box.style.opacity = "0";
    document.body.append(box);
    box.select();
    const copied = document.execCommand("copy");
    box.remove();
    if (focused instanceof HTMLElement) focused.focus();
    return copied;
  }
}

/**
 * Subscribes to what the backend says: of a client's screen whenever it changes, of a run
 * as it goes, and of what is not of one client. `listening` resolves once nothing that is
 * said can be missed any more, and `stop` ends the subscription.
 */
export function onSaid(heard: {
  site: (site: SiteView) => void;
  progress: (progress: Progress) => void;
  app: (word: AppWord) => void;
}) {
  if (!inApp) return { listening: Promise.resolve(), stop: () => {} };
  const subscriptions = [
    listen<SiteView>("site", (event) => heard.site(event.payload)),
    listen<Progress>("progress", (event) => heard.progress(event.payload)),
    listen<AppWord>("app", (event) => heard.app(event.payload)),
  ];
  return {
    listening: Promise.all(subscriptions).then(() => {}),
    stop: () => subscriptions.forEach((subscription) => void subscription.then((unlisten) => unlisten())),
  };
}

/**
 * Calls `onShown` when the window is looked at again: brought back from next to the clock,
 * made large again, or given the keyboard. Returns a function that ends that.
 */
export function onShown(onShown: () => void) {
  if (!inApp) return () => {};
  const visible = () => {
    if (document.visibilityState === "visible") onShown();
  };
  document.addEventListener("visibilitychange", visible);
  const focused = getCurrentWindow().onFocusChanged((event) => {
    if (event.payload) onShown();
  });
  return () => {
    document.removeEventListener("visibilitychange", visible);
    void focused.then((unlisten) => unlisten());
  };
}

/**
 * Says that the page has on it what the backend first told it. The window is made hidden
 * and shows now, with the page's own letters loaded, instead of as a plain surface that the
 * page then draws itself onto. A page that never gets this far is shown by the backend
 * after a moment all the same.
 */
export async function pageDrawn() {
  if (!inApp) return;
  const faces = ["400", "700"].map((weight) => document.fonts.load(`${weight} 16px "NEXON Lv2 Gothic"`));
  await Promise.allSettled(faces);
  await invoke<void>("page_drawn");
}

export const minimizeWindow = () => (inApp ? getCurrentWindow().minimize() : Promise.resolve());
export const closeWindow = () => (inApp ? getCurrentWindow().close() : Promise.resolve());
