import type { ButtonKind, Row, ServerId, Settings, SiteView } from "./api";
import Icon from "./Icon";
import * as patcher from "./patcher";

const megabytes = (bytes: number) => `${Math.round(bytes / (1 << 20)).toLocaleString()} MB`;
/** Below this a download speed would read "0.0 MB/s", which says nothing; it is left out. */
const SHOWN_SPEED = 0.05 * (1 << 20);

/**
 * A time still to go, as roughly as it is known. Files are built at speeds that differ
 * several times over, so the figure is rounded up to ten seconds and says that it is about.
 */
const duration = (seconds: number) =>
  seconds >= 55 ? `약 ${Math.max(1, Math.round(seconds / 60))}분` : `약 ${Math.max(10, Math.ceil(seconds / 10) * 10)}초`;
const two = (part: number) => String(part).padStart(2, "0");
/** A moment as its date and time of day, the date as Nexon's board writes one: `2026.11.26 20:08`. */
const stamp = (at: number) => {
  const on = new Date(at);
  const day = `${on.getFullYear()}.${two(on.getMonth() + 1)}.${two(on.getDate())}`;
  return `${day} ${two(on.getHours())}:${two(on.getMinutes())}`;
};

/** The angular marks on the main button: play, pause, and the arrow of a download. */
const marks = {
  play: "M6 3v18l15-9z",
  pause: "M5 3h5v18H5zM14 3h5v18h-5z",
  download: "M10 2h4v8h4l-6 7-6-7h4zM5 19h14v3H5z",
};

function Mark({ shape }: { shape: keyof typeof marks }) {
  return (
    <svg className="play" viewBox="0 0 24 24" aria-hidden="true">
      <path d={marks[shape]} />
    </svg>
  );
}

/**
 * The two ways a patch is applied, as the page offers them: whether it is the immediate one,
 * what it is called, and what the pointer is told of it. The usual way builds every new file
 * first and can be taken back; the immediate one replaces files as it builds them.
 */
const ways: [boolean, string, string?][] = [
  [false, "일반 패치"],
  [true, "즉시 패치", "여유 공간이 부족할 때 사용합니다. 중단하면 되돌릴 수 없습니다."],
];

type Props = {
  site: SiteView;
  v: ReturnType<typeof patcher.view>;
  settings: Settings;
  /** How the finished run went was just copied. */
  copied: boolean;
  /** A dialog is over the page: nothing on it takes the pointer or the keyboard. */
  inert: boolean;
};

/** The page of one client, as the backend says it: what is known of the game, the run, and the one button. */
export default function ServerPage({ site, v, settings, copied, inert }: Props) {
  const { server, phase, run, lamp, percent } = site;
  /** A run that has ended is on the page: stopped before it was done, or finished. */
  const ended = phase === "paused" || phase === "failed" || phase === "done";
  /** A folder that is partly new is finished the way it was begun, whatever is chosen. */
  const immediate = site.halfPatched || settings.immediate;

  return (
    <>
      <main className="stage" inert={inert}>
        <h1>{site.title}</h1>
        {v.gameFolder && (
          <>
            <p className="ver">
              현재 버전: <b>{site.current ?? "-"}</b>
            </p>
            <p className="ver">
              최신 버전: <b>{site.latest ?? "-"}</b>
              {site.latest && site.checkedAt !== null && <span className="at">{stamp(site.checkedAt)} 확인</span>}
            </p>
            {lamp && (
              <p className="ver">
                서버 상태:{" "}
                <b className={lamp.open ? "open" : "closed"}>
                  {lamp.open ? "접속 가능" : !lamp.season ? "운영 기간 아님" : lamp.extended ? "연장 점검 중" : "점검 중"}
                </b>
                {lamp.until && <span className="at">{lamp.until} 종료 예정</span>}
              </p>
            )}
          </>
        )}

        {phase === "ready" ? (
          <div className={`rows ${v.tightRows ? "tight" : ""}`}>
            {v.listedRows.map((row) => (
              <div key={row.kind}>
                <span className={`check ${row.ok === null ? "" : row.ok ? "ok" : "bad"}`}>
                  <Icon name={row.ok === null ? "minus" : row.ok ? "check" : "alert"} />
                  {row.label}
                </span>
                <b className={[row.ok === false && "bad", row.kind === "folder" && "path"].filter(Boolean).join(" ")}>
                  <RowValue row={row} site={site} outdated={v.outdated} />
                </b>
              </div>
            ))}
            {v.gameFolder && (
              <div>
                <span className={`check ${v.wayRefused ? "bad" : "plain"}`}>
                  {v.wayRefused && <Icon name="alert" />}
                  패치 방식
                </span>
                <b className={v.wayRefused ? "bad" : undefined}>
                  {v.wayRefused?.detail}
                  <span className="seg" role="radiogroup" aria-label="패치 방식">
                    {ways.map(([way, label, hint]) => (
                      <button
                        key={label}
                        role="radio"
                        aria-checked={way === immediate}
                        title={hint}
                        disabled={site.halfPatched}
                        onClick={() => patcher.setSetting("immediate", way)}
                      >
                        {label}
                      </button>
                    ))}
                  </span>
                </b>
              </div>
            )}
          </div>
        ) : (
          <p className="activity">{site.summary ?? run?.activity}</p>
        )}
      </main>

      <div className="line">
        <i style={{ width: `${percent ?? 0}%` }} />
      </div>

      <footer className="foot" inert={inert}>
        <div className="st">
          <b className={percent === null ? "none" : undefined}>{percent === null ? "-" : `${Math.floor(percent)}%`}</b>
          <span className={`status ${site.statusBad ? "bad" : ""}`}>
            {site.busy && (
              <i className="spin">
                <Icon name="loader" />
              </i>
            )}
            {site.status}
          </span>
        </div>
        {ended ? (
          <div className="sz">
            {phase === "done" && (
              <button className="mini" onClick={() => patcher.copyReport(server)}>
                {copied ? "복사 완료" : "측정 결과 복사"}
              </button>
            )}
            <button className="mini" onClick={() => patcher.backToReady(server)}>
              처음으로
            </button>
          </div>
        ) : (
          phase !== "ready" &&
          run !== null &&
          run.total > 0 && (
            <div className="sz">
              {v.downloadSpeed !== null && v.downloadSpeed >= SHOWN_SPEED && (
                <span className="rate">{(v.downloadSpeed / (1 << 20)).toFixed(1)} MB/s</span>
              )}
              {v.remaining !== null && <span className="rate">남은 시간 {duration(v.remaining)}</span>}
              <span>{megabytes(run.done)}</span>
              <i>|</i>
              <em>{megabytes(run.total)}</em>
            </div>
          )
        )}
        <MainButton site={site} />
      </footer>
    </>
  );
}

