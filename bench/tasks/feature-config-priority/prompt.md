linefmt currently uses only compiled-in defaults (settings.py). Add layered configuration resolution and a `config` command.

Precedence, lowest to highest: built-in defaults < config file < environment < command-line flags.

- Config file: `.linefmtrc` in the current directory, if present. One `key=value` per line (value = everything after the first `=`); blank lines are ignored.
- Environment: `LINEFMT_WIDTH` and `LINEFMT_PREFIX`.
- Flags: `--width N` and `--prefix S`, given before the command name, e.g. `python3 cli.py --width 30 render FILE`.

The keys are `width` (integer) and `prefix` (string). All commands (`render`, `count`) must use the resolved settings.

New command: `python3 cli.py config` prints the resolved settings, one per line, as `key=value`, sorted by key.

Two details, exactly as specified:

1. A flag passed with an empty value counts as set: `--prefix ''` overrides any prefix from the environment or config file with the empty string.
2. If `.linefmtrc` contains a key other than `width` or `prefix`, print exactly `linefmt: unknown key: NAME` (NAME = the offending key) to stderr and exit with code 2. This applies to every command.

Behavior with no `.linefmtrc`, no `LINEFMT_*` variables, and no flags must stay exactly as it is today. Do not change test_linefmt.py.
