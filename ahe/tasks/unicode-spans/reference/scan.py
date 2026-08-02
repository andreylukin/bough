"""The scanner. Whitespace separates tokens and is never itself a token."""

import unicodedata


class Scanner:
    def __init__(self, text):
        self.text = text

    @staticmethod
    def _cls(ch):
        if ch.isdigit():
            return "number"
        # R5: a combining mark continues the letter it sits on.
        if ch.isalpha() or unicodedata.category(ch).startswith("M"):
            return "word"
        return "punct"

    def run(self):
        from .api import Token

        text = self.text
        # R4: one prefix table, so a byte offset is a lookup rather than a guess.
        blen = [0]
        for ch in text:
            blen.append(blen[-1] + len(ch.encode("utf-8")))

        out = []
        i = 0
        while i < len(text):
            if text[i].isspace():  # R2
                i += 1
                continue
            cls = self._cls(text[i])
            if cls == "punct":
                j = i + 1  # R1: one punctuation character per token.
            else:
                j = i
                while (
                    j < len(text)
                    and not text[j].isspace()
                    and self._cls(text[j]) == cls
                ):
                    j += 1
            out.append(
                Token(
                    kind=cls,
                    text=text[i:j],
                    start=i,
                    end=j,
                    bstart=blen[i],
                    bend=blen[j],
                )
            )
            i = j
        return out
