Add a `stats` command to this toolkit, following the existing command conventions: create commands/stats.py and register it in app.py. `python3 app.py stats FILE` reads a file containing one number per line and prints exactly three lines:

```
min 1
max 10
mean 5.0
```

(shown for a file whose numbers are 10, 4, 7, 1, 3 — mean is the arithmetic mean printed with Python's default float formatting). Existing commands must keep working unchanged.
