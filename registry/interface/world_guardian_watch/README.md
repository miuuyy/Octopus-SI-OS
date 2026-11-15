# Guardian watch

A World sensor that notices when one run goes quiet. It folds the movement
ledger and remembers the last artifact the watched run's provider interface
emitted for that run: a tool call, a message, a result. That is the run acting.
When the run has emitted nothing of its own for longer than `quiet_seconds`,
the sensor hands in one `guardian.quiet_alert`; the receiving run decides what
to do about it.

It differs from the `world_cron` inactivity schedule in what counts as life.
The inactivity schedule is postponed by arrivals, so a run that keeps receiving
heartbeats or tool results looks active. Here a delivery **to** the watched run
is evidence of demand, never of life: only its own outbound production resets
the quiet period.

Bind `world_guardian_watch` as a standing external input accepting the
configured `notice_label`. It lives only while bound; installing the folder
starts nothing. It reads the ledger and, when granted, the watched run's
record; it writes only its private state and never changes a run.

## Configuration

The life reads `$OCTOPUS_VAR_DIR/guardian_config.json` (normally
`<Home>/interfaces/world_guardian_watch/guardian_config.json`) first and this
folder's `guardian_config.json` only when no private file exists. Keep an
installation's own configuration in the private file: the registry folder
holds a template and is rewritten by a package repair or a rebuild, while the
private state directory is not. A private file that is unreadable or invalid
stops the life at start; it never falls back to the template. The packaged
template names `RUN_ID` and `PROVIDER_INTERFACE_ID` and is refused for the same
reason: a sensor watching a run that does not exist would report a silence that
means nothing.

The file is read again when it changes. A changed file that is not usable is
logged and the previous configuration stays in force. Changing the watched run
or provider folds the ledger again from its start.

Every key is required:

- `schema_version`: `1`;
- `watched_run`: the exact run id;
- `provider_interface`: the provider interface that speaks for that run's model;
- `quiet_seconds`: silence after which the first alert is handed in;
- `repeat_cooldown_seconds`, `max_repeat_cooldown_seconds`: the same silence is
  reported again after the cooldown, which doubles with every report up to the
  maximum;
- `refused_retry_seconds`: wait before an alert the runtime refused is prepared
  again;
- `poll_seconds`: how often the ledger tail is read;
- `own_action_labels`: provider artifact labels that count as the run acting
  (at least one);
- `error_labels`: provider artifact labels recorded as its last error;
- `heartbeat_labels`: labels of deliveries reported as heartbeats;
- `notice_label`: the label of the alert artifact;
- `instructions`: the text handed to the receiving run as `task`.

## Alert payload

`trigger` (the event sentence, the watched run and provider, the threshold and
the own-action labels), `observed_at`, `quiet_seconds`, `last_own_action`,
`last_provider_emission`, `last_provider_error`,
`delivered_since_last_own_action` (counts of inputs and tool results and the
most recent twelve), `last_heartbeat_delivery`, `run_record` (status, EE and
checkpoint counts when `run_records_root` is granted, otherwise the reason it
is not readable), `ledger` (tail position and age, unreadable rows passed
over), `alert` (number, repeat index and the wait before the next report) and
`task`.

## State and durability

Private state lives only in `OCTOPUS_VAR_DIR`:

- `guardian-state.json`: the ledger cursor, the folded observations, the count
  of admitted alerts and the current alert with its status (`prepared`,
  `admitted` or `refused`);
- `payloads/`: the content-pinned payload and manifest of the one pending alert.

An alert is written and recorded as `prepared` before it is handed in. After a
restart the life looks the exact artifact up in the ledger before handing it
again, and a duplicate verdict is accepted only with that ledger evidence. A
refusal is recorded with its reason and retried after `refused_retry_seconds`.
A ledger that changed identity or shrank, a saved state of another shape and a
changed watch all fold the ledger again from its start. The life's stderr is
capped by the host, so it speaks only on transitions.

## Limits

One folder watches one run through one provider interface; watch another run
with a copy of the folder and its own private state. Silence is measured from
ledger rows, so a run that works for a long time inside one provider call
without emitting an own-action label looks quiet: choose `own_action_labels`
and `quiet_seconds` for the provider. The sensor reports; it does not restart,
interrupt or repair anything.

Run the owner contracts:

```sh
python3 -m unittest discover -s registry/interface/world_guardian_watch/tests -p 'test_*.py'
```