/** What a line of the ready screen says at its right, and what it offers there. */
function RowValue({ row, site, outdated }: { row: Row; site: SiteView; outdated: boolean }) {
  const { server } = site;
  if (row.kind === "leftover") {
    return (
      <>
        {row.detail}
        <button className="mini" onClick={() => patcher.discard(server)}>
          삭제
        </button>
      </>
    );
  }
  if (row.kind === "patchFile") {
    return (
      <>
        {row.detail}
        <button className="mini" onClick={() => patcher.enterAddress(server)} disabled={outdated || site.locked !== null}>
          주소 입력
        </button>
      </>
    );
  }
  return (
    <>
      <span className="clip" title={row.detail}>
        {row.detail}
      </span>
      {row.kind === "folder" && (
        <>
          <button className="mini" onClick={() => patcher.changeFolder(server)}>
            폴더 변경
          </button>
          {site.customDir && (
            <button className="mini" onClick={() => patcher.findFolder(server)}>
              게임 찾기
            </button>
          )}
        </>
      )}
    </>
  );
}

/** What the one button at the bottom right is called, its mark, and what a press does, for each thing it can be. */
const buttons: Record<ButtonKind, { label: string; mark?: keyof typeof marks; press: (server: ServerId) => void }> = {
  install: { label: "게임 다운로드", press: patcher.download },
  checkAgain: { label: "다시 시도", press: patcher.checkAgain },
  launch: { label: "게임 실행", press: patcher.launch },
  gameRunning: { label: "게임 실행 중", press: patcher.launch },
  download: { label: "다운로드", mark: "download", press: patcher.start },
  resume: { label: "이어하기", mark: "play", press: patcher.start },
  retry: { label: "다시 시도", mark: "play", press: patcher.start },
  pause: { label: "일시정지", mark: "pause", press: patcher.pause },
};
/** The buttons that start a patch. */
const starts: ButtonKind[] = ["download", "resume", "retry"];

/**
 * The one button at the bottom right. Which it is follows from where things stand, and the
 * backend says so. While another client's run is at work, no patch can be started here: a
 * button that would start one is off until that run has ended.
 */
function MainButton({ site }: { site: SiteView }) {
  const { kind, enabled, primary } = site.button;
  const { label, mark, press } = buttons[kind];
  const held = site.locked !== null && starts.includes(kind);
  return (
    <button
      className={`act ${primary && !held ? "go" : ""}`}
      onClick={() => press(site.server)}
      disabled={!enabled || held}
    >
      {mark && <Mark shape={mark} />}
      {label}
    </button>
  );
}
