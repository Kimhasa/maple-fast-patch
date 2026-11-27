import { Fragment, type KeyboardEvent, useState } from "react";
import type { Checking, ServerId, SiteView } from "./types";
import * as patcher from "./patcher";
import { clients } from "./Sidebar";

/** The clients in the order of their tabs: the live game first. */
const order: ServerId[] = ["live", "test"];

/** The client whose tab was open last: the page comes back to it when it is opened again. */
let lastOpen: ServerId = "live";

type Props = {
  sites: SiteView[];
  /** How far each check of a client's game files is that is at work. */
  checks: Checking[];
  /** The client whose report was just copied. */
  copied: ServerId | null;
  /** A dialog is over the page: nothing on it takes the pointer or the keyboard. */
  inert: boolean;
};

/** A line of the report: what it is about, and what is said of it. A line that goes on from the one above it has no name. */
type Row = { name: string; said: string };
/** A part of the report under its heading. What stands before the first heading has none. */
type Part = { title: string; rows: Row[] };

/**
 * The report as its parts. It is text made to be passed on, in columns kept by spaces: a
 * heading in brackets, and under it lines of a name, two spaces or more, and what is said
 * of it. A line that begins with spaces goes on from the one above it.
 */
function partsOf(report: string): Part[] {
  const parts: Part[] = [];
  for (const line of report.split("\n")) {
    const title = /^\[(.+)\]$/.exec(line);
    if (title) parts.push({ title: title[1], rows: [] });
    else if (line.trim() !== "") {
      if (parts.length === 0) parts.push({ title: "", rows: [] });
      const [, name = "", said = line.trim()] = /^(\S.*?) {2,}(.*)$/.exec(line) ?? [];
      parts[parts.length - 1].rows.push({ name, said });
    }
  }
  return parts;
}

/**
 * The report on the page. Spaces keep no column in the letters of the app, so the names
 * stand in a column of their own here, as wide as the longest of them. What is copied is
 * the text itself, not what is drawn here.
 */
function Report({ text }: { text: string }) {
  const parts = partsOf(text);
  const stamp = parts.find((part) => part.title === "");
  const shown = parts.filter((part) => part.title !== "");
  return (
    <div className="report">
      {stamp && <p>{stamp.rows.map((row) => row.said).join(" ")}</p>}
      {shown.map((part) => (
        <Fragment key={part.title}>
          <h3>{part.title}</h3>
          {part.rows.map((row, at) => (
            <Fragment key={at}>
              <span>{row.name}</span>
              <b>{row.said}</b>
            </Fragment>
          ))}
        </Fragment>
      ))}
    </div>
  );
}

/**
 * What kind of PC this is, as the diagnosis of it says: a tab for each client that has an
 * install folder, and for the one that is open the verdict's answers, each with the one
 * line said for why, and what can be done about it. The whole report is there to be read
 * once it is asked for. The backend words all of it, the names of the answers too; with no
 * folder to look at there is nothing to show, and the page is empty.
 */
export default function DiagnosisPage({ sites, checks, copied, inert }: Props) {
  const [open, setOpen] = useState(lastOpen);
  const show = (server: ServerId) => {
    lastOpen = server;
    setOpen(server);
  };
  /** The whole report was asked for. It is put away again when the page is left: the page begins without it. */
  const [detailed, setDetailed] = useState(false);
  const tabs = order.flatMap((server) => sites.filter((site) => site.server === server && site.diagnosis !== null));
  /** The client whose tab is open: the one that was chosen, while it still has a folder. */
  const site = tabs.find((site) => site.server === open) ?? tabs[0];
  if (!site?.diagnosis) return <main className="diagnosis" inert={inert} />;
  const { server } = site;
  const { working, checking, canCheck, answers, report } = site.diagnosis;
  /** The report is on the page. Until there is one, there is nothing to show of it. */
  const unfolded = detailed && report !== "";
  /** How far the check of the game's files is. Until it has said so, nothing of the bar is filled. */
  const read = checks.find((check) => check.server === server);
  const percent = read && read.total > 0 ? Math.min(100, (read.done / read.total) * 100) : 0;
  /** One button is for the check of the game's files: it asks for the check, and stops the one at work. */
  const checkOrStop = () => (checking ? patcher.stopChecking(server) : patcher.checkFiles(server));
  /** The arrow keys go from tab to tab, as they do in a row of tabs anywhere. */
  const onKey = (event: KeyboardEvent) => {
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    const next = tabs[(tabs.indexOf(site) + step + tabs.length) % tabs.length].server;
    show(next);
    document.getElementById(`tab-${next}`)?.focus();
  };
  return (
    <main className="diagnosis" inert={inert}>
      <div className="head">
        <div className="tabs" role="tablist" onKeyDown={onKey}>
          {tabs.map((tab) => (
            <button
              key={tab.server}
              id={`tab-${tab.server}`}
              role="tab"
              aria-selected={tab === site}
              aria-controls="tab-panel"
              tabIndex={tab === site ? 0 : -1}
              onClick={() => show(tab.server)}
            >
              {clients[tab.server].label}
            </button>
          ))}
        </div>
        <div className="tools">
          <button className="mini" onClick={() => patcher.diagnose(server)} disabled={working || checking}>
            다시 검사
          </button>
          <button className="mini" onClick={checkOrStop} disabled={!checking && !canCheck}>
            {checking ? "취소" : "파일 검사"}
          </button>
          <button className="mini" onClick={() => patcher.copyDiagnosis(server)} disabled={report === ""}>
            {copied === server ? "복사 완료" : "복사"}
          </button>
          <button className="mini" onClick={patcher.reportBug}>
            버그 제보
          </button>
        </div>
      </div>
      <section
        id="tab-panel"
        role="tabpanel"
        aria-labelledby={`tab-${server}`}
        className={unfolded ? "detailed" : undefined}
      >
        <div className="result">
          {answers.length > 0 && (
            <div className="answers">
              {answers.map(({ title, answer, reason, tone }) => (
                <div key={title}>
                  <span>{title}</span>
                  <b className={tone || undefined}>{answer}</b>
                  {reason && <small>{reason}</small>}
                </div>
              ))}
            </div>
          )}
          <button
            className="mini"
            aria-expanded={unfolded}
            onClick={() => setDetailed(!detailed)}
            disabled={report === ""}
          >
            {unfolded ? "접기" : "상세 보기"}
          </button>
        </div>
        {unfolded && <Report text={report} />}
        {checking && (
          <div className="checking">
            <p className="activity">검사 중</p>
            <div className="line">
              <i style={{ width: `${percent}%` }} />
            </div>
          </div>
        )}
      </section>
    </main>
  );
}
