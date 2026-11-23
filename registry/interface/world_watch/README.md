# World watch sensor

`world_watch` is a reusable, read-only World observation sensor driven purely by
JSON configuration. It is the external-world companion to `world_cron`: where
`world_cron` watches time and quiet periods, `world_watch` watches an HTTP/HTTPS
endpoint, a local file, or an incoming HTTP webhook (`source.kind: "webhook"`)
and emits a declared artifact (`watch.event` by default) whenever a change,
predicate match, recovery, push delivery, or probe error occurs.

Every run whose active checkpoint pins the interface folder receives the
artifact. Like `world_cron`, when `hold_while_working` is `true`, delivery waits
while any pinning run has `status: "running"` so an active turn is never
interrupted mid-work.

## Creating a watch without writing driver code

Author three files in `registry/interface/world_watch_<slug>/`:

1. `interface.json` with `"copy_of": "world_watch"`, `"interface_type": "world"`,
   `"role": "sensor"`, `"direction": "inbound"`, `"interface_driver_ref": "world_watch_life"`,
   and `"source_names": ["watch.event"]` (or the custom `label` from `probe.json`).
2. `life.json` copied from `registry/interface/world_watch/life.json`. For a
   `file` watch, set `"allow_network": false` and add the exact watched parent
   directory to `"authority.read_paths"`.
3. `probe.json` describing what to watch and when to fire:

```json
{
  "enabled": true,
  "poll_seconds": 300,
  "source": {
    "kind": "http",
    "url": "https://api.example.com/status.json",
    "method": "GET",
    "headers": {}
  },
  "extract": {
    "format": "json",
    "json_pointer": "/status"
  },
  "trigger": {
    "mode": "on_match",
    "condition": { "not_equals": "ok" },
    "edge_triggered": true,
    "notify_on_recovery": true,
    "notify_on_error": true,
    "cooldown_seconds": 600
  },
  "hold_while_working": true,
  "label": "watch.event",
  "note": "Service health left ok state."
}
```

Then pin `world_watch_<slug>` on a requirement-free checkpoint of the target
run. The sensor never creates or edits runs.

## Boundaries

- Supported sources: `http` (read-only `GET`/`HEAD` with conditional `ETag`/
  `Last-Modified`), `file` (`O_NOFOLLOW`, bounded regular file read), and
  `webhook` (local HTTP listener defaulting to `127.0.0.1`, constant-time
  `hmac_sha256` / `bearer` / `header` verification, bounded body & pending
  queue, `202 Accepted` only after durable staging).
- Credentials never live in `probe.json`: `source.auth` references either a
  `0600` file inside `OCTOPUS_VAR_DIR` (`token_file`) or an explicit inherited
  environment variable (`env`).
- Every emitted artifact sets `"untrusted_external_data": true` and clips large
  values to a bounded preview.
- Pending arrivals are stored durably in `watch.sqlite3` and reclaimed from
  staging only after confirmed ledger admission.
