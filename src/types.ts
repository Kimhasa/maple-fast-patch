// What the backend tells the page. The backend decides what the screen says; the page draws it.

/** Which client: the live game, or the test world. */
export type ServerId = "live" | "test";

type Step = "download" | "verifyPatch" | "verifyInstall" | "trial" | "rebuild" | "apply";

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
  /**
   * A line of kind `way` is shown on the page's own line for the choice of the way of patching.
   * One of kind `diagnosis` says what kind of PC this is, where it is no ordinary one.
   */
  kind: "folder" | "running" | "writable" | "space" | "recovery" | "way" | "leftover" | "patchFile" | "diagnosis";
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
export type Run = {
  /**
   * The window was closed on the run, and the user is to be asked whether it is to go on
   * with the app next to the clock. Until the backend has their answer.
   */
  asking: boolean;
  step: Step | null;
  /** Bytes done and total within the step, and how many of them this run worked for itself. */
  done: number;
  total: number;
  worked: number;
  /** What the run is working on, as in "생성 중: Map001.wz". */
  activity: string;
  failure: Failure | null;
};

/**
 * One answer of a diagnosis's verdict, in the backend's words: what is asked, the answer, and
 * the one line said for why. `reason` is empty where the answer rests on nothing to say.
 */
export type Answer = {
  title: string;
  answer: string;
  reason: string;
  /**
   * How the answer is to be taken, which is the colour it is written in: as good news, as
   * something to know of, or as neither.
   */
  tone: "good" | "caution" | "";
};

/**
 * What the diagnosis of the PC says for one client: what kind of PC this is, whether a main
 * patch is expected to go through on it, and whether what a patch wrote stays after a restart.
 */
export type Diagnosis = {
  /** A check of the PC is at work. */
  working: boolean;
  /** The check of the game's files is at work. How far it is, the page hears as it goes. */
  checking: boolean;
  /** That check can be asked for now. */
  canCheck: boolean;
  /** The verdict's three answers. Empty until a check of the PC has ended. */
  answers: Answer[];
  /** The whole report as text, for showing and for copying. Empty until then. */
  report: string;
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
  /** What the diagnosis of the PC says for this client; `null` where it has no install folder. */
  diagnosis: Diagnosis | null;
};

/** What is not of one client: what the user has set, word of a newer version, and whose run is at work. */
export type AppWord = { settings: Settings; update: Update | null; running: ServerId | null };

export type AppView = AppWord & { sites: SiteView[] };

/** Where a run is, as it says up to a dozen times a second. */
export type Progress = {
  server: ServerId;
  percent: number;
  step: Step | null;
  done: number;
  total: number;
  worked: number;
  activity: string;
};

/** How far the check of a client's game files is, in bytes read of those there are to read. */
export type Checking = { server: ServerId; done: number; total: number };

/**
 * Why a run was not begun. The first of `messages` is the one to lead with. With `address`
 * the address the user gave is not one a patch is fetched from, or no patch is there. With
 * `ask` it is Nexon's and not among the game's own patch files: the one message is the
 * question, and the run waits for the user's yes.
 */
export type Refusal = {
  kind: "blocked" | "busy" | "nothing" | "address" | "ask";
  messages: string[];
};

/** What the settings are before the backend has said what the user set. */
export const defaultSettings: Settings = {
  autoPatch: false,
  maintenanceAlert: true,
  autoStart: false,
  exitAfterStart: false,
  background: false,
  immediate: false,
};
