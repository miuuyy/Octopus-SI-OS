//! Finite policy graph and bounded whole-secret transformations. Not general DLP.
use crate::*;
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub canonical_path: PathBuf,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub device: u64,
    pub inode: u64,
    pub sha256: String,
}

/// Keep the descriptor alive through spawn. Path-based exec still has a residual race.
pub fn attest(path: &Path) -> Result<(File, Attestation)> {
    let canonical_path = fs::canonicalize(path).map_err(ioerr)?;
    if canonical_path != path || !path.is_absolute() {
        return Err(deny(
            "EXECUTABLE_UNSAFE",
            "identity",
            "canonical absolute binary required",
            "Operator must resolve and review executable path.",
            false,
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(ioerr)?;
    let meta = file.metadata().map_err(ioerr)?;
    // SAFETY: geteuid is argument-free.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_file()
        || meta.mode() & 0o022 != 0
        || meta.mode() & 0o6000 != 0
        || meta.mode() & 0o111 == 0
        || (meta.uid() != 0 && meta.uid() != uid)
    {
        return Err(deny(
            "EXECUTABLE_UNSAFE",
            "identity",
            "unsafe binary ownership or mode",
            "Operator must install a root/current-user-owned executable without group/world write or set-id bits.",
            false,
        ));
    }
    let mut sha = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).map_err(ioerr)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    let after = file.metadata().map_err(ioerr)?;
    if meta.len() != after.len()
        || meta.mtime() != after.mtime()
        || meta.mtime_nsec() != after.mtime_nsec()
        || meta.ctime() != after.ctime()
        || meta.ctime_nsec() != after.ctime_nsec()
    {
        return Err(ioerr("binary changed while hashing"));
    }
    Ok((
        file,
        Attestation {
            canonical_path,
            size: meta.len(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            device: meta.dev(),
            inode: meta.ino(),
            sha256: format!("{:x}", sha.finalize()),
        },
    ))
}

/// A conservative length/diversity admission guard, NOT an entropy estimator.
pub fn strong_secret(s: &str) -> bool {
    s.len() >= 12 && s.bytes().collect::<BTreeSet<_>>().len() >= 6
}
fn base64(bytes: &[u8], url: bool) -> String {
    let abc = if url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let bits = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        for i in 0..4 {
            out.push(if i > chunk.len() {
                '='
            } else {
                abc[((bits >> (18 - i * 6)) & 63) as usize] as char
            });
        }
    }
    out
}
pub fn secret_variants(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::from([s.to_owned()]);
    if !strong_secret(s) {
        return out;
    }
    for url in [false, true] {
        let b = base64(s.as_bytes(), url);
        out.insert(b.trim_end_matches('=').into());
        out.insert(b);
    }
    let hex: String = s.bytes().map(|b| format!("{b:02x}")).collect();
    out.insert(hex.to_uppercase());
    out.insert(hex);
    for upper in [false, true] {
        let pct = |b: u8| {
            if upper {
                format!("%{b:02X}")
            } else {
                format!("%{b:02x}")
            }
        };
        out.insert(s.bytes().map(pct).collect());
        out.insert(
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        pct(b)
                    }
                })
                .collect(),
        );
    }
    let escaped = serde_json::to_string(s).expect("string serializes");
    out.insert(escaped[1..escaped.len() - 1].to_owned());
    out.insert(escaped[1..escaped.len() - 1].replace('/', "\\/"));
    for upper in [false, true] {
        out.insert(
            s.encode_utf16()
                .map(|c| {
                    if upper {
                        format!("\\u{c:04X}")
                    } else {
                        format!("\\u{c:04x}")
                    }
                })
                .collect(),
        );
    }
    out
}
#[derive(Default)]
pub struct Scrubber {
    patterns: Vec<String>,
}
impl Scrubber {
    pub fn new<'a>(secrets: impl Iterator<Item = &'a str>) -> Self {
        let mut patterns: Vec<_> = secrets
            .flat_map(secret_variants)
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        patterns.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Self { patterns }
    }
    fn text(&self, mut text: String) -> String {
        for p in &self.patterns {
            text = text.replace(p, "[REDACTED]");
        }
        text
    }
    pub fn scrub(&self, v: Value) -> Value {
        match v {
            Value::String(s) => json!(self.text(s)),
            Value::Array(a) => Value::Array(a.into_iter().map(|v| self.scrub(v)).collect()),
            Value::Object(o) => Value::Object(
                o.into_iter()
                    .map(|(k, v)| (self.text(k), self.scrub(v)))
                    .collect(),
            ),
            v => v,
        }
    }
}

