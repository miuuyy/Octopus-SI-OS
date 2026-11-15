# GitHub activity sensor

A read-only, polling World sensor. Bind `world_github_watch` as a standing
external input accepting `github.activity.notice`. It lives only while bound;
installing the folder starts nothing. It never posts comments or changes GitHub.

## Configuration and credentials

Copy this owner folder and install its explicit `world_legacy_watch` code
dependency. `life.json` imports only that owner's private-file, hash, ledger and
stdin/stdout protocol helpers, not its mail-watch behavior. No kernel changes,
webhooks, public listener, or notifications permission are needed.

Set `github-watch.json`. The life reads `$OCTOPUS_VAR_DIR/github-watch.json`
(normally `<Home>/interfaces/world_github_watch/github-watch.json`) first and
this folder's `github-watch.json` only when no private file exists. Keep an
installation's own repository in the private file: the registry folder holds
the `OWNER/REPOSITORY` template and is rewritten by a package repair or a
rebuild, while the private state directory is not. A private file that is
unreadable or invalid stops the life; it never falls back to the template.
The configuration is read once at start, so restart the life after changing it.

- `repository`: exact `OWNER/REPOSITORY`;
- `branch`: branch whose failed workflow runs matter;
- `poll_seconds`: at least 300 (default 300, after the previous cycle);
- `ignore_comment_authors`: logins whose issue/PR comments, review comments,
  reviews, discussion comments and replies must not wake the receiver;
- `max_pages`: 100 per REST/GraphQL connection;
- `max_requests`: 500 per entire poll;
- `max_resources`: 20,000 retained resource identities;
- `timeout_seconds`: 30 per HTTP request.

This is deliberately not named `watch.json`: that filename belongs to the
legacy watch's client condition editor. The ordinary World/sensor/inbound
classification and receiving run binding make this sensor visible in Triggers.

Write a token privately to `$OCTOPUS_VAR_DIR/github.token` (mode 0600, owner
only; directory 0700). An operator may capture `gh auth token` directly into
that private file without printing it. No credential belongs in the descriptor,
source, diagnostic output, or a tool-result artifact. The life reads the token
again each cycle, so rotation needs no restart. GitHub.com is the only API
origin; redirects and ambient proxies are disabled. The token needs read access
to issues, pull requests, discussions and Actions for the selected repository.
The sensor cannot grant those permissions.

## Polling and payload

Each successful poll walks bounded, independently paginated resource lists:
- issues and pull requests, including closed resources;
- repository issue comments (also PR conversation comments) and review comments;
- each PR's submitted reviews, including closed PRs;
- discussions, each discussion's comments, and each comment's replies;
- branch workflow runs with failure, timed_out, startup_failure or action_required.

The REST issues API also returns PRs; those are observed through the pulls
endpoint instead. Each GraphQL query has just one paginated connection, avoiding
multiplicative node limits. Every child is read even when the parent's update
timestamp is unchanged. All API pages must succeed before preparing a poll.
HTTP, rate-limit, GraphQL partial errors, malformed responses, conflicting
duplicate resources and exhausted bounds preserve previous watermarks.
Retry-After and primary rate reset backoff persist across process restarts.

The first complete inventory becomes a **quiet baseline**, with no historical
notices. Future snapshots compare stable resource IDs and content fingerprints,
not a strict timestamp cutoff: equal timestamps, late-visible IDs and edits
with unchanged timestamps are retained. Per-kind timestamp watermarks include
all IDs at the tie. Old resources first appearing later with timestamps before
the baseline are silently incorporated. Deleted resources are not emitted or
forgotten.

Content changes include issue/PR title, body, state, PR head/base/draft, review
body/state, discussion title/body/answered state, and comments/replies. Labels,
reactions, comment counts and update-time-only changes are not events. Ignored
comment authors still advance stored fingerprints. Thus a maintainer's reply
cannot continually wake the steward by bumping a parent issue's timestamp.
Ignoring an account also suppresses that account's human-authored comments;
use a dedicated automation account when that distinction matters.

A notice contains up to 100 events with resource kind, stable ID, content
digest, updated time, URL, parent, author, title and state, plus repository,
branch, observation time and generation. Bodies are fingerprinted, not sent.
Titles and other external content are explicitly marked untrusted. The
receiving run should fetch the whole conversation before acting. A notice is a
snapshot change, not a claim about every intermediate GitHub event.

## Durability and health

Private state lives only in `OCTOPUS_VAR_DIR`, normally
`<Home>/interfaces/world_github_watch/`:
- `watch.sqlite3`: committed resource fingerprints/watermarks, pending proposed
  state and immutable notice identities, admission receipts and health;
- `payloads/`: content-pinned notice payloads and manifests;
- `health.json`: safe state/error codes, last successful API inventory time,
  request/resource counts, pending flag, next poll time and last admission;
- `watch.lock`: exclusive process ownership; `github.token`: private credential.

SQLite FULL synchronous transactions persist a proposed poll before hand-in.
Committed watermarks move only after every pending notice is admitted. On
refusal, retain the exact manifests and retry after the poll interval; repair
the binding/policy rather than resetting state. On a lost or malformed verdict
the process fails with pending work intact. After restart it looks up the exact
interface/artifact identity in the movement ledger before any resend, and uses
stable admission evidence to close the remaining race. A protocol failure is
never presented as a completed delivery. Replacing a repository, branch or
ignored-author policy under existing state fails closed; use a separate
interface/state for a different watch.

Health states are `watching`, `poll_failed`, `auth_unavailable`,
`delivery_refused`, `delivery_uncertain`, or `failed`. An error is local
health evidence, not a recurring notice that wakes the steward every five
minutes. Life readiness means the protocol is ready, not that GitHub has been
successfully seeded. Check `last_success_at`, `error`, and `pending`.

## Limits

Polling observes current resource state, not a webhook event log. Changes made
and reverted between polls, deleted comments, label-only edits, draft reviews,
reactions, checks outside Actions, and successful/cancelled runs are not emitted.
The full inventory favors reliable edit detection for small/medium repositories;
it costs requests proportional to all retained PR/discussion connections.
GitHub caps filtered workflow lists at 1,000: exceeding that per conclusion
fails explicitly rather than pretending a truncated inventory is complete.
Bounds and rate limits can delay all streams; health reports the specific
boundary. Raise explicit bounds when appropriate; very large repositories need
a separately designed incremental or webhook ingestion strategy.

Run the owner contracts:
```sh
python3 -m unittest discover -s registry/interface/world_github_watch/tests -p 'test_*.py'
```
