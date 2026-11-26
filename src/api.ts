// What the backend tells the page and what the page asks of it. The backend decides what
// the screen says; the page draws it.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";

/** Which client: the live game, or the test world. */
export type ServerId = "live" | "test";

export type Step = "download" | "verifyPatch" | "verifyInstall" | "trial" | "rebuild" | "apply";

export type Settings = {
  /** Apply a patch as soon as one is found, without waiting for the button. */
  autoPatch: boolean;
  /** Say so when the game's maintenance is over or has been extended. */
  maintenanceAlert: boolean;
  /** Start the game by itself once a patch is done and the game can be played. */
  autoStart: boolean;
  /** Close this app when it has started the game. */
  exitAfterStart: boolean;
  /** Closing the window only puts it away; the app stays at work next to the clock. */
  background: boolean;
  /** Replace game files as they are built when the drive has no room to build them all first. */
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
  kind: "folder" | "running" | "writable" | "space" | "recovery" | "leftover" | "patchFile";
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
  /** The patch file the user picked for it; `null` for the patch from the server. */
  source: string | null;
  label: string | null;
  /** The stop was asked for by closing the window; the app goes once the run has stopped. */
  leaving: boolean;
  percent: number;
  step: Step | null;
  /** Bytes done and total within the step, and how many of them this run worked for itself. */
  done: number;
  total: number;
  worked: number;
  /** What the run is working on, as in "생성중: Map001.wz". */
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
  /** When the server last answered a check, in milliseconds since 1970. */
  checkedAt: number | null;
  lamp: Lamp | null;
  /** What the ready screen lists, in order. */
  rows: Row[];
  customDir: boolean;
  halfPatched: boolean;
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

/** Why a run was not begun. The first of `messages` is the one to lead with. */
export type Refusal = { kind: "blocked" | "busy" | "outdated" | "nothing" | "notPatch"; messages: string[] };

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
const patchFile: Row = { kind: "patchFile", label: "패치 파일", ok: true, detail: "1,229 MB", message: "" };
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
const leftover: Row = { kind: "leftover", label: "이전 패치 작업", ok: null, detail: "12.3 GB", message: "" };
const building: Run = {
  source: null,
  label: "1.2.422",
  leaving: false,
  percent: 61.5,
  step: "rebuild",
  done: 6_442_450_944,
  total: 15_032_385_536,
  worked: 6_442_450_944,
  activity: "생성중: Map001.wz",
  failure: null,
};
/** A game folder with the patch to 1.2.422 on the server and nothing in the way. */
const withPatch: SiteView = {
  server: "live",
  rev: 1,
  title: "메이플스토리",
  current: "1.2.421",
  latest: "1.2.422",
  checkedAt: Date.now(),
  lamp: { open: true, extended: false, until: null, season: true },
  rows: [folder, space, patchFile],
  customDir: false,
  halfPatched: false,
  phase: "ready",
  run: null,
  percent: 0,
  status: "새 패치 있음",
  statusBad: false,
  busy: false,
  summary: null,
  button: { kind: "download", enabled: true, primary: true },
  locked: null,
};
const scripted: Record<string, Partial<SiteView>> = {
  patch: {},
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
    percent: 41,
    status: "중단된 패치 있음",
    button: { kind: "resume", enabled: true, primary: true },
  },
  notinstalled: {
    current: null,
    latest: null,
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
    run: { ...building, percent: 12, step: "download", done: 515_396_075, total: 1_288_490_189, activity: "다운로드중: 00421to00422.patch" },
    percent: 12,
    status: "패치 파일 다운로드 중",
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
  lamp: null,
  rows: [testFolder, space, patchFile],
};
/** The test world on a PC that does not have it, which is most: there is no figure and nothing to say of it. */
const testNotInstalled: SiteView = {
  ...testWithPatch,
  current: null,
  latest: null,
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
  settings: defaultSettings,
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
// Another address is another screen.
if (!inApp) window.addEventListener("hashchange", () => window.location.reload());

/** Everything the screen shows, as it stands: for the start, and for a window that was put away. */
export const takeSnapshot = () => (inApp ? invoke<AppView>("snapshot") : Promise.resolve(browserView));
export const setSettings = (settings: Settings) =>
  inApp ? invoke<void>("set_settings", { settings }) : Promise.resolve();
/**
 * Starts a patch for `server`: of the patch file at `file` when one was picked, and of what
 * the screen offers otherwise. Rejects with a `Refusal` when none is begun.
 */
export const startPatch = (server: ServerId, file: string | null = null) =>
  inApp ? invoke<void>("start_patch", { server, file }) : browserStart(server);
export const stopPatch = (server: ServerId) => (inApp ? invoke<void>("stop_patch", { server }) : Promise.resolve());
/** Puts the run that ended away and shows the ready screen again. */
export const dismissRun = (server: ServerId) =>
  inApp ? invoke<void>("dismiss_run", { server }) : Promise.resolve();
/** Has the server asked again at once, after a check that failed. */
export const askAgain = (server: ServerId) => (inApp ? invoke<void>("check_again", { server }) : Promise.resolve());
/**
 * Where a picked patch file was downloaded from, when that was somewhere other than Nexon:
 * the host name, or "" when only "from the internet" is known. `null` for anything else.
 */
export const foreignOrigin = (file: string) =>
  inApp ? invoke<string | null>("foreign_origin", { file }) : Promise.resolve(null);
/** Deletes what a paused or interrupted run left in the install folder. */
export const discardLeftovers = (server: ServerId) =>
  inApp ? invoke<void>("discard_leftovers", { server }) : Promise.resolve();
export const launchGame = (server: ServerId) => (inApp ? invoke<void>("launch_game", { server }) : Promise.resolve());
/** Opens the official page that offers the game's installer. */
export const openDownloadPage = (server: ServerId) =>
  inApp ? invoke<void>("open_download_page", { server }) : Promise.resolve();
/** Opens the page a newer version of this app is downloaded from. Which page that is, the backend knows. */
export const openReleasePage = () => (inApp ? invoke<void>("open_release_page") : Promise.resolve());
/** Uses `path` as the install folder; `null` goes back to finding it automatically. */
export const setInstallDir = (server: ServerId, path: string | null) =>
  inApp ? invoke<void>("set_install_dir", { server, path }) : Promise.resolve();
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

/** Lets the user pick the install folder. Resolves to `null` when they cancel. */
export const pickFolder = () => (inApp ? open({ directory: true }) : Promise.resolve(null));

/** Lets the user pick a patch file. Resolves to `null` when they cancel. */
export const pickPatchFile = () =>
  inApp
    ? open({ filters: [{ name: "패치 파일", extensions: ["patch", "exe"] }] })
    : Promise.resolve(null);

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
