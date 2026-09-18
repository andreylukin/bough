import type { Meta, StoryObj } from "@storybook/react-vite";
import { PortalPane } from "../portal";
import type { OrbPortal, Row } from "../types";
import { rows } from "./fixtures";

// PortalPage reads the session's orb over the API, so the stories stub
// fetch and let the page's own request path (and its failure branch)
// run. The stub is installed at module scope: a decorator's effect runs
// after its children mount, which is too late for the request the page
// fires on its first render.
let answer: OrbPortal[] | "fail" = [];
const real = window.fetch;
window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(typeof input === "string" ? input : input instanceof URL ? input : input.url);
  if (!url.includes("/orb")) return real(input as RequestInfo, init);
  if (answer === "fail") return new Response("nope", { status: 500 });
  return new Response(
    JSON.stringify({ orb: { session: "s", project: "webshop", status: "running", updatedAt: "", portals: answer } }),
    { status: 200, headers: { "Content-Type": "application/json" } },
  );
};

const project: Row = { ...rows.find((r) => r.mode === "project")!, title: "Try the checkout flow" };
const local: Row = { ...rows.find((r) => r.mode === "local")!, title: "Rename the scratch dir" };

const meta: Meta<typeof PortalPane> = { title: "Sessions/Portal", component: PortalPane };
export default meta;
type S = StoryObj<typeof PortalPane>;

const frame = (portals: OrbPortal[] | "fail", row = project): S => ({
  args: { row, onClose: () => {} },
  render: (args) => {
    answer = portals; // set before the pane mounts and fetches
    // The pane is a flex child of .app in the real layout; the story
    // stands in a row of the same height so its height rule applies.
    return (
      <div style={{ height: "100vh", display: "flex" }}>
        <div style={{ flex: 1, minWidth: 0 }} />
        <PortalPane key={JSON.stringify(portals)} {...args} />
      </div>
    );
  },
});

// about:blank stands in for a dev server: a story should not need one
// running. Point url at a real portal to see the orb's own page here.
export const Open: S = frame([{ host: 53108, guest: 3000, name: "web", url: "about:blank", live: true }]);

export const SeveralPorts: S = frame([
  { host: 53108, guest: 3000, name: "web", url: "about:blank", live: true },
  { host: 53109, guest: 8080, name: "api", url: "about:blank", live: true },
]);

// The session that opened it has exited: the record is still here, the
// listener is not.
export const Dead: S = frame([{ host: 53108, guest: 3000, name: "web", url: "http://127.0.0.1:53108", live: false }]);

export const NoPortalYet: S = frame([]);
export const LocalSession: S = frame([], local);
export const Unreachable: S = frame("fail");
