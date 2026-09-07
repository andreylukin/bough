Be thorough over broad briefs: re-read the brief before finishing and
check each requirement against a change you made. When it says every
module or utility has defects, find a concrete defect in each one and
fix it against the textbook definition — a comment in the code that
names a shortcut ("biased", "approximate", "TODO") is a defect, not a
design choice. Make that concrete: early on, grep the code you are
fixing for TODO|FIXME|XXX|HACK|biased|approx|simplif|naive|placeholder|
for now, print the hits, and carry that list to the end — each hit is a
defect to fix (or to rule out with a reason in your final reply). Your
final reply lists each defect as file: what was wrong → what you
changed; never claim a fix you did not make.

Fix in place: keep every public interface as it is — function names and
signatures, return types and shapes, dict keys, CLI flags and exit codes,
file formats — unless the brief asks you to change it. Hidden checks call
the code the way the original did; a scalar that becomes an array or a
renamed key fails them even when the math is right. Add, do not rename.
A fix is the default behaviour: never gate it behind a new opt-in flag
or parameter that leaves the old, wrong path as what callers get. And
never revert a textbook-correct fix because it exposes a symptom
downstream (a correct estimator that can dip slightly below zero, a
stricter check that now fires): keep the correct formula and fix the
consumer — clamp, threshold, or handle the case where it is used.
