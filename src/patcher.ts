// What the window knows and what it can do. The pages and the frame around them only draw this.
//
// What a client's page says is decided in the backend, which looks after the game whether
// the window is there or not; this keeps what it last said. What is the window's own is
// here too: which page and which dialog is open, and how fast a run is going.
//
// The state lives here, outside React, because most of what changes it does not come from
// the screen: what the backend says, a run as it goes, a timer. Every action reads the
// state as it is at that moment, never a copy from an earlier render.

import {
  answerClose,
  askAgain,
  askFileCheck,
  copyText,
  confirmPatchAddress,
  diagnoseAgain,
  discardLeftovers,
  dismissRun,
  launchGame,
  onSaid,
  onShown,
  openBugReportPage,
  openDownloadPage,
  openReleasePage,
  pickInstallDir,
  resetInstallDir,
  restartWithRights,
  runReport,
  setSettings,
  startFileCheck,
  startPatch,
  startPatchAt,
  stopFileCheck,
  stopPatch,
  takeSnapshot,
} from "./api";
import { opensOn } from "./standins";
import {
  defaultSettings,
  type AppView,
  type AppWord,
  type Checking,
  type Progress,
  type Refusal,
  type ServerId,
  type Settings,
  type SiteView,
} from "./types";

/** A download that brings nothing for this long is not timed: nothing is arriving. */
const QUIET_MS = 4000;
/** How many lines the ready screen has room for at their usual height, in the smallest window. */
const ROWS_AT_EASE = 5;
/** How many it has room for there once they are drawn closer together. */
const ROWS_TIGHT = 8;

/**
 * What the window shows beside its sidebar: the page of a client, the page that says what
 * kind of PC this is, or the settings.
 */
export type Page = ServerId | "diagnosis" | "settings";

export type State = {
  /** What the backend last said of everything the screen shows. Of a client, nothing until it has. */
  app: AppView;
  /** The page that is open. `null` until it is settled where the app opens. */
  page: Page | null;
  /**
   * The dialog that is open. The "closing" one is the backend's: it is open while a run
   * says that the window was closed on it, and the backend is given the answer.
   */
  dialog: "none" | "blocked" | "failed" | "ask" | "address" | "closing";
  /** What the "ask" dialog asks, and what a yes does. */
  question: { message: string; yes: () => void } | null;
  /** What the "address" dialog is for: whose patch, and what stands in its field. */
  address: { server: ServerId; value: string } | null;
  /** What the "failed" dialog says: a headline, the reason, and any further lines. */
  failure: { title: string; message: string | null; details: string[] } | null;
  /**
   * The start that was turned down for what is in the way: whose it was, and what the
   * backend said was in the way. The "blocked" dialog says that until the page lists it.
   */
  blocked: { server: ServerId; messages: string[] } | null;
  /** How the finished run went was just copied; the button that copies it says so for a moment. */
  copied: boolean;
  /** The client whose report of the PC was just copied; the button that copies it says so for a moment. */
  reportCopied: ServerId | null;
  /** How far each check of a client's game files is that is at work and has said so. */
  checks: Checking[];
  /** How fast the current step is going, in bytes a second. Measured for the two long steps. */
  speed: number;
  /** Seconds the current step still needs, once it has been watched long enough to say. */
  left: number | null;
};

let state: State = {
  app: { sites: [], settings: defaultSettings, update: null, running: null },
  page: opensOn,
  dialog: "none",
  question: null,
  address: null,
  failure: null,
  blocked: null,
  copied: false,
  reportCopied: null,
  checks: [],
  speed: 0,
  left: null,
};

/** The reading the download speed is measured from. */
let speedFrom: { at: number; bytes: number } | null = null;
/**
 * Readings of the current step, one a second, for the time it still needs. The speed of one
 * second says little about that: building goes several times faster through some files than
 * through others. The estimate goes by the last stretch as a whole, and is only made once
 * there is enough of it. It counts what this run worked for itself (`worked`): files an
 * earlier run had built are only checked, many times faster than files are built.
 */
let pace: { at: number; bytes: number }[] = [];
const PACE_READINGS = 20;
const PACE_MS = 5000;
/** When the run last said where it is. */
let heardAt = 0;
/** How many times the backend has said what is not of one client. */
let wordsHeard = 0;
/** The backend has said which clients there are. Until then one of them may be all that is known. */
let clientsKnown = false;
/** The last failure of each client's runs that a dialog has shown. Each is shown once. */
const failuresShown = new Map<ServerId, number>();
/** The game is being started: a second press does nothing until the first is answered. */
let launching = false;

const listeners = new Set<() => void>();