/// Exact minimum weighted cover over four enumerated repair operations.
/// Each failed conjunct supplies a mask of sufficient alternative repairs.
pub fn solve(clauses: &[u8]) -> (Vec<String>, u32) {
    let names = [
        "argument_fix",
        "lockfile_refresh",
        "secret_scope_grant",
        "server_pin_addition",
    ];
    // Superincreasing costs prefer any combination of lower-authority changes.
    let costs = [1, 4, 16, 64];
    let best = (0u8..16)
        .filter(|m| clauses.iter().all(|c| m & c != 0))
        .min_by_key(|m| {
            (
                (0..4)
                    .filter(|i| m & (1 << i) != 0)
                    .map(|i| costs[i])
                    .sum::<u32>(),
                *m,
            )
        })
        .expect("nonempty repair alternatives");
    (
        names
            .iter()
            .enumerate()
            .filter(|(i, _)| best & (1 << i) != 0)
            .map(|(_, n)| n.to_string())
            .collect(),
        (0..4)
            .filter(|i| best & (1 << i) != 0)
            .map(|i| costs[i])
            .sum(),
    )
}
pub fn policy_receipt(failures: Vec<(Receipt, &str)>) -> Receipt {
    let masks: Vec<_> = failures
        .iter()
        .map(|(_, kind)| match *kind {
            "argument_fix" => 1,
            "lockfile_refresh" => 2,
            "secret_scope_grant" => 4,
            "server_pin_addition" => 8,
            _ => unreachable!(),
        })
        .collect();
    let (repairs, cost) = solve(&masks);
    let mut r = failures[0].0.clone();
    r.minimal_recourse = failures
        .iter()
        .flat_map(|(e, _)| e.minimal_recourse.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    r.minimal_recourse_set = repairs;
    r.total_cost = cost;
    r.is_globally_minimal = true;
    r.retryable = failures.iter().all(|(e, _)| e.retryable);
    r.violated_clause = failures
        .iter()
        .map(|(e, _)| e.violated_clause.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    r.recourse_scope = "all observable preflight conjuncts for this pinned provider; fixed-request repair vocabulary; operator refresh includes reviewed session restart; excludes IO and future upstream behavior".into();
    r
}

/// Deliberately explicit subset; unknown JSON Schema keywords remain upstream-owned.
pub fn arguments_valid(v: &Value, schema: &Value) -> bool {
    if let Some(t) = schema["type"].as_str() {
        let valid = match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "number" => v.is_number(),
            "integer" => v.as_i64().is_some() || v.as_u64().is_some(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            _ => true,
        };
        if !valid {
            return false;
        }
    }
    if schema["enum"].as_array().is_some_and(|a| !a.contains(v)) {
        return false;
    }
    if let Some(n) = v.as_f64()
        && (schema["minimum"].as_f64().is_some_and(|m| n < m)
            || schema["maximum"].as_f64().is_some_and(|m| n > m))
    {
        return false;
    }
    if let Some(o) = v.as_object() {
        if schema["required"].as_array().is_some_and(|a| {
            a.iter()
                .any(|k| k.as_str().is_some_and(|k| !o.contains_key(k)))
        }) {
            return false;
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (k, sub) in properties {
                if let Some(value) = o.get(k) {
                    // Handle authorization is checked separately, before injection.
                    if value.get("$secret_handle").is_none() && !arguments_valid(value, sub) {
                        return false;
                    }
                }
            }
        }
    }
    true
}
