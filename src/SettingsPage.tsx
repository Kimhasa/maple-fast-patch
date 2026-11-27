import { type KeyboardEvent, useState } from "react";
import type { Settings } from "./types";
import * as patcher from "./patcher";

/**
 * The settings in their groups, in the order the page lists them: what each is called, and
 * under it what it does, in a line. This page is the one place that says such a line: a
 * switch has to be understood before it is flipped.
 */
const groups: [string, [keyof Settings, string, string][]][] = [
  [
    "패치",
    // Which way a patch is applied is chosen on a client's page, where the patch is started.
    [["autoPatch", "자동 패치", "주기적 확인 후 최신 패치 자동 진행"]],
  ],
  [
    "게임 실행",
    [
      ["autoStart", "패치 후 게임 자동 실행", "패치 완료 및 서버 오픈 시 게임 자동 실행"],
      ["exitAfterStart", "게임 실행 후 종료", "게임 실행 시 프로그램 자동 종료"],
    ],
  ],
  [
    "알림 및 앱 동작",
    [
      ["maintenanceAlert", "점검 상태 알림", "서버 점검 시작 및 종료 시 알림 발송"],
      ["background", "백그라운드 실행", "창 닫기 시 시스템 트레이로 최소화하여 유지"],
    ],
  ],
];

/** The group that was open last: the page comes back to it when it is opened again. */
let lastOpen = 0;

/**
 * The settings: a tab for each group, and a switch for each setting of the group that is
 * open. A switch takes effect when it is flipped, so the page has nothing to confirm and is
 * left like any other.
 */
export default function SettingsPage({ settings, inert }: { settings: Settings; inert: boolean }) {
  const [open, setOpen] = useState(lastOpen);
  const show = (group: number) => {
    lastOpen = group;
    setOpen(group);
  };
  /** The arrow keys go from tab to tab, as they do in a row of tabs anywhere. */
  const onKey = (event: KeyboardEvent) => {
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    const next = (open + step + groups.length) % groups.length;
    show(next);
    document.getElementById(`tab-${next}`)?.focus();
  };
  return (
    <main className="settings" inert={inert}>
      <div className="tabs" role="tablist" onKeyDown={onKey}>
        {groups.map(([heading], group) => (
          <button
            key={heading}
            id={`tab-${group}`}
            role="tab"
            aria-selected={group === open}
            aria-controls="tab-panel"
            tabIndex={group === open ? 0 : -1}
            onClick={() => show(group)}
          >
            {heading}
          </button>
        ))}
      </div>
      <section id="tab-panel" role="tabpanel" aria-labelledby={`tab-${open}`}>
        {groups[open][1].map(([name, label, does]) => (
          <label className="set" key={name}>
            <span>
              {label}
              <small id={`does-${name}`}>{does}</small>
            </span>
            <button
              className={`switch ${settings[name] ? "on" : ""}`}
              role="switch"
              aria-checked={settings[name]}
              aria-label={label}
              aria-describedby={`does-${name}`}
              onClick={() => patcher.setSetting(name, !settings[name])}
            >
              <i />
            </button>
          </label>
        ))}
      </section>
    </main>
  );
}
