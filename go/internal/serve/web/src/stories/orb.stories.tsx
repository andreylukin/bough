import type { Meta, StoryObj } from "@storybook/react-vite";
import { useEffect, useState } from "react";
import { ProjectOrb } from "../orb";
import type { OrbDetail } from "../types";
import { projects } from "./fixtures";

const noop = () => {};
const hoursAgo = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
const project = projects[1];

const detail: OrbDetail = {
  project,
  files: {
    "project.yml": "repos:\n  - path: ~/repos/bough\n    branch: main\nchecks:\n  fast: cd go && go vet ./...\n  full: cd go && go test ./...\ncaches:\n  - /root/.cache/go-build\n",
    Dockerfile: "",
    "setup.sh": "apt-get update && apt-get install -y golang git\n",
    "resume.sh": "",
  },
  hash: "3f9a2c71d0be",
  orb: { slug: "bough", image: "bough-orb/bough:3f9a2c71d0be", built: true, build: "ok" },
  build: { tag: "bough-orb/bough:3f9a2c71d0be", hash: "3f9a2c71d0be", state: "ok", startedAt: hoursAgo(3), endedAt: hoursAgo(2.9) },
  orbs: [
    { session: "20260913-1412-a81f3c", project: "bough", status: "running", container: "bough-orb-20260913-1412-a81f3c", updatedAt: hoursAgo(1) },
    { session: "20260912-0930-77be02", project: "bough", status: "stopped", container: "bough-orb-20260912-0930-77be02", updatedAt: hoursAgo(20) },
  ],
  runtime: { name: "apple", available: true },
};

const meta: Meta<typeof ProjectOrb> = {
  title: "Projects/ProjectOrb",
  component: ProjectOrb,
  args: { project, log: "", onAttach: noop, onDetach: noop, onSave: async () => {}, onBuild: noop, onStopOrb: noop, onOpen: noop, onRetry: noop },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><div className="scroll proj-body"><section className="proj"><Story /></section></div></div>],
};
export default meta;
type S = StoryObj<typeof ProjectOrb>;

export const NoOrb: S = { args: { project: projects[0] } };
export const Editing: S = { args: { detail } };
export const Building: S = {
  args: {
    detail: { ...detail, orb: { ...detail.orb, built: false, build: "building" }, build: { ...detail.build, state: "building", endedAt: undefined }, orbs: [] },
    log: "#1 [internal] load build definition from Dockerfile\n#2 FROM docker.io/library/debian:bookworm\n#3 RUN sh /bough-setup/setup.sh\n#3 12.4 Get:1 http://deb.debian.org/debian bookworm InRelease [151 kB]\n#3 18.0 Setting up golang-1.22-go (1.22.2-2) ...\n",
  },
};
const failedArgs: S["args"] = {
    detail: {
      ...detail, orb: { ...detail.orb, built: false, build: "failed" },
      build: { ...detail.build, state: "failed", error: "exit status 100" },
      orbs: [{ session: "20260913-1412-a81f3c", project: "bough", status: "failed", error: "resume.sh: exit 1", updatedAt: hoursAgo(1) }],
      runtime: { name: "apple", available: false, error: "run `container system start`" },
    },
  log: "#3 RUN sh /bough-setup/setup.sh\n#3 2.1 E: Unable to locate package golang-9\nERROR: process \"/bin/sh -c sh /bough-setup/setup.sh\" did not complete successfully: exit code: 100\n",
};

export const Failed: S = { args: failedArgs };

// The orb page polls, so it re-renders under the user constantly. The
// build log used to take `open` straight from the build state, and every
// one of those renders forced a log the user had closed back open. This
// story re-renders on a timer so a close that does not stick shows up.
export const FailedRerendering: S = {
  render: (args) => {
    const [, tick] = useState(0);
    useEffect(() => {
      const t = setInterval(() => tick((n) => n + 1), 300);
      return () => clearInterval(t);
    }, []);
    return <ProjectOrb {...args} />;
  },
  args: failedArgs,
};

/* The verdict a healthy orb reads: Ready with no mark, a build time and who is inside. */
export const Ready: S = { args: { detail } };

/* No image yet: a resting verdict, so the word stands alone — no grey glyph, no "Never" build. */
export const NeverBuilt: S = {
  args: { detail: { ...detail, orb: { ...detail.orb, image: "", built: false, build: "" }, build: { tag: "", hash: "", state: "" }, orbs: [] } },
};

/* An older good image behind a failed build: amber, sessions still start. */
export const StaleImage: S = {
  args: { detail: { ...detail, build: { ...detail.build, state: "failed", error: "exit status 100" } } },
};

/* The read itself failed: one error voice with Retry and the verbatim server error. */
export const ReadFailed: S = { args: { detail: undefined, error: "orb: dial unix /var/run/container.sock: connect: connection refused" } };

/* Preflight failures carry the two things that fix them; the legacy proxy warning is a callout. */
export const PreflightFailing: S = {
  args: {
    detail: {
      ...detail,
      preflight: [
        { kind: "runtime", name: "apple", status: "ok" },
        { kind: "clone", name: "bough", status: "fail", detail: "no such remote" },
        { kind: "secret", name: "GH_TOKEN", status: "fail", detail: "`op read` returned nothing" },
      ],
      orbs: [{ ...detail.orbs[0], proxyAuth: "legacy" }],
    },
  },
};
