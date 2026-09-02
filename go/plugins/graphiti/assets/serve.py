#!/usr/bin/env python3
"""bough graphiti launcher: embedded FalkorDB (falkordblite) + the stock Graphiti MCP server.

Written by `bough graphiti install`; run by launchd (com.bough.graphiti). Env, all
optional: GRAPHITI_HOME (state dir), GRAPHITI_PORT (MCP http port), FALKORDB_PORT
(embedded redis port), GRAPHITI_LLM (openrouter | openai), MODEL_NAME,
EMBEDDER_MODEL, GRAPHITI_GROUP_ID. API keys come from $BOUGH_HOME/env (default
~/.bough/env), the same file the bough server sources.
"""
import atexit
import os
import signal
import sys
from pathlib import Path

home = Path(os.environ.get("GRAPHITI_HOME", Path.home() / ".bough" / "graphiti"))
envfile = Path(os.environ.get("BOUGH_HOME", Path.home() / ".bough")) / "env"
if envfile.exists():
    for line in envfile.read_text().splitlines():
        line = line.strip()
        if line.startswith("export "):
            line = line[7:]
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))

src = home / "src" / "mcp_server"
sys.path.insert(0, str(src / "src"))
os.chdir(src)

# The graph: an embedded redis-server with the FalkorDB module, one file, this
# process only. That is why exactly one launcher runs and every bough talks to
# it over http instead of spawning its own.
db_port = os.environ.get("FALKORDB_PORT", "6399")
from redislite.falkordb_client import FalkorDB  # noqa: E402

_db = FalkorDB(str(home / "graph.db"), serverconfig={"port": db_port, "bind": "127.0.0.1"})
os.environ["FALKORDB_URI"] = f"redis://127.0.0.1:{db_port}"


# The redis must die with this process: launchd's SIGTERM (bough graphiti
# stop) bypasses atexit, and redislite only stops a server it started itself,
# so an orphan on FALKORDB_PORT would be silently adopted by the next start.
def _shutdown(*_):
    try:
        _db.client.execute_command("SHUTDOWN")  # saves the rdb first
    except Exception:  # noqa: BLE001 - the connection drops as it obeys
        pass
    os._exit(0)


signal.signal(signal.SIGTERM, _shutdown)
atexit.register(_shutdown)
os.environ.setdefault("GRAPHITI_GROUP_ID", "bough")

# OpenRouter serves both chat and /embeddings on an OpenAI-compatible surface,
# so one key covers extraction and embedding. Structured output is json_object:
# json_schema through OpenRouter fails OpenAI's strict-schema check.
llm = os.environ.get("GRAPHITI_LLM", "openrouter")
if llm == "openrouter":
    os.environ["OPENAI_API_URL"] = "https://openrouter.ai/api/v1"
    os.environ["OPENAI_API_KEY"] = os.environ.get("OPENROUTER_API_KEY", "")
    os.environ.setdefault("LLM_STRUCTURED_OUTPUT_MODE", "json_object")
    os.environ.setdefault("MODEL_NAME", "openai/gpt-5-mini")
    os.environ.setdefault("EMBEDDER_MODEL", "openai/text-embedding-3-small")
elif llm == "openai":
    os.environ.setdefault("MODEL_NAME", "gpt-5-mini")
    os.environ.setdefault("EMBEDDER_MODEL", "text-embedding-3-small")
else:
    sys.exit(f"GRAPHITI_LLM={llm!r}: want openrouter or openai")

port = os.environ.get("GRAPHITI_PORT", "8621")
argv = ["graphiti", "--config", str(home / "config.yaml"), "--transport", "http",
        "--host", "127.0.0.1", "--port", port]
if m := os.environ.get("MODEL_NAME"):
    argv += ["--model", m, "--small-model", m]
if m := os.environ.get("EMBEDDER_MODEL"):
    argv += ["--embedder-model", m]
sys.argv = argv + sys.argv[1:]
from graphiti_mcp_server import main  # noqa: E402

main()
