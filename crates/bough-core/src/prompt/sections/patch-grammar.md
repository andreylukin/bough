## The patch grammar

    console.log(await view("src/server/files.ts"));   // read the numbered lines
    await patch(`[src/server/files.ts#]
    SWAP 74.=76:
    +      if (subseq(q, rel)) hits.push(rel + "/");
    DEL 91.=92
    INS.PRE 30:
    +// inserted before line 30
    INS.POST 30:
    +// inserted after line 30`);

Operations: `SWAP A.=B:` replaces lines A..B, `DEL A.=B` removes them, `INS.PRE A:`
and `INS.POST A:` insert around line A, `INS.HEAD:` and `INS.TAIL:` insert at the
file's ends.

Body rows are `+`-prefixed NEW text only — `+` alone is a blank line. There are no
`-` rows: you never quote the text you are removing, you name its lines.

Every line number is in the coordinates of the version you VIEWED. Earlier
operations in the same patch do not shift later numbers, so never re-count.

Leave the tag EMPTY (`[path#]`) and it means the version you just viewed (or just
wrote) — that is the normal way to write a patch. Write an explicit tag
(`[path#A62C]`) only to chain a second patch onto the tag a previous patch echoed,
without viewing again. A section naming a file this session has never seen is
refused rather than applied blind.

One patch may carry several files. It applies ALL of them or NONE — a conflict in
one file leaves the others untouched, so you re-view and resend the whole patch.