function set(change: Partial<State>) {
  state = { ...state, ...change };
  for (const listener of listeners) listener();
}

/** For React's `useSyncExternalStore`. */
export function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
export const getState = () => state;

/**
 * The page the app opens on: that of a client with a run that was left unfinished, and
 * otherwise the live game's.
 */
function opening(sites: SiteView[]): Page | null {
  const unfinished = sites.find(
    (site) => (site.phase !== "ready" && site.phase !== "done") || site.halfPatched || site.button.kind === "resume",
  );
  return (unfinished ?? sites.find((site) => site.server === "live") ?? sites[0])?.server ?? null;
}

/** What a client's page lists as being in the way of a patch, a sentence each. */
const inTheWayOf = (site: SiteView | undefined) =>
  site?.rows.filter((row) => row.ok === false).map((row) => row.message) ?? [];

/** Everything the screen shows that follows from the state. */
export function view(s: State) {
  const { sites } = s.app;
  /** The page that is open. Until that is settled, it is the one the app would open on. */
  const page = s.page ?? opening(sites);
  /** The client whose page is open; none while the settings are. */
  const site = sites.find((site) => site.server === page) ?? null;
  const rows = site?.rows ?? [];
  /** The install folder is there and is a game folder. Only then are there versions to show. */
  const gameFolder = rows.some((row) => row.kind === "folder" && row.ok);
  /**
   * The way of patching that is chosen is one the drive does not take: the backend's line
   * of it. It is not drawn among the lines; the page's own line for the choice carries it.
   */
  const wayRefused = rows.find((row) => row.kind === "way" && row.ok === false) ?? null;
  const listedRows = rows.filter((row) => row.kind !== "way");
  /**
   * The ready screen has more lines than fit at their usual height. It then draws them closer
   * together. Under the lines the backend lists stands one of the page's own, where the way
   * of patching is chosen, whenever there is a game folder.
   */
  const drawnRows = listedRows.length + (gameFolder ? 1 : 0);
  const tightRows = drawnRows > ROWS_AT_EASE;
  /**
   * There are more lines than fit even so: all of them at once, at a PC that is no ordinary
   * one, which has a line that says so. They are then drawn closer still.
   */
  const tightestRows = drawnRows > ROWS_TIGHT;
  /** What is in the way of the start that was turned down, a sentence each. */
  const listed = inTheWayOf(sites.find((site) => site.server === s.blocked?.server));
  const inTheWay = listed.length > 0 ? listed : (s.blocked?.messages ?? []);
  const run = site?.phase === "running" ? site.run : null;
  const moving = run !== null && s.speed > 0;
  /** The download speed, when there is one worth showing. */
  const downloadSpeed = moving && run.step === "download" ? s.speed : null;
  /** Seconds the building of the new files still needs. */
  const remaining = moving && run.step === "rebuild" ? s.left : null;

  return {
    page,
    site,
    gameFolder,
    listedRows,
    wayRefused,
    tightRows,
    tightestRows,
    inTheWay,
    downloadSpeed,
    remaining,
  };
}

/**
 * Takes in what the backend says of a client's page. It says so whenever something has
 * changed, and counts what it says: what comes with a count that is not higher is older or
 * the same, and is dropped.
 */
function heard(site: SiteView) {
  const known = state.app.sites.find((other) => other.server === site.server);
  if (known && site.rev <= known.rev) return;
  const sites = known ? state.app.sites.map((other) => (other === known ? site : other)) : [...state.app.sites, site];
  // Only a run that is going has a speed. There is one run at a time, whichever client's.
  const going = sites.some((other) => other.phase === "running");
  if (!going) {
    speedFrom = null;
    pace = [];
  }
  set({ app: { ...state.app, sites }, ...(going ? {} : { speed: 0, left: null }) });
  settle();
  // How far a check of the game's files is, is kept only while the check is at work.
  if (!site.diagnosis?.checking && state.checks.some((check) => check.server === site.server)) {
    set({ checks: state.checks.filter((check) => check.server !== site.server) });
  }
  // A dialog about what is in the way has nothing left to say once nothing is.
  if (state.dialog === "blocked" && site.server === state.blocked?.server && inTheWayOf(site).length === 0) {
    set({ dialog: "none" });
  }
  // The window was closed on a run at work, and the user is asked what is to become of the
  // run. The backend says so with the run, and again once that is settled: by an answer,
  // or by the run, which has stopped or ended meanwhile.
  const [asking, asked] = [site.run?.asking ?? false, known?.run?.asking ?? false];
  if (asking && !asked) set({ dialog: "closing" });
  if (!asking && asked && state.dialog === "closing") set({ dialog: "none" });
  // A run that has finished has the screen to itself.
  if (site.phase === "done" && known?.phase !== "done") set({ dialog: "none" });
  const failure = site.run?.failure ?? null;
  if (failure !== null && failure.id > (failuresShown.get(site.server) ?? 0)) {
    failuresShown.set(site.server, failure.id);
    set({ failure, dialog: "failed" });
  }
}

