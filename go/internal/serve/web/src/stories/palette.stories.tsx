import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { Palette, type Command } from "../palette";
import { rows } from "./fixtures";

/**
 * ⌘K. The palette is the only surface that has to answer two different
 * questions from one box — "take me to a thing that exists" and "start
 * a thing that does not" — so the stories below are mostly about which
 * one it decides you meant.
 */
const commands: Command[] = [
  { id: "new:here", group: "Start", label: "New conversation", hint: "~", run: () => {} },
  { id: "new:project", group: "Start", label: "New project…", run: () => {} },
  { id: "go:sessions", group: "Go to", label: "Conversations", run: () => {} },
  { id: "go:projects", group: "Go to", label: "Projects", run: () => {} },
  { id: "go:hooks", group: "Go to", label: "Hooks", run: () => {} },
  { id: "s:context", group: "This conversation", label: "Show what is shaping this conversation",
    hint: "Context", run: () => {} },
  { id: "s:stop", group: "This conversation", label: "Stop this turn", run: () => {} },
];

/** Storybook cannot type into the box for us, so the query is seeded. */
function Open({ query, start }: { query: string; start?: boolean }) {
  const [open, setOpen] = useState(true);
  return (
    <div style={{ height: 460 }}>
      <Palette
        key={query}
        open={open}
        onClose={() => setOpen(true)}
        rows={rows}
        commands={commands}
        onOpenSession={() => {}}
        onStart={start === false ? undefined : () => {}}
        initialQuery={query}
      />
    </div>
  );
}

const meta: Meta<typeof Open> = { title: "Palette", component: Open };
export default meta;
type S = StoryObj<typeof Open>;

/** Empty: commands only. Listing every session would be the sidebar again. */
export const Empty: S = { args: { query: "" } };

/** A word or two is a search — and still offers to start, last. */
export const OneWord: S = { args: { query: "hello" } };

/**
 * Four words or more reads as something to say rather than something to
 * find, so starting leads and the full-text search does not run. Typing
 * a sentence used to return the whole history, because every term has
 * to appear somewhere and "to" and "the" appear in all of them.
 */
export const ASentenceOffersToStart: S = {
  args: { query: "add retries to the deploy script" },
};

/** Nothing matched, and there is nowhere to start from. */
export const NoMatch: S = { args: { query: "zzzzz", start: false } };
