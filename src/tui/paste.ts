/**
 * Long pastes, and WHERE IN THE DRAFT they belong.
 *
 * A paste over `QUEUE_ABOVE_CHARS` is not inlined — a 400-line stack trace in the
 * composer buries the sentence being written and pushes the transcript off the
 * screen. It is held aside and shown as one compact row instead (`Composer`'s
 * `[Pasted text #1]`), which is the part that already worked.
 *
 * What did not work is that the held text was appended to the END of the message at
 * submit, in the order it was pasted. "Compare `<paste>` with `<paste>` and explain
 * the difference" came out as the two sentences first and both pastes afterwards, in
 * an order the user could not influence and had no way to see. Position is meaning:
 * the model reads the message as prose, and prose whose subjects are all shifted to
 * the end says something else.
 *
 * So a paste leaves a MARK where the cursor was, and the mark is what the draft
 * carries: `[Pasted text #1]`, the same text the chip row shows. Three properties
 * follow from making the draft the record rather than a parallel list of offsets,
 * and all three are why it is a mark and not a remembered index:
 *
 *   - **Edits cannot desynchronize it.** Typing before a mark, rewrapping a
 *     sentence, moving a clause — an offset would need patching after every one of
 *     them; a mark just moves with the text it sits in.
 *   - **Deleting the mark drops the paste.** That is the removal gesture, and it
 *     needs no key, no selection and no explanation: what the draft says is what
 *     gets sent.
 *   - **Order is the draft's order.** Cutting `[Pasted text #2]` and putting it
 *     ahead of `#1` sends them that way, because expansion follows the marks and not
 *     the queue.
 *
 * An ordinal is a stable NAME, never a position: it is the paste's index in the
 * queue and it never changes, so deleting #1's mark leaves #2 still reading `#2` in
 * both the draft and the chip row. A mark naming a paste that does not exist — one
 * the user typed themselves — is left exactly as written, because inventing a
 * substitution for it would be worse than the literal text they asked for.
 *
 * PURE. Strings in, strings out: no React, no state, no terminal.
 */

/**
 * Above this many characters a paste is held aside instead of inlined.
 *
 * Low on purpose. The cost of holding a paste is one row and a mark; the cost of
 * inlining one is a composer the user cannot see past, and that asymmetry starts
 * well before a paste is what anyone would call large.
 */
export const QUEUE_ABOVE_CHARS = 50;

/** The mark a held paste leaves in the draft. Matches the chip row's own label. */
export function pasteMark(ordinal: number): string {
  return `[Pasted text #${ordinal}]`;
}

/**
 * Global, because a draft may hold several — and the same one more than once.
 *
 * There is deliberately no "which pastes does this draft refer to" helper beside it:
 * a held paste has no chip row of its own, because the mark in the draft IS the row.
 * One label, in the place the user put it.
 */
const MARK = /\[Pasted text #(\d+)\]/g;

/**
 * The message as it will actually be sent: every mark replaced by its paste.
 *
 * A paste nobody refers to is dropped — its mark was deleted, which is how a held
 * paste is thrown away. A mark with no paste behind it is left verbatim; see the
 * module header.
 */
export function expandPastes(text: string, pastes: readonly string[]): string {
  return text.replace(MARK, (whole, digits: string) => pastes[Number(digits) - 1] ?? whole);
}
