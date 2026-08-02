"""Schema history.

v1: {"v":1, "name": "Ada Lovelace", "email": "a@b.c"}
v2: `name` split into `first` / `last`
v3: `email` becomes a list `emails`, primary first
v4: adds `active` (bool), defaulting True
"""

from copy import deepcopy

LOWEST, HIGHEST = 1, 4


class MigrationError(Exception):
    pass


class Migrator:
    def run(self, record, to):
        # R6: every shape error surfaces as MigrationError.
        if not isinstance(record, dict) or "v" not in record:
            raise MigrationError("record has no version")
        version = record["v"]
        if not isinstance(version, int) or not LOWEST <= version <= HIGHEST:
            raise MigrationError(f"unknown schema version: {version!r}")
        if not isinstance(to, int) or not LOWEST <= to <= HIGHEST:
            raise MigrationError(f"unknown target version: {to!r}")

        r = deepcopy(record)  # R1: nothing shared with the caller, at any depth.
        while r["v"] < to:
            r = getattr(self, f"up_{r['v']}")(r)
        while r["v"] > to:  # R3
            r = getattr(self, f"down_{r['v']}")(r)
        return r

    # -- up ----------------------------------------------------------------
    def up_1(self, r):
        name = r.pop("name", "") or ""
        first, sep, last = name.rpartition(" ")  # R2: split on the LAST space.
        r["first"], r["last"] = first, last
        r["v"] = 2
        return r

    def up_2(self, r):
        r["emails"] = [r.pop("email")]
        r["v"] = 3
        return r

    def up_3(self, r):
        r["active"] = True
        r["v"] = 4
        return r

    # -- down --------------------------------------------------------------
    def down_4(self, r):
        r.pop("active", None)
        r["v"] = 3
        return r

    def down_3(self, r):
        emails = r.pop("emails", [])
        if not emails:
            raise MigrationError("cannot downgrade a record with no emails")
        r["email"] = emails[0]
        r["v"] = 2
        return r

    def down_2(self, r):
        first = r.pop("first", "")
        last = r.pop("last", "")
        r["name"] = " ".join(p for p in (first, last) if p)
        r["v"] = 1
        return r
