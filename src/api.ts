// What the page asks of the backend, and how it hears what the backend says. In a plain
// browser nothing is asked of it: the stand-in screens answer.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  browserAsks,
  browserAsksToCheck,
  browserChecking,
  browserReport,
  browserStart,
  browserStartAt,
  browserView,
  inApp,
} from "./standins";
import type { AppView, AppWord, Checking, Progress, ServerId, Settings, SiteView } from "./types";

/** Everything the screen shows, as it stands: for the start, and for a window that was put away. */
export const takeSnapshot = () => (inApp ? invoke<AppView>("snapshot") : Promise.resolve(browserView));
/**
 * Has the backend keep what the user has set. Resolves to a question for the page to ask,
 * where what was set calls for one, and to `null` otherwise. The backend asks one when
 * 패치 후 게임 자동 실행 is switched on in an app that runs without administrator rights:
 * whether the app is to start again with them. What was set is kept whatever the answer.
 */
export const setSettings = (settings: Settings) =>
  inApp ? invoke<string | null>("set_settings", { settings }) : Promise.resolve(browserAsks(settings.autoStart));
/**
 * Starts the app again with administrator rights: the user said yes to the question the
 * backend had the page ask. Windows asks once more, in a prompt of its own, and the app
 * leaves when its other copy is started. A no there changes nothing, and nothing is said.
 */
export const restartWithRights = () => (inApp ? invoke<void>("restart_with_rights") : Promise.resolve());
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
 * Has the PC checked again for `server`: what kind of PC it is, and what is to be expected
 * of a patch on it. The check begins once nothing works in the install folder.
 */
export const diagnoseAgain = (server: ServerId) =>
  inApp ? invoke<void>("diagnose_again", { server }) : Promise.resolve();
/**
 * What the user is to be asked before the game's files of `server` are checked, with about
 * how long that takes on this PC. Resolves to `null` when the check cannot be begun now: the
 * page then asks nothing.
 */
export const askFileCheck = (server: ServerId) =>
  inApp ? invoke<string | null>("ask_file_check", { server }) : Promise.resolve<string | null>(browserAsksToCheck);
/** Begins the check of the game's files of `server`: the user said yes to being asked. */
export const startFileCheck = (server: ServerId) =>
  inApp ? invoke<void>("start_file_check", { server }) : Promise.resolve();
/** Stops the check of the game's files of `server`. What was found before it stays as it was. */
export const stopFileCheck = (server: ServerId) =>
  inApp ? invoke<void>("stop_file_check", { server }) : Promise.resolve();
/** Opens the page bugs are reported on. Which page that is, the backend knows. */
export const openBugReportPage = () => (inApp ? invoke<void>("open_bug_report_page") : Promise.resolve());

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
 * as it goes, of what is not of one client, and of a check of a client's game files as it
 * goes. `listening` resolves once nothing that is said can be missed any more, and `stop`
 * ends the subscription.
 */
export function onSaid(heard: {
  site: (site: SiteView) => void;
  progress: (progress: Progress) => void;
  app: (word: AppWord) => void;
  checking: (word: Checking) => void;
}) {
  if (!inApp) {
    // Nothing is said in a browser. A stand-in screen on which the game's files are being
    // checked says once how far that is, and it stays there.
    if (browserChecking) heard.checking(browserChecking);
    return { listening: Promise.resolve(), stop: () => {} };
  }
  const subscriptions = [
    listen<SiteView>("site", (event) => heard.site(event.payload)),
    listen<Progress>("progress", (event) => heard.progress(event.payload)),
    listen<AppWord>("app", (event) => heard.app(event.payload)),
    listen<Checking>("checking", (event) => heard.checking(event.payload)),
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