/**
 * Settles where the app opens, once every client's folder has been looked at: only then is
 * it known whether a run was left unfinished in one. A folder that was looked at has its
 * line on the page.
 */
function settle() {
  const { sites } = state.app;
  const looked = clientsKnown && sites.length > 0 && sites.every((site) => site.rows.length > 0);
  if (state.page === null && looked) set({ page: opening(sites) });
}

/** Takes in what the backend says that is not of one client. */
function heardWord(word: AppWord) {
  wordsHeard += 1;
  set({ app: { ...state.app, ...word } });
}

/** Takes in where the run at work says it is. Everything else on its page stays. */
function progressed(progress: Progress) {
  const site = state.app.sites.find((known) => known.server === progress.server);
  if (!site?.run) return;
  const at = Date.now();
  // Speed is counted from the first figure of a step, which after a pause already
  // includes what was done before.
  const paced =
    site.phase === "running" &&
    (progress.step === "download" || progress.step === "rebuild") &&
    progress.step === site.run.step;
  if (!paced) {
    speedFrom = null;
    pace = [];
  } else if (speedFrom === null && progress.total > 0) {
    speedFrom = { at, bytes: progress.done };
    pace = [{ at, bytes: progress.worked }];
  }
  heardAt = at;
  const { percent, step, done, total, worked, activity } = progress;
  const told ={ ...site, percent, run: { ...site.run, step, done, total, worked, activity } };
  set({
    app: { ...state.app, sites: state.app.sites.map((known) => (known === site ? told : known)) },
    speed: paced ? state.speed : 0,
    left: paced ? state.left : null,
  });
}

/**
 * Takes in how far a check of a client's game files says it is. What is said of a check
 * that has ended comes late, and is dropped.
 */
function checked(word: Checking) {
  const site = state.app.sites.find((known) => known.server === word.server);
  if (site && !site.diagnosis?.checking) return;
  set({ checks: [...state.checks.filter((check) => check.server !== word.server), word] });
}

/**
 * Asks the backend for everything as it stands: at the start, and when the window is looked
 * at again after a while.
 */
async function sync() {
  const heardBefore = wordsHeard;
  const { sites, ...word } = await takeSnapshot();
  // What was said while this was on its way is as new or newer.
  if (wordsHeard === heardBefore) set({ app: { ...state.app, ...word } });
  sites.forEach(heard);
  clientsKnown = true;
  settle();
}

/**
 * Shows that something other than a patch run went wrong. A reason that says no more than
 * the headline does is not said a second time.
 */
function report(title: string, reason: unknown) {
  const said = String(reason);
  set({ failure: { title, message: said === title ? null : said, details: [] }, dialog: "failed" });
}

/** Opens a page. Whatever dialog is open is closed with it, as by its own button. */
export function openPage(page: Page) {
  closeDialog();
  set({ page });
}

/** Asks the server again right away, after a check that failed. */
export function checkAgain(server: ServerId) {
  askAgain(server);
}

/** Whether a start was rejected with one of the backend's refusals, and not with something else. */
const isRefusal = (reason: unknown): reason is Refusal =>
  typeof reason === "object" && reason !== null && "kind" in reason && Array.isArray((reason as Refusal).messages);

/**
 * Waits for a start that was asked of the backend for `server`, and shows why when no run
 * was begun. The backend looks at the folder first, and decides.
 */
async function begun(server: ServerId, asked: Promise<void>) {
  try {
    await asked;
  } catch (reason) {
    // Something else than one of the backend's refusals (a plain string from Tauri, an
    // Error): the start cannot go on, said as the backend says that, with what came as the
    // reason under it.
    if (!isRefusal(reason)) {
      report("패치를 진행할 수 없습니다.", reason);
      return;
    }
    const refusal = reason;
    // A run is at work already: the button was pressed twice.
    if (refusal.kind === "busy") return;
    if (refusal.kind === "blocked") {
      set({ blocked: { server, messages: refusal.messages }, dialog: "blocked" });
      return;
    }
    // An address that is Nexon's and not among the game's own patch files is only used
    // after a yes, which the backend takes for that one address. The question is its own.
    if (refusal.kind === "ask") {
      set({
        question: { message: refusal.messages[0] ?? "", yes: () => begun(server, confirmPatchAddress(server)) },
        dialog: "ask",
      });
      return;
    }
    // Nothing was tried and nothing failed. What is in the way is the headline itself, as
    // it is when the ready screen says what is in the way.
    const [title, ...rest] = refusal.messages.join("\n").split("\n");
    set({ failure: { title, message: rest.join("\n") || null, details: [] }, dialog: "failed" });
  }
}

