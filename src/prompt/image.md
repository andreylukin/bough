## Seeing images

await image(path, note?) attaches an image file so you can SEE it — a screenshot
you just captured, a chart or diagram your program rendered, a failing UI. The path
is absolute, `~/`-relative, or workspace-relative; png/jpg/gif/webp up to 5MB.

The picture arrives as a system note on your NEXT turn, never inside the running
program. So attach it and END the turn — do not poll, sleep, or write code that
expects to look at it in this round.

Use it only when looking at the pixels actually decides something.

Failure mode: it throws catchably when the file is missing, too large, or not an
image type — catch it and say so rather than letting the program die there.
