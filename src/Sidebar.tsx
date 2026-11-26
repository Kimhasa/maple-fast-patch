import type { ServerId, SiteView } from "./api";
import Icon, { type IconName } from "./Icon";
import * as patcher from "./patcher";

/** What a client is called in the sidebar, and its mark there. */
const clients: Record<ServerId, { label: string; icon: IconName }> = {
  live: { label: "본섭", icon: "monitor" },
  test: { label: "테섭", icon: "flask" },
};

type Props = {
  sites: SiteView[];
  /** The client a run is at work for. */
  running: ServerId | null;
  /** The page that is open. */
  page: patcher.Page | null;
};

/**
 * The way from page to page: an item for each client the backend has, in its order, and the
 * settings at the foot.
 *
 * An item also says what a page that is not open would: a lock where another client's run
 * keeps this one from starting, and how far a run has come while its own page is not the
 * one open.
 */
export default function Sidebar({ sites, running, page }: Props) {
  return (
    <nav className="sidebar">
      {sites.map((site) => {
        const { label, icon } = clients[site.server];
        const open = site.server === page;
        return (
          <button
            key={site.server}
            className="nav"
            aria-current={open ? "page" : undefined}
            onClick={() => patcher.openPage(site.server)}
          >
            <Icon name={icon} />
            {label}
            {site.locked !== null && (
              <span className="lock">
                <Icon name="lock" />
              </span>
            )}
            {running === site.server && !open && (
              <i className="going">
                <b style={{ width: `${site.percent ?? 0}%` }} />
              </i>
            )}
          </button>
        );
      })}
      <button
        className="nav last"
        aria-current={page === "settings" ? "page" : undefined}
        onClick={() => patcher.openPage("settings")}
      >
        <Icon name="settings" />
        설정
      </button>
    </nav>
  );
}