/** Starts the patch the page of `server` offers. */
export function start(server: ServerId) {
  return begun(server, startPatch(server));
}

/**
 * Lets the user state where the patch for `server` is. The field begins with the address
 * the backend gives for it: that of the patch the app found, or where the next would be.
 */
export function enterAddress(server: ServerId) {
  const value = state.app.sites.find((site) => site.server === server)?.patchAddress ?? "";
  set({ address: { server, value }, dialog: "address" });
}

/** Takes in what the user types into the field of the "address" dialog. */
export function typeAddress(value: string) {
  if (state.address) set({ address: { ...state.address, value } });
}

/** Starts the patch at the address in the field. The backend decides whether one is fetched from there. */
export function startAtAddress() {
  const given = state.address;
  if (!given) return;
  set({ dialog: "none" });
  return begun(given.server, startPatchAt(given.server, given.value));
}

/** Leaves a paused, failed or finished run and shows the ready screen again. */
export function backToReady(server: ServerId) {
  dismissRun(server);
}

export function pause(server: ServerId) {
  stopPatch(server);
}

/** Waits for a change of the install folder that was asked of the backend, and says so when it could not be made. */
async function folderChanged(asked: Promise<void>) {
  try {
    await asked;
  } catch (reason) {
    report("설치 경로를 변경할 수 없습니다.", reason);
  }
}

/** Lets the user pick the install folder, in a dialog the backend opens. */
export function changeFolder(server: ServerId) {
  return folderChanged(pickInstallDir(server));
}

/** Goes back from a folder the user picked to the one that is found automatically. */
export function findFolder(server: ServerId) {
  return folderChanged(resetInstallDir(server));
}

/**
 * Deletes what a run that will not be continued left in the install folder. Where the app
 * cannot make good what goes with it, that is only done after a yes: a run that left the
 * folder partly new, whose files exist nowhere else, and an apply whose record cannot be
 * read, which can no longer be undone. The backend says which folders those are.
 */
export function discard(server: ServerId) {
  if (!state.app.sites.find((site) => site.server === server)?.discardAsks) {
    discardNow(server, false);
    return;
  }
  set({
    question: {
      message: "삭제 후에는 공식 런처로 게임을 복구해야 합니다.\n이전 패치 작업을 삭제하시겠습니까?",
      yes: () => discardNow(server, true),
    },
    dialog: "ask",
  });
}

async function discardNow(server: ServerId, confirmed: boolean) {
  try {
    await discardLeftovers(server, confirmed);
  } catch (reason) {
    report("이전 패치 작업을 삭제할 수 없습니다.", reason);
  }
}

/** Copies how the finished run went to the clipboard, as text a tester can send on. */
export async function copyReport(server: ServerId) {
  const text = await runReport(server).catch(() => null);
  if (text === null || !(await copyText(text))) {
    set({ failure: { title: "측정 결과를 복사할 수 없습니다.", message: null, details: [] }, dialog: "failed" });
    return;
  }
  set({ copied: true });
  setTimeout(() => set({ copied: false }), 2000);
}

/** Has the PC checked again for `server`: what kind of PC it is, and what a patch is to expect on it. */
export function diagnose(server: ServerId) {
  diagnoseAgain(server);
}

/**
 * Has the game's files of `server` checked, once the user has said yes to it. The backend
 * words the question, with about how long the check takes on this PC. Where the check
 * cannot be begun now it has none, and nothing is asked.
 */
export async function checkFiles(server: ServerId) {
  const message = await askFileCheck(server).catch(() => null);
  if (message === null) return;
  set({ question: { message, yes: () => void startFileCheck(server) }, dialog: "ask" });
}

/** Stops the check of the game's files of `server`. What was found before it stays on the page. */
export function stopChecking(server: ServerId) {
  stopFileCheck(server);
}

/**
 * Copies the report of the PC that stands on the page for `server` to the clipboard, as
 * text to send on. Where it does not get there, the button does not say that it did.
 */
export async function copyDiagnosis(server: ServerId) {
  const report = state.app.sites.find((site) => site.server === server)?.diagnosis?.report ?? "";
  if (report === "" || !(await copyText(report))) return;
  set({ reportCopied: server });
  setTimeout(() => set({ reportCopied: null }), 2000);
}

