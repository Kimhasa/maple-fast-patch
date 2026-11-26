import { useEffect, useSyncExternalStore } from "react";
import { closeWindow, minimizeWindow, pageDrawn } from "./api";
import Icon from "./Icon";
import * as patcher from "./patcher";
import ServerPage from "./ServerPage";
import SettingsPage from "./SettingsPage";
import Sidebar from "./Sidebar";

/** The window's frame: the bar at the top, the sidebar, the page that is open, and a dialog over that page. */
export default function App() {
  const s = useSyncExternalStore(patcher.subscribe, patcher.getState);
  const v = patcher.view(s);
  const { update } = s.app;
  const dialogOpen = s.dialog !== "none";

  useEffect(() => patcher.init(), []);

  /** What the backend first said is on the page: the window, which waits for that, can show. */
  const drawn = s.app.sites.length > 0;
  useEffect(() => {
    if (drawn) void pageDrawn();
  }, [drawn]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") patcher.closeDialog();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <header className="top" data-tauri-drag-region>
        <span className="side lead">
          {update && (
            <button className="mini" title={update.message ?? undefined} tabIndex={-1} onClick={patcher.downloadUpdate}>
              새 버전 {update.latest}
            </button>
          )}
        </span>
        <span className="logo" data-tauri-drag-region>
          MAPLE FAST PATCH
        </span>
        <span className="side">
          <button title="최소화" tabIndex={-1} onClick={minimizeWindow}>
            <Icon name="minus" />
          </button>
          <button title="닫기" tabIndex={-1} onClick={closeWindow}>
            <Icon name="x" />
          </button>
        </span>
      </header>

      <div className="frame">
        <Sidebar sites={s.app.sites} running={s.app.running} page={v.page} />
        <div className="content">
          {v.page === "settings" ? (
            <SettingsPage settings={s.app.settings} inert={dialogOpen} />
          ) : (
            v.site && (
              <ServerPage
                key={v.site.server}
                site={v.site}
                v={v}
                settings={s.app.settings}
                copied={s.copied}
                inert={dialogOpen}
              />
            )
          )}

          {dialogOpen && (
            <div className={`dim ${v.page === "settings" ? "whole" : ""}`}>
              <div className={`dlg ${s.dialog === "address" ? "wide" : ""}`}>
                <div className="hd">
                  <b>MAPLE FAST PATCH</b>
                  <button title="닫기" onClick={patcher.closeDialog}>
                    <Icon name="x" />
                  </button>
                </div>
                <DialogBody s={s} v={v} />
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/** What a dialog says. Its main button takes the keyboard, so Enter answers it. */
function DialogBody({ s, v }: { s: patcher.State; v: ReturnType<typeof patcher.view> }) {
  if (s.dialog === "blocked") {
    return (
      <div className="bd">
        {v.inTheWay.map((message, index) => (
          <p key={message} className={index > 0 ? "why" : undefined}>
            {message}
          </p>
        ))}
        <button className="primary" onClick={patcher.closeDialog} autoFocus>
          확인
        </button>
      </div>
    );
  }
  if (s.dialog === "address" && s.address) {
    return (
      <div className="bd">
        <label className="field">
          패치 주소
          <input
            value={s.address.value}
            onChange={(event) => patcher.typeAddress(event.target.value)}
            onFocus={(event) => event.target.select()}
            onKeyDown={(event) => {
              // The key answers this dialog and no other: what it would go on to do is left
              // undone, or it would press the button of whatever dialog the answer opens.
              if (event.key !== "Enter") return;
              event.preventDefault();
              patcher.startAtAddress();
            }}
            spellCheck={false}
            autoFocus
          />
        </label>
        <div className="pair">
          <button className="primary" onClick={patcher.startAtAddress}>
            확인
          </button>
          <button className="primary second" onClick={patcher.closeDialog}>
            취소
          </button>
        </div>
      </div>
    );
  }
  if (s.dialog === "closing") {
    return (
      <div className="bd">
        <p>{"패치가 진행 중입니다.\n백그라운드에서 계속 진행하시겠습니까?"}</p>
        <div className="pair">
          <button className="primary" onClick={() => patcher.answerClosing(true)} autoFocus>
            확인
          </button>
          <button className="primary second" onClick={() => patcher.answerClosing(false)}>
            종료
          </button>
        </div>
      </div>
    );
  }
  if (s.dialog === "ask" && s.question) {
    return (
      <div className="bd">
        <p>{s.question.message}</p>
        {s.question.detail && <p className="why detail">{s.question.detail}</p>}
        <div className="pair">
          <button className="primary" onClick={patcher.agree}>
            확인
          </button>
          <button className="primary second" onClick={patcher.closeDialog} autoFocus>
            취소
          </button>
        </div>
      </div>
    );
  }
  if (!s.failure) return null;
  return (
    <div className="bd">
      <p>{s.failure.title}</p>
      {s.failure.message && <p className="why">{s.failure.message}</p>}
      {s.failure.details.map((detail) => (
        <p key={detail} className="why detail">
          {detail}
        </p>
      ))}
      <button className="primary" onClick={patcher.closeDialog} autoFocus>
        확인
      </button>
    </div>
  );
}
