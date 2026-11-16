"""Bounded, read-only GitHub resource inventories. No notifications permission needed."""
import datetime as dt
import hashlib
import json
import re
import time
import urllib.error
import urllib.parse
import urllib.request

LABEL = "github.activity.notice"
COMMENT_KINDS = {"issue_comment", "review_comment", "review", "discussion_comment", "discussion_reply"}
FAILURES = ("failure", "timed_out", "startup_failure", "action_required")


class PollError(RuntimeError):
    def __init__(self, code, retry_at=None):
        self.code, self.retry_at = code, retry_at
        super().__init__(code)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def timestamp(value):
    if not isinstance(value, str):
        raise PollError("invalid_resource_timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            raise ValueError()
        return parsed.astimezone(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    except ValueError:
        raise PollError("invalid_resource_timestamp") from None


def validate_config(c):
    required = {"schema_version", "repository", "branch", "poll_seconds", "ignore_comment_authors"}
    optional = {"max_pages", "max_requests", "max_resources", "timeout_seconds"}
    if not isinstance(c, dict) or required - c.keys() or c.keys() - required - optional:
        raise ValueError("invalid github-watch.json keys")
    if c["schema_version"] != 1 or not isinstance(c["repository"], str) or not re.fullmatch(
            r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", c["repository"]):
        raise ValueError("invalid repository or schema_version")
    if not isinstance(c["branch"], str) or not 1 <= len(c["branch"]) <= 200:
        raise ValueError("invalid branch")
    if not isinstance(c["ignore_comment_authors"], list) or any(
            not isinstance(a, str) or not re.fullmatch(r"[A-Za-z0-9_\[\]-]{1,100}", a)
            for a in c["ignore_comment_authors"]):
        raise ValueError("invalid ignore_comment_authors")
    c = dict(c)
    for key, default, low, high in [
        ("poll_seconds", 300, 300, 86400), ("max_pages", 100, 1, 1000),
        ("max_requests", 500, 1, 5000), ("max_resources", 20000, 1, 100000),
        ("timeout_seconds", 30, 1, 120),
    ]:
        value = c.setdefault(key, default)
        if type(value) is not int or not low <= value <= high:
            raise ValueError("invalid " + key)
    return c


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise PollError("github_redirect_refused")


class API:
    def __init__(self, config, token, opener=None):
        self.c, self.token = config, token
        self.opener = opener or urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect())
        self.requests = 0

    def request(self, path, data=None):
        self.requests += 1
        if self.requests > self.c["max_requests"]:
            raise PollError("request_budget_exceeded")
        if not path.startswith("/") or path.startswith("//"):
            raise PollError("invalid_api_path")
        request = urllib.request.Request("https://api.github.com" + path,
            data=None if data is None else canonical(data),
            headers={"Authorization": "Bearer " + self.token,
                     "Accept": "application/vnd.github+json",
                     "Content-Type": "application/json",
                     "User-Agent": "octopus-github-watch",
                     "X-GitHub-Api-Version": "2022-11-28"})
        try:
            with self.opener.open(request, timeout=self.c["timeout_seconds"]) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as error:
            retry = None
            try:
                if error.headers.get("Retry-After"):
                    retry = time.time() + min(86400, max(300, int(error.headers["Retry-After"])))
                elif error.headers.get("X-RateLimit-Remaining") == "0":
                    retry = max(time.time() + 300, float(error.headers["X-RateLimit-Reset"]))
            except (ValueError, TypeError, KeyError):
                pass
            raise PollError("github_http_" + str(error.code), retry) from None
        except (OSError, urllib.error.URLError, TimeoutError):
            raise PollError("github_transport_failed") from None
        if len(raw) > 8 * 1024 * 1024:
            raise PollError("response_byte_limit")
        try:
            value = json.loads(raw)
        except (ValueError, UnicodeDecodeError):
            raise PollError("invalid_api_json") from None
        if isinstance(value, dict) and value.get("errors"):
            raise PollError("github_graphql_errors")
        return value

    def rest(self, path, params=None, collection=None):
        params = dict(params or {}, per_page=100)
        for page in range(1, self.c["max_pages"] + 1):
            value = self.request(path + "?" + urllib.parse.urlencode(dict(params, page=page)))
            if collection:
                if not isinstance(value, dict) or type(value.get("total_count")) is not int:
                    raise PollError("invalid_rest_collection")
                if value["total_count"] > 1000:
                    raise PollError("workflow_result_limit")
                value = value.get(collection)
            if not isinstance(value, list) or len(value) > 100 or any(not isinstance(v, dict) for v in value):
                raise PollError("invalid_rest_page")
            yield from value
            if len(value) < 100:
                return
        raise PollError("rest_page_limit")

    def connection(self, query, variables, route):
        cursor, seen = None, set()
        for _ in range(self.c["max_pages"]):
            value = self.request("/graphql", {"query": query, "variables": dict(variables, cursor=cursor)})
            try:
                connection = value["data"]
                for part in route:
                    connection = connection[part]
                nodes, info = connection["nodes"], connection["pageInfo"]
                if not isinstance(nodes, list) or len(nodes) > 100 or any(not isinstance(n, dict) for n in nodes):
                    raise ValueError()
                if type(info["hasNextPage"]) is not bool:
                    raise ValueError()
                yield from nodes
                if not info["hasNextPage"]:
                    return
                cursor = info["endCursor"]
                if not isinstance(cursor, str) or not cursor or cursor in seen:
                    raise ValueError()
                seen.add(cursor)
            except (KeyError, TypeError, ValueError):
                raise PollError("invalid_graphql_page") from None
        raise PollError("graphql_page_limit")


PAGE = "pageInfo { hasNextPage endCursor }"
DISCUSSIONS = """query($owner:String!,$repo:String!,$cursor:String) {
 repository(owner:$owner,name:$repo) {
 discussions(first:100,after:$cursor,orderBy:{field:UPDATED_AT,direction:DESC}) {
 nodes { id number title body url createdAt updatedAt isAnswered author { login } }
 """ + PAGE + " } } }"
COMMENTS = """query($id:ID!,$cursor:String) {
 node(id:$id) { ... on Discussion { comments(first:100,after:$cursor) {
 nodes { id body url createdAt updatedAt author { login } }
 """ + PAGE + " } } } }"
REPLIES = """query($id:ID!,$cursor:String) {
 node(id:$id) { ... on DiscussionComment { replies(first:100,after:$cursor) {
 nodes { id body url createdAt updatedAt author { login } }
 """ + PAGE + " } } } }"


def resource(kind, row, parent=None, graph=False):
    try:
        identifier = str(row["id"])
        updated = timestamp(row.get("updatedAt" if graph else "updated_at")
                            or row.get("submitted_at") or row.get("created_at"))
        url = row["url" if graph else "html_url"]
        if not isinstance(url, str) or not url.startswith("https://github.com/"):
            raise ValueError()
        user = row.get("author" if graph else "user") or {}
        actor = user.get("login")
        if actor is not None and not isinstance(actor, str):
            raise ValueError()
        fields = ("title", "body", "state", "state_reason", "draft", "isAnswered")
        content = {key: row[key] for key in fields if key in row}
        if kind == "pull_request":
            content.update(head=row["head"]["sha"], base=row["base"]["ref"],
                           merged_at=row.get("merged_at"))
        if kind == "workflow_failure":
            content = {key: row[key] for key in
                       ("run_attempt", "head_sha", "conclusion", "name", "status")}
        digest = hashlib.sha256(canonical(content)).hexdigest()
        return {"key": kind + ":" + identifier, "kind": kind, "id": identifier,
                "updated_at": updated, "digest": digest, "url": url,
                "actor": actor, "parent": parent,
                "title": str(row.get("title") or row.get("name") or "")[:500],
                "state": row.get("state") or row.get("conclusion")}
    except (KeyError, TypeError, ValueError):
        raise PollError("invalid_resource") from None


def inventory(api):
    """Complete bounded traversal; callers commit nothing if any connection fails."""
    c, resources = api.c, {}
    base = "/repos/" + c["repository"]

    def add(kind, row, parent=None, graph=False):
        event = resource(kind, row, parent, graph)
        old = resources.get(event["key"])
        if old is not None and old != event:
            raise PollError("resource_changed_during_pagination")
        resources[event["key"]] = event
        if len(resources) > c["max_resources"]:
            raise PollError("resource_limit")

    for row in api.rest(base + "/issues", {"state": "all", "sort": "updated", "direction": "asc"}):
        if "pull_request" not in row:
            add("issue", row)
    for row in api.rest(base + "/issues/comments", {"sort": "updated", "direction": "asc"}):
        add("issue_comment", row, row.get("issue_url"))
    for row in api.rest(base + "/pulls/comments", {"sort": "updated", "direction": "asc"}):
        add("review_comment", row, row.get("pull_request_url"))
    for row in api.rest(base + "/pulls", {"state": "all", "sort": "updated", "direction": "asc"}):
        add("pull_request", row)
        for review in api.rest(base + "/pulls/" + str(row["number"]) + "/reviews"):
            if review.get("state") != "PENDING":
                add("review", review, row["html_url"])
    owner, repo = c["repository"].split("/")
    for row in api.connection(DISCUSSIONS, {"owner": owner, "repo": repo},
                              ("repository", "discussions")):
        add("discussion", row, graph=True)
        for comment in api.connection(COMMENTS, {"id": row["id"]}, ("node", "comments")):
            add("discussion_comment", comment, row["url"], True)
            for reply in api.connection(REPLIES, {"id": comment["id"]}, ("node", "replies")):
                add("discussion_reply", reply, comment["url"], True)
    for conclusion in FAILURES:
        for row in api.rest(base + "/actions/runs", {"branch": c["branch"], "status": conclusion},
                            "workflow_runs"):
            if row.get("head_branch") != c["branch"] or row.get("conclusion") != conclusion:
                raise PollError("workflow_filter_mismatch")
            add("workflow_failure", row)
    return resources
