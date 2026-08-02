"""Occurrence generation."""

from calendar import monthrange
from datetime import date, timedelta


class Engine:
    def __init__(self, rule):
        self.r = rule
        if rule.count is None and rule.until is None:
            raise ValueError("a rule needs count or until")

    def run(self):
        out = []
        for day in self._candidates():
            if self.r.until is not None and day > self.r.until:  # R5: inclusive.
                break
            if day in self.r.exclude:  # R5: an exclusion does not consume count.
                continue
            out.append(day)
            if self.r.count is not None and len(out) >= self.r.count:
                break
        return out

    def _candidates(self):
        if self.r.freq == "daily":
            return self._daily()
        if self.r.freq == "weekly":
            return self._weekly()
        if self.r.freq == "monthly":
            return self._monthly()
        raise ValueError(self.r.freq)

    def _limit(self):
        """A hard bound so a rule with only `until` still terminates."""
        return 4000

    def _daily(self):
        day = self.r.start
        for _ in range(self._limit()):
            yield day
            day += timedelta(days=self.r.interval)

    def _weekly(self):
        # R3: ascending by date, whatever order byday came in.
        days = sorted(self.r.byday) if self.r.byday else (self.r.start.isoweekday(),)
        monday = self.r.start - timedelta(days=self.r.start.isoweekday() - 1)
        for _ in range(self._limit()):
            for wd in days:
                day = monday + timedelta(days=wd - 1)
                if day < self.r.start:  # R3: the first week is truncated at start.
                    continue
                yield day
            monday += timedelta(weeks=self.r.interval)

    def _monthly(self):
        y, m = self.r.start.year, self.r.start.month
        wanted = self.r.start.day
        for _ in range(self._limit()):
            # R4: skip, never clamp. The phase advances either way.
            if wanted <= monthrange(y, m)[1]:
                yield date(y, m, wanted)
            m += self.r.interval
            while m > 12:
                m -= 12
                y += 1
