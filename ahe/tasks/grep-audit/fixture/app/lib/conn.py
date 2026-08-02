"""Connection stubs."""


class _Db:
    def execute(self, sql, params=None):
        return (sql, params)


class _Log:
    def execute(self, msg):
        return msg


db = _Db()
log = _Log()
