`tok/` splits text into tokens and reports where each one came from. It is wrong on
both counts. Fix it.

## Spec

1. **A token is a run of one class.** The classes are `word` (letters), `number`
   (digits) and `punct` (anything else that is not whitespace). A token ends when
   the class changes, not only at whitespace: `hello,world` is three tokens
   (`hello`, `,`, `world`) and `abc123` is two (`abc`, `123`). Each punctuation
   character is its own token — `?!` is two.
2. **Whitespace is never a token** and always separates. Any Unicode whitespace
   counts, not just ASCII space.
3. **`start`/`end` are code-point offsets**, end-exclusive, such that
   `text[start:end] == token.text` for every token.
4. **`bstart`/`bend` are UTF-8 byte offsets**, end-exclusive, such that
   `text.encode("utf-8")[bstart:bend].decode("utf-8") == token.text`. On ASCII the
   two systems coincide; on anything else they do not, and copying one into the
   other is the bug you are looking for.
5. **A code point is the unit** — no grapheme clustering. An emoji outside the BMP
   is one code point and four bytes. Letters outside ASCII are still letters and
   digits outside ASCII are still digits, and a **combining mark** (Unicode
   category `M`) continues the `word` it sits on rather than starting a `punct`
   token — so decomposed `e` + U+0301 is one token, not three.

Tokens come back in order of appearance.

## Constraints

- `tok/api.py` is the published surface and is **protected**: do not modify it.
- `test_tok.py` is the checked-in test suite. It must still pass.