/** Opens the page bugs are reported on. Where it does not open, nothing is said. */
export function reportBug() {
  openBugReportPage().catch(() => {});
}

export async function launch(server: ServerId) {
  if (launching) return;
  launching = true;
  try {
    await launchGame(server);
  } catch (reason) {
    report("게임 실행에 실패했습니다.", reason);
  } finally {
    launching = false;
  }
}

/** Waits for a download page that was asked of the backend, and says so when it could not be opened. */
async function pageOpened(asked: Promise<void>) {
  try {
    await asked;
  } catch (reason) {
    report("다운로드 페이지를 열 수 없습니다.", reason);
  }
}

export function download(server: ServerId) {
  return pageOpened(openDownloadPage(server));
}

/** Opens the page a newer version of this app is downloaded from. */
export function downloadUpdate() {
  return pageOpened(openReleasePage());
}

/** Turns one of the settings on or off. */
export async function setSetting(name: keyof Settings, on: boolean) {
  const before = state.app.settings[name];
  const wanted = { ...state.app.settings, [name]: on };
  set({ app: { ...state.app, settings: wanted } });
  try {
    // What was set is kept. Where it calls for a question, the backend has the page ask
    // it: whether the app is to start again with administrator rights, which starting the
    // game by itself takes. A no leaves everything as it was set.
    const question = await setSettings(wanted);
    if (question) set({ question: { message: question, yes: () => void restartWithRights() }, dialog: "ask" });
  } catch (reason) {
    // Only this one goes back: another may have been set meanwhile, and stays as it is.
    set({ app: { ...state.app, settings: { ...state.app.settings, [name]: before } } });
    report("설정을 저장할 수 없습니다.", reason);
  }
}

export function closeDialog() {
  // The question on closing the window stays open in the backend until it is answered, and
  // a window closed again over it stops the run. Put aside, it is answered: nothing changes.
  if (state.dialog === "closing") void answerClose("stay");
  set({ dialog: "none" });
}

/**
 * Answers the question on closing the window during a run: with `on` the run goes on and
 * the app is next to the clock for the time being, without it the run is stopped and the
 * app goes.
 */
export function answerClosing(on: boolean) {
  set({ dialog: "none" });
  void answerClose(on ? "background" : "quit");
}

/** Answers the "ask" dialog with a yes. */
export function agree() {
  const yes = state.question?.yes;
  set({ dialog: "none" });
  yes?.();
}

/** Once a second while a run is going: how fast its step goes, and how long it still needs. */
function tick() {
  const run = state.app.sites.find((site) => site.phase === "running")?.run ?? null;
  if (run === null || speedFrom === null) return;
  const now = Date.now();
  // The watch begins with the first work done: while files from an earlier run are still
  // being checked, nothing has been said about how fast building goes.
  const idle = pace.length > 0 && pace[0].bytes === run.worked;
  pace = idle ? [{ at: now, bytes: run.worked }] : [...pace, { at: now, bytes: run.worked }].slice(-PACE_READINGS);
  const watched = now - pace[0].at;
  const done = run.worked - pace[0].bytes;
  const left = watched >= PACE_MS && done > 0 ? Math.ceil(((run.total - run.done) * watched) / done / 1000) : null;
  // Nothing is arriving. When it does again, the speed is measured from that moment, not
  // averaged over the silence.
  if (now - heardAt > QUIET_MS) {
    speedFrom = { at: now, bytes: run.done };
    set({ speed: 0, left });
    return;
  }
  const rate = (Math.max(0, run.done - speedFrom.bytes) * 1000) / Math.max(1, now - speedFrom.at);
  speedFrom = { at: now, bytes: run.done };
  // Averaged with the reading before, so the figure does not twitch.
  set({ speed: state.speed === 0 ? rate : (state.speed + rate) / 2, left });
}

/**
 * Starts everything that runs by itself: hearing what the backend says, taking in what it
 * knows, and the one timer. Returns what stops it all again.
 *
 * The backend looks at the install, asks the server, and starts what the user wants started
 * by itself. So the screen stays current without a clock here: closing the game clears the
 * warning, and a patch that gets published while the app is open shows up.
 */
export function init() {
  const said = onSaid({ site: heard, progress: progressed, app: heardWord, checking: checked });
  // What the backend knows is asked for once what it says from now on is heard, so that
  // nothing it says in between is missed.
  said.listening.then(sync);
  // A window that was put away may have missed what was said meanwhile.
  const stopLooking = onShown(sync);
  const timer = setInterval(tick, 1000);

  return () => {
    said.stop();
    stopLooking();
    clearInterval(timer);
  };
}
