//! A deliberately narrow, fail-closed MCP stdio security boundary (Unix).
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::Duration;

pub mod hardening;
pub mod oracle;
use hardening::*;

pub type Result<T> = std::result::Result<T, Receipt>;
const MAX: u64 = 1_048_576;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub refusal_code: String,
    pub boundary: String,
    pub violated_clause: String,
    pub minimal_recourse: Vec<String>,
    pub retryable: bool,
    #[serde(flatten)]
    pub recourse: Box<Recourse>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recourse {
    pub minimal_recourse_set: Vec<String>,
    pub total_cost: u32,
    pub is_globally_minimal: bool,
    pub recourse_scope: String,
}
impl std::ops::Deref for Receipt {
    type Target = Recourse;
    fn deref(&self) -> &Recourse {
        &self.recourse
    }
}
impl std::ops::DerefMut for Receipt {
    fn deref_mut(&mut self) -> &mut Recourse {
        &mut self.recourse
    }
}
impl std::fmt::Display for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::to_string(self).unwrap_or_default())
    }
}
impl std::error::Error for Receipt {}
pub fn deny(code: &str, boundary: &str, clause: &str, recourse: &str, retryable: bool) -> Receipt {
    Receipt {
        refusal_code: code.into(),
        boundary: boundary.into(),
        violated_clause: clause.into(),
        minimal_recourse: vec![recourse.into()],
        retryable,
        recourse: Box::new(Recourse {
            minimal_recourse_set: Vec::new(),
            total_cost: 0,
            is_globally_minimal: false,
            recourse_scope: "single boundary; no global claim".into(),
        }),
    }
}
fn ioerr(_: impl std::fmt::Display) -> Receipt {
    deny(
        "IO",
        "transport",
        "bounded local IO failed",
        "Operator must inspect local IO; do not blindly retry side effects.",
        false,
    )
}
pub fn canonical(v: &Value) -> Vec<u8> {
    // serde_json without preserve_order uses sorted BTreeMap keys, recursively.
    serde_json::to_vec(v).expect("Value serializes")
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn hash(v: &Value) -> String {
    digest(&canonical(v))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub path: PathBuf,
    pub tools: BTreeSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub server_id: String,
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub executable_sha256: String,
    #[serde(default)]
    pub executable_attestation: Option<Attestation>,
    #[serde(default)]
    pub tools: BTreeMap<String, Value>,
    #[serde(default)]
    pub tool_sha256: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, Secret>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub servers: Vec<Server>,
}

pub fn read_manifest(path: &Path) -> Result<Manifest> {
    let m: Manifest = serde_json::from_slice(&fs::read(path).map_err(ioerr)?).map_err(ioerr)?;
    let mut ids = BTreeSet::new();
    if m.version != 1 || m.servers.is_empty() {
        return Err(ioerr("version"));
    }
    for s in &m.servers {
        if !component(&s.server_id)
            || !ids.insert(&s.server_id)
            || !s.executable.is_absolute()
            || !s.cwd.is_absolute()
            || fs::canonicalize(&s.executable).map_err(ioerr)? != s.executable
            || fs::canonicalize(&s.cwd).map_err(ioerr)? != s.cwd
        {
            return Err(deny(
                "MANIFEST",
                "identity",
                "unique ids and canonical absolute executable/cwd required",
                "Operator must correct manifest paths/ids.",
                false,
            ));
        }
    }
    Ok(m)
}
fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && !s.contains("__")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub struct Session {
    child: Child,
    input: ChildStdin,
    output: Receiver<std::result::Result<Value, ()>>,
    seq: u64,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Session {
    pub fn spawn(s: &Server, check: bool) -> Result<Self> {
        Self::spawn_audited(s, check, None)
    }
    fn spawn_audited(s: &Server, check: bool, ledger: Option<&mut Ledger>) -> Result<Self> {
        let (_descriptor, attestation) = attest(&s.executable)?;
        if check
            && (attestation.sha256 != s.executable_sha256
                || s.executable_attestation.as_ref() != Some(&attestation))
        {
            return Err(deny(
                "EXECUTABLE_CHANGED",
                "identity",
                "executable_sha256 mismatch",
                "Operator must review executable and regenerate lock; caller cannot amend ceiling.",
                false,
            ));
        }
        if let Some(ledger) = ledger {
            ledger.append(json!({"phase":"binary_verified","server_id":s.server_id,"attestation":attestation}))?;
        }
        let mut child = Command::new(&attestation.canonical_path)
            .args(&s.argv)
            .current_dir(&s.cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ioerr)?;
        let input = child.stdin.take().ok_or_else(|| ioerr("stdin"))?;
        let out = child.stdout.take().ok_or_else(|| ioerr("stdout"))?;
        let (tx, output) = sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(out);
            loop {
                let mut b = Vec::new();
                let n = (&mut reader).take(MAX + 1).read_until(b'\n', &mut b);
                let parsed = match n {
                    Ok(n) if n > 0 && n as u64 <= MAX && b.last() == Some(&b'\n') => {
                        serde_json::from_slice(&b).map_err(|_| ())
                    }
                    _ => Err(()),
                };
                let stop = parsed.is_err();
                if tx.send(parsed).is_err() || stop {
                    break;
                }
            }
        });
        let mut session = Self {
            child,
            input,
            output,
            seq: 0,
        };
        let init = session.rpc("initialize", json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"mcp-identity-guard","version":env!("CARGO_PKG_VERSION")}}))?;
        if init["protocolVersion"] != "2025-03-26" {
            return Err(ioerr("unsupported protocol"));
        }
        session.notify("notifications/initialized")?;
        Ok(session)
    }
    fn notify(&mut self, method: &str) -> Result<()> {
        writeln!(self.input, "{}", json!({"jsonrpc":"2.0","method":method})).map_err(ioerr)?;
        self.input.flush().map_err(ioerr)
    }
    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.seq += 1;
        let id = self.seq;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .map_err(ioerr)?;
        self.input.flush().map_err(ioerr)?;
        // Notifications and server-initiated requests are intentionally not forwarded.
        // Refuse rather than accidentally authorize an unexpected protocol message.
        let v = self
            .output
            .recv_timeout(Duration::from_secs(3))
            .map_err(ioerr)?
            .map_err(|_| ioerr("frame"))?;
        if v["jsonrpc"] != "2.0"
            || v["id"] != id
            || v.get("error").is_some()
            || v.get("result").is_none()
        {
            return Err(deny(
                "UPSTREAM_PROTOCOL",
                "transport",
                "unexpected or erroneous upstream response",
                "Operator must inspect upstream; completion may be uncertain, do not replay.",
                false,
            ));
        }
        Ok(v["result"].clone())
    }
    pub fn catalog(&mut self) -> Result<BTreeMap<String, Value>> {
        let result = self.rpc("tools/list", json!({}))?;
        if result.get("nextCursor").is_some() {
            return Err(ioerr("pagination unsupported"));
        }
        let tools = result["tools"].as_array().ok_or_else(|| ioerr("tools"))?;
        let mut map = BTreeMap::new();
        for t in tools {
            let name = t["name"]
                .as_str()
                .filter(|s| component(s))
                .ok_or_else(|| ioerr("name"))?;
            // Extra metadata is rejected: it must not introduce unpinned prompts/annotations.
            if t.as_object().is_none_or(|o| {
                o.keys()
                    .any(|k| !["name", "description", "inputSchema"].contains(&k.as_str()))
            }) {
                return Err(deny(
                    "UNSUPPORTED_TOOL_METADATA",
                    "tool_identity",
                    "tool metadata permits only name, description and inputSchema",
                    "Operator must review provider compatibility with this metadata subset; annotations are untrusted, not authority. No automatic approval or permissive fallback.",
                    false,
                ));
            }
            if !t["description"].is_string()
                || !t["inputSchema"].is_object()
                || map.insert(name.into(), t.clone()).is_some()
            {
                return Err(ioerr("unsupported/duplicate tool metadata"));
            }
        }
        Ok(map)
    }
}
pub fn lock(mut m: Manifest) -> Result<Manifest> {
    for s in &mut m.servers {
        let (_, attestation) = attest(&s.executable)?;
        s.executable_sha256 = attestation.sha256.clone();
        s.executable_attestation = Some(attestation);
        let mut session = Session::spawn(s, true)?;
        s.tools = session.catalog()?;
        s.tool_sha256 = s.tools.iter().map(|(n, t)| (n.clone(), hash(t))).collect();
    }
    Ok(m)
}
fn check_catalog(s: &Server, current: &BTreeMap<String, Value>) -> Result<()> {
    let hashes: BTreeMap<_, _> = current.iter().map(|(n, t)| (n.clone(), hash(t))).collect();
    let locked: BTreeMap<_, _> = s.tools.iter().map(|(n, t)| (n.clone(), hash(t))).collect();
    if hashes != s.tool_sha256 || locked != s.tool_sha256 {
        return Err(deny(
            "CATALOG_CHANGED",
            "tool_identity",
            "complete catalog hashes differ from reviewed lock",
            "Operator must review catalog diff and regenerate lock; no automatic approval.",
            false,
        ));
    }
    Ok(())
}
fn secret_file(path: &Path) -> Result<String> {
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(ioerr)?;
    let meta = f.metadata().map_err(ioerr)?;
    // SAFETY: getuid takes no arguments and has no preconditions.
    if !meta.is_file()
        || meta.mode() & 0o777 != 0o600
        || meta.uid() != unsafe { libc::getuid() }
        || meta.len() > 65536
    {
        return Err(deny(
            "SECRET_FILE",
            "secret_storage",
            "owned regular 0600 file <=64KiB required",
            "Operator must repair secret file ownership/mode; never put secret in the call.",
            false,
        ));
    }
    let mut text = String::new();
    (&mut f)
        .take(65537)
        .read_to_string(&mut text)
        .map_err(ioerr)?;
    if !strong_secret(&text) || text.len() > 65536 {
        return Err(deny(
            "SECRET_STRENGTH",
            "secret_storage",
            "secret requires 12 bytes and 6 distinct bytes, at most 64KiB",
            "Operator must install a longer diverse secret; never inline it.",
            false,
        ));
    }
    Ok(text)
}
fn handles(v: &Value, out: &mut BTreeSet<String>) -> Result<()> {
    match v {
        Value::Object(o) => {
            if let Some(h) = o.get("$secret_handle") {
                if o.len() != 1 || !h.is_string() {
                    return Err(deny(
                        "HANDLE_SHAPE",
                        "parameters",
                        "handle must be a singleton string object",
                        "Use exactly {\"$secret_handle\":\"handle_id\"}.",
                        true,
                    ));
                }
                out.insert(h.as_str().unwrap().into());
            } else {
                for x in o.values() {
                    handles(x, out)?;
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                handles(x, out)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn inject(v: &Value, secrets: &BTreeMap<String, String>) -> Value {
    match v {
        Value::Object(o) if o.contains_key("$secret_handle") => {
            json!(secrets[o["$secret_handle"].as_str().unwrap()])
        }
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), inject(x, secrets)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| inject(x, secrets)).collect()),
        _ => v.clone(),
    }
}
pub fn scrub(v: Value, secrets: &BTreeMap<String, String>) -> Value {
    Scrubber::new(secrets.values().map(String::as_str)).scrub(v)
}
pub struct Guard {
    manifest: Manifest,
    frozen: BTreeSet<String>,
    sessions: BTreeMap<String, Session>,
    secrets: BTreeMap<String, String>,
    scrubber: Scrubber,
    pub last_scrubbed: bool,
}
impl Guard {
    pub fn new(manifest: Manifest) -> Result<Self> {
        Self::new_audited(manifest, None)
    }
    fn new_audited(manifest: Manifest, mut ledger: Option<&mut Ledger>) -> Result<Self> {
        let mut sessions = BTreeMap::new();
        for s in &manifest.servers {
            let mut session = Session::spawn_audited(s, true, ledger.as_deref_mut())?;
            check_catalog(s, &session.catalog()?)?;
            sessions.insert(s.server_id.clone(), session);
        }
        Ok(Self {
            frozen: BTreeSet::new(),
            manifest,
            sessions,
            secrets: BTreeMap::new(),
            scrubber: Scrubber::default(),
            last_scrubbed: false,
        })
    }
    pub fn list(&self) -> Value {
        let tools: Vec<_> = self
            .manifest
            .servers
            .iter()
            .flat_map(|s| {
                s.tools.iter().map(|(n, t)| {
                    let mut t = t.clone();
                    t["name"] = json!(format!("{}__{}", s.server_id, n));
                    t
                })
            })
            .collect();
        json!({"tools":tools})
    }
    pub fn call(&mut self, name: &str, args: Value) -> Result<Value> {
        let (sid, tool) = name.split_once("__").ok_or_else(|| {
            deny(
                "QUALIFIED_NAME_REQUIRED",
                "routing",
                "flat names are forbidden",
                "Use the exact server_id__tool_name from tools/list.",
                true,
            )
        })?;
        let s = self
            .manifest
            .servers
            .iter()
            .find(|s| s.server_id == sid)
            .ok_or_else(|| {
                deny(
                    "SERVER_DENIED",
                    "capability_ceiling",
                    "server is outside reviewed manifest",
                    "No caller-side amendment; operator must review a new server grant.",
                    false,
                )
            })?;
        self.last_scrubbed = false;
        if self.frozen.contains(sid) {
            return Err(deny(
                "SESSION_FROZEN",
                "identity",
                "prior upstream violation",
                "Operator must inspect and restart reviewed session.",
                false,
            ));
        }
        let mut failures = Vec::new();
        if !s.tools.contains_key(tool) {
            failures.push((
                deny(
                    "TOOL_DENIED",
                    "capability_ceiling",
                    "tool is outside server grant",
                    "Operator must review tool grant.",
                    false,
                ),
                "lockfile_refresh",
            ));
        }
        if !args.is_object() {
            failures.push((
                deny(
                    "ARGUMENTS",
                    "parameters",
                    "arguments must be an object",
                    "Supply an object as arguments.",
                    true,
                ),
                "argument_fix",
            ));
        }
        let mut requested = BTreeSet::new();
        if let Err(e) = handles(&args, &mut requested) {
            failures.push((e, "argument_fix"));
        }
        if requested
            .iter()
            .any(|h| s.secrets.get(h).is_none_or(|g| !g.tools.contains(tool)))
        {
            failures.push((deny("SECRET_DENIED", "capability_ceiling",
                "one or more handles lack exact server/tool grant",
                "Remove all ungranted handles; access requires operator-reviewed exact-pair grants.", false), "secret_scope_grant"));
        }
        if s.tools
            .get(tool)
            .is_some_and(|t| !arguments_valid(&args, &t["inputSchema"]))
            && args.is_object()
        {
            failures.push((
                deny(
                    "ARGUMENT_CONSTRAINT",
                    "parameters",
                    "supported argument constraint failed",
                    "Correct required, type, enum or numeric bound constraints.",
                    true,
                ),
                "argument_fix",
            ));
        }
        let session = self.sessions.get_mut(sid).ok_or_else(|| ioerr("session"))?;
        if let Err(e) = session.catalog().and_then(|c| check_catalog(s, &c)) {
            self.frozen.insert(sid.into());
            if e.refusal_code != "CATALOG_CHANGED" {
                return Err(e);
            }
            failures.push((e, "lockfile_refresh"));
        }
        if !failures.is_empty() {
            return Err(policy_receipt(failures));
        }
        let mut resolved = BTreeMap::new();
        for h in requested {
            let value = secret_file(&s.secrets[&h].path)?;
            self.secrets.insert(format!("{sid}/{h}"), value.clone());
            resolved.insert(h, value);
        }
        self.scrubber = Scrubber::new(self.secrets.values().map(String::as_str));
        let args = inject(&args, &resolved);
        let result = match session.rpc("tools/call", json!({"name":tool,"arguments":args})) {
            Ok(r) => r,
            Err(e) => {
                self.frozen.insert(sid.into());
                return Err(e);
            }
        };
        let clean = self.scrubber.scrub(result.clone());
        self.last_scrubbed = clean != result;
        Ok(clean)
    }
    pub fn sanitize(&self, v: Value) -> Value {
        self.scrubber.scrub(v)
    }
}

/// Hash-chain evidence, not signatures: anchor the final digest externally to detect truncation.
pub fn verify_ledger(path: &Path) -> Result<(u64, String)> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(ioerr)?;
    let mut prev = "0".repeat(64);
    let mut seq = 0;
    for line in BufReader::new(f).lines() {
        let v: Value = serde_json::from_str(&line.map_err(ioerr)?).map_err(ioerr)?;
        if v["seq"] != seq
            || v["prev"] != prev
            || v["hash"] != hash(&json!({"seq":seq,"prev":prev,"event":v["event"]}))
        {
            return Err(deny(
                "LEDGER_TAMPER",
                "audit",
                "chain or sequence mismatch",
                "Restore trusted ledger; compare externally anchored head.",
                false,
            ));
        }
        prev = v["hash"].as_str().ok_or_else(|| ioerr("hash"))?.into();
        seq += 1;
    }
    Ok((seq, prev))
}
pub struct Ledger {
    file: File,
    seq: u64,
    prev: String,
    lock_path: PathBuf,
}
impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        let lock_path = path.with_extension("writer-lock");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock_path)
            .map_err(ioerr)?;
        let opened = (|| {
            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(ioerr)?;
            let (seq, prev) = verify_ledger(path)?;
            Ok(Self {
                file,
                seq,
                prev,
                lock_path: lock_path.clone(),
            })
        })();
        if opened.is_err() {
            let _ = fs::remove_file(lock_path);
        }
        opened
    }
    pub fn append(&mut self, event: Value) -> Result<()> {
        let entry = json!({"seq":self.seq,"prev":self.prev,"event":event});
        let head = hash(&entry);
        let mut entry = entry;
        entry["hash"] = json!(head);
        writeln!(self.file, "{}", entry).map_err(ioerr)?;
        self.file.sync_data().map_err(ioerr)?;
        self.prev = head;
        self.seq += 1;
        Ok(())
    }
}
impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

