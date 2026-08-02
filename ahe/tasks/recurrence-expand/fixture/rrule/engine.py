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
        seen = 0
        for day in self._candidates():
            if self.r.until is not None and day >= self.r.until:
                break
            if self.r.count is not None and seen >= self.r.count:
                break
            seen += 1
            if day in self.r.exclude:
                continue
            out.append(day)
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
        days = self.r.byday or (self.r.start.isoweekday(),)
        monday = self.r.start - timedelta(days=self.r.start.isoweekday() - 1)
        for _ in range(self._limit()):
            for wd in days:
                yield monday + timedelta(days=wd - 1)
            monday += timedelta(weeks=self.r.interval)

    def _monthly(self):
        y, m = self.r.start.year, self.r.start.month
        wanted = self.r.start.day
        for _ in range(self._limit()):
            last = monthrange(y, m)[1]
            yield date(y, m, min(wanted, last))
            m += self.r.interval
            while m > 12:
                m -= 12
                y += 1
