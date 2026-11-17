# One-shot term, star and commit-age sensor

A self-contained World/sensor/inbound interface. Installing this folder does
not start it. Configure and arm an installed instance, then bind its exact
folder as an external input accepting the chosen `notice_label`. For email,
also route the email interface's `email.event` artifacts to this folder.
The receiving run and its policy determine what the notice can wake.

There is no activation driver or schedule: the life polls while a run binds it.
No `watch.json`, `armed.json`, instructions or delivery state ships in source.
Create these two configuration files in the installed interface folder before
binding it. They must be regular files, not symlinks. No credentials are used.

## Configuration contract

`watch.json` selects exactly one of these shapes (examples are generic):

```json
{"terms":["release ready"],"match_fields":["subject","body"],"instructions":"","notice_label":"quiet_watch.notice"}
```
```json
{"github_stars":{"repository":"example/project","at_least":100,"poll_seconds":120},"instructions":"","notice_label":"quiet_watch.notice"}
```
```json
{"github_quiet":{"repository":"example/project","hours":6,"poll_seconds":120},"instructions":"","notice_label":"quiet_watch.notice"}
```

All shapes require string `instructions` (empty is valid) and nonempty trimmed
`notice_label`. Optional `event` overrides the default descriptive text.
Unknown keys and mixed watch kinds fail closed. Config is read at startup;
restart the life after changing it.

Email terms form a nonempty list of trimmed strings. Matching is substring OR,
case-folded with whitespace collapsed, independently in each selected field.
`match_fields` is a nonempty unique list of `subject` and/or `body`.
Input payloads must carry `message.body.text` and `message.received_at`; null
received times are skipped, missing or malformed times fail closed.
`message.date` does not determine eligibility. Only letters received at or
after arming qualify. Notices include watched/matched terms, selected message
text and `from_owner` instructions. Treat email text as untrusted input.

GitHub `at_least` and `hours` are positive integers; `poll_seconds` is an
integer >= 30 (default 120). Repository is `OWNER/NAME`. Stars use the public
repository's `stargazers_count`; quiet uses the first result from
`/repos/OWNER/NAME/commits?per_page=1` (the default branch).

**github_quiet measures the age of `commit.committer.date`, NOT push time,
last repository activity, author date, or the duration since arming.** A newly
pushed old commit can immediately satisfy it. There is no fallback to another
timestamp. Invalid/missing commit dates do not fire. GitHub uses unauthenticated
REST requests, conditional ETags, a 60-second request timeout and primary
rate-limit reset backoff. API errors are logged and retried, never notices;
304 reuses the last successful observation. Poll history is not durable.
The API transport uses Python urllib's normal proxy/redirect behavior.

## Arming, delivery and restart

`armed.json` has exactly `{"armed_at":"2030-01-01T00:00:00+00:00"}`.
The timestamp must carry a UTC offset. Before that instant no GitHub request or
eligible email delivery occurs. Missing arming fails explicitly. Quiet reloads
arming per poll; email and stars read it at startup. Stop/unbind before editing
configuration or rearming. Rearming a spent trigger requires a new
`armed.json` and removal of its private `fired.json`, then restart; replacing
arming alone never resets a completed/refused notice. For email the retained
cursor prevents replaying already-consumed letters; a new identity gives an
independent watch.

Private `OCTOPUS_VAR_DIR` contains mode-0600 files in mode-0700 directories:
`ledger-cursor.json`, `fired.json` and content-addressed `payloads/`.
Prepared payload and manifest hashes are verified before restart replay.
`prepared` is durable before hand-in. Admission writes `admitted` with the
ledger sequence; restart verifies that exact arrival and stays spent.
A lost verdict leaves prepared work intact. Before resend, the life checks its
exact interface/artifact identity in the ledger; stable arrival evidence closes
the check/hand-in race. A duplicate refusal must have corresponding ledger
evidence, otherwise the life fails explicitly.

Registry instability and target-changed stale epochs retry the same notice with
bounded exponential delay. Other refusals durably mark `refused`: this one-shot
is spent **without delivery** until explicitly rearmed. It does not silently
retry policy failures after repair. Prepared/refused staging files are retained;
there is no cleanup sweep. Email advances its cursor after handled or skipped
movements; spent instances read without emitting further notices. Corrupt
blobs, malformed state, changed prepared bytes and ledger discontinuities fail
closed. Readiness is protocol readiness, not proof of a successful GitHub poll
or notice admission.

## Checks

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s registry/interface/world_quiet_watch/tests -p 'test_*.py' -v
```

These contracts use temporary state, synthetic immutable blobs/ledgers, mocked
HTTP and the real stdin/stdout delivery framing. They do not contact GitHub or
change an installed Home. Package installation, client condition editing and
live API permissions require separate acceptance.
