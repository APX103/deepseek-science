# Local A2A fixture

Start two deterministic specialists on different loopback ports:

```bash
python3 scripts/a2a_fixture_server.py --port 18931 --name "Fast Reactor Reviewer" --specialty "Generation IV sodium fast reactors"
python3 scripts/a2a_fixture_server.py --port 18932 --name "Materials Reviewer" --specialty "irradiation-resistant reactor materials" --mode direct
```

Configure `http://127.0.0.1:18931` and `http://127.0.0.1:18932` in the App. The standard
Agent Card is served at `/.well-known/agent-card.json`; the JSON-RPC interface is `/a2a`.

Inspect counters without reading request contents:

```bash
curl http://127.0.0.1:18931/_control/state
```

`card_gets` proves save-time and mandatory pre-invocation discovery. `sends` proves a mutating
message was not replayed; `polls` proves every received Task snapshot can be rendered/restored.
Modes `error`, `malformed_card`, `malformed_response`, and `slow` exercise failure boundaries.