pub fn proxy(manifest: Manifest, ledger: &Path) -> Result<()> {
    let mut ledger = Ledger::open(ledger)?;
    let mut guard = match Guard::new_audited(manifest, Some(&mut ledger)) {
        Ok(g) => g,
        Err(e) => {
            ledger.append(json!({"phase":"startup","refusal":e}))?;
            return Err(e);
        }
    };
    let input = std::io::stdin();
    let mut reader = input.lock();
    let mut initialized = false;
    loop {
        let mut b = Vec::new();
        let n = (&mut reader)
            .take(MAX + 1)
            .read_until(b'\n', &mut b)
            .map_err(ioerr)?;
        if n == 0 {
            break;
        }
        if n as u64 > MAX || b.last() != Some(&b'\n') {
            return Err(ioerr("frame"));
        }
        let v: Value = serde_json::from_slice(&b).map_err(ioerr)?;
        let id = v.get("id").cloned();
        if id.is_none() {
            if v["method"] == "notifications/initialized" {
                initialized = true;
                continue;
            }
            return Err(ioerr("unsupported notification"));
        }
        ledger.append(json!({"phase":"received"}))?;
        let result = if v["jsonrpc"] != "2.0" {
            Err(ioerr("jsonrpc"))
        } else {
            match v["method"].as_str().unwrap_or("") {
                "initialize" => Ok(
                    json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"mcp-identity-guard","version":env!("CARGO_PKG_VERSION")}}),
                ),
                "tools/list" if initialized => Ok(guard.list()),
                "tools/call" if initialized => guard.call(
                    v["params"]["name"].as_str().unwrap_or(""),
                    v["params"].get("arguments").cloned().unwrap_or(json!({})),
                ),
                _ => Err(deny(
                    "METHOD_DENIED",
                    "capability_ceiling",
                    "method unavailable or session not initialized",
                    "Complete initialization and use only tools/list or tools/call.",
                    false,
                )),
            }
        };
        let status = match &result {
            Ok(_) => json!({"outcome":"allowed","scrubbed":guard.last_scrubbed}),
            Err(e) => json!({"outcome":"refused","receipt":e}),
        };
        // No arguments, names, ids or result data enter the audit stream.
        ledger.append(status)?;
        let response = match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err(e) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"Guard refused request","data":e}})
            }
        };
        println!("{}", guard.sanitize(response));
    }
    Ok(())
}
