# Time and inactivity triggers

`world_cron` is the clock interface. When its condition holds it sends one
`cron.current_time` artifact, and every run whose checkpoint pins the folder
receives it. A requirement-free checkpoint is a standing route: one binding
receives every firing, and the interface never edits or creates a run. A
firing that comes due while a run pinning the folder is working (its record's
`status` is `running`) waits until no pinning run works, then is sent once;
ticks missed meanwhile are not replayed.

A time trigger is a copy of this folder: `interface.json` with
`copy_of: "world_cron"`, `life.json` and an `activation.json` whose `cron_life`
config holds only the schedule. The schedule selects exactly one condition:
`interval_seconds`, `cron`, `due_at`, or `inactivity`. A disabled copy that no
run binds is a template; packages ship templates such as `world_system_review`.
The client projects triggers in **Triggers** together with the tasks that pin
them.

The descriptor binds `timezone_database_root` to the system timezone database
through `interface_driver_config.host_slots`; copies keep that binding.

A cron schedule steps through UTC minutes in its time zone. In a repeated
fall-back hour a fixed time runs once, while wildcard minutes and hours keep
firing through both occurrences; a fixed time skipped by a spring-forward jump
runs at the first minute after the jump.

An inactivity condition for user-facing tasks is:

```json
{
  "inactivity": {
    "seconds": 3600,
    "interface_match": {
      "interface_type": "world",
      "role": "user",
      "interface_driver_ref": "app_server"
    },
    "exclude_interface_match": {
      "interface_driver_ref": "cron_life"
    }
  },
  "fire_on_start": false,
  "timezone": "UTC",
  "reminder_env": "",
  "reminder_default": "Review recent work and make a useful improvement if warranted."
}
```

For ordinary qualifying runs, every artifact movement with typed run provenance
counts, regardless of label. A run qualifies when one of its checkpoint bindings
names an interface whose descriptor matches every selector field. The optional
`within_ee` narrows this scope to runs whose EE chain contains that EE.

Runs that bind this trigger or have received an arrival from it are excluded,
and so are runs that bind or were fed by an interface matching the optional
`exclude_interface_match`; an arrival keeps the exclusion after the binding is
retired. In these runs, only `InterfaceArrival` movements from a source
interface matching `interface_match` count. Thus a user's reply inside a review
task starts a new quiet period, but the review's output and tool traffic do not.
With the `cron_life` exclusion, time triggers cannot wake each other forever.
Other sensors and passive background runs do not postpone a user-task quiet
period. An unreadable run record contributes no activity and is named on stderr.

The driver folds `movements.jsonl` once at startup, then reads new complete rows
and reparses run/interface descriptors only when their file metadata changes.
Missing initial activity does not manufacture a deadline. A quiet period that
elapsed while the interface was stopped fires on restart. Polling resolution is
five seconds; the deadline uses the ledger's `recorded_at_unix_ms`.

Each quiet episode delivers one `cron.current_time` artifact with
`schedule_kind: "inactivity"`, its due time, last activity's run and sequence,
`activity_window` (the ledger sequences after the previous delivery through the
last activity) and the configured prompt. `cron-inactivity-state.json` records
preparation and the exact admitted movement. Restart recovers admission even
after a lost acknowledgement. Another delivery requires new qualifying
activity. Activity before the deadline replaces it; once the deadline has
passed the prepared artifact is delivered, and later activity opens the next
episode. Missing admission evidence, a truncated ledger or malformed state
stops the interface explicitly.

Editing the prompt or duration preserves the consumed activity sequence.
Pausing stops the interface; resuming catches up on the same ledger history.
