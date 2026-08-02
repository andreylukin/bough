"""The scanner. Whitespace separates tokens and is never itself a token."""


class Scanner:
    def __init__(self, text):
        self.text = text

    def _kind(self, chunk):
        if chunk[0].isdigit():
            return "number"
        if chunk[0].isalpha():
            return "word"
        return "punct"

    def run(self):
        from .api import Token

        out = []
        i = 0
        text = self.text
        while i < len(text):
            if text[i].isspace():
                i += 1
                continue
            j = i
            while j < len(text) and not text[j].isspace():
                j += 1
            chunk = text[i:j]
            out.append(
                Token(
                    kind=self._kind(chunk),
                    text=chunk,
                    start=i,
                    end=j,
                    bstart=i,
                    bend=j,
                )
            )
            i = j
        return out
