import type { Settings } from "./api";
import * as patcher from "./patcher";

/** The settings in their groups, in the order the page lists them. */
const groups: [string, [keyof Settings, string][]][] = [
  [
    "패치",
    [
      ["autoPatch", "자동 패치"],
      ["immediate", "즉시 패치"],
    ],
  ],
  [
    "게임 실행",
    [
      ["autoStart", "패치 후 게임 자동 실행"],
      ["exitAfterStart", "게임 실행 후 종료"],
    ],
  ],
  [
    "알림 및 앱 동작",
    [
      ["maintenanceAlert", "점검 종료 알림"],
      ["background", "백그라운드 실행"],
    ],
  ],
];

/**
 * The settings: a switch each. A switch takes effect when it is flipped, so the page has
 * nothing to confirm and is left like any other.
 */
export default function SettingsPage({ settings, inert }: { settings: Settings; inert: boolean }) {
  return (
    <main className="settings" inert={inert}>
      <h1>설정</h1>
      {groups.map(([heading, switches]) => (
        <section key={heading}>
          <h2>{heading}</h2>
          {switches.map(([name, label]) => (
            <label className="set" key={name}>
              {label}
              <button
                className={`switch ${settings[name] ? "on" : ""}`}
                role="switch"
                aria-checked={settings[name]}
                onClick={() => patcher.setSetting(name, !settings[name])}
              >
                <i />
              </button>
            </label>
          ))}
        </section>
      ))}
    </main>
  );
}
