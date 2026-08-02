"""Schema history.

v1: {"v":1, "name": "Ada Lovelace", "email": "a@b.c"}
v2: `name` split into `first` / `last`
v3: `email` becomes a list `emails`, primary first
v4: adds `active` (bool), defaulting True
"""


class MigrationError(Exception):
    pass


class Migrator:
    def run(self, record, to):
        r = dict(record)
        while r["v"] < to:
            r = getattr(self, f"up_{r['v']}")(r)
        return r

    def up_1(self, r):
        first, last = r["name"].split(" ")
        del r["name"]
        r["first"], r["last"] = first, last
        r["v"] = 2
        return r

    def up_2(self, r):
        r["emails"] = [r["email"]]
        del r["email"]
        r["v"] = 3
        return r

    def up_3(self, r):
        r["active"] = True
        r["v"] = 4
        return r
