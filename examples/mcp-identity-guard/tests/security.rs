use mcp_identity_guard::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    manifest: Manifest,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "test-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join("secret"))
            .unwrap();
        f.write_all(b"synthetic-PAT-only-7f91").unwrap();
        let servers = ["a", "b", "rug"]
            .iter()
            .map(|id| Server {
                server_id: (*id).into(),
                executable: fs::canonicalize(env!("CARGO_BIN_EXE_mcp-identity-guard")).unwrap(),
                argv: vec!["__mock".into(), (*id).into()],
                cwd: fs::canonicalize(&root).unwrap(),
                executable_sha256: String::new(),
                executable_attestation: None,
                tools: BTreeMap::new(),
                tool_sha256: BTreeMap::new(),
                secrets: if *id == "a" {
                    BTreeMap::from([(
                        "pat".into(),
                        Secret {
                            path: root.join("secret"),
                            tools: BTreeSet::from(["github_read_issue".into()]),
                        },
                    )])
                } else {
                    BTreeMap::new()
                },
            })
            .collect();
        Self {
            root,
            manifest: lock(Manifest {
                version: 1,
                servers,
            })
            .unwrap(),
        }
    }
}
#[test]
fn namespaces_never_shadow() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    assert_eq!(
        g.call("github_read_issue", json!({}))
            .unwrap_err()
            .refusal_code,
        "QUALIFIED_NAME_REQUIRED"
    );
    assert_eq!(
        g.call("a__github_read_issue", json!({})).unwrap()["provider"],
        "a"
    );
    assert_eq!(
        g.call("b__github_read_issue", json!({})).unwrap()["provider"],
        "b"
    );
}
#[test]
fn mutation_freezes_session() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    assert_eq!(
        g.call("rug__github_read_issue", json!({}))
            .unwrap_err()
            .refusal_code,
        "CATALOG_CHANGED"
    );
    assert_eq!(
        g.call("rug__github_read_issue", json!({}))
            .unwrap_err()
            .refusal_code,
        "SESSION_FROZEN"
    );
}
#[test]
fn secret_injected_and_echo_scrubbed() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    let v = g
        .call(
            "a__github_read_issue",
            json!({"token":{"$secret_handle":"pat"}}),
        )
        .unwrap();
    assert_eq!(v["injected"], true);
    assert!(v.to_string().contains("[REDACTED]"));
    assert!(!v.to_string().contains("synthetic-PAT-only-7f91"));
}
#[test]
fn cross_server_and_tool_ceiling_denied() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    assert_eq!(
        g.call(
            "b__github_read_issue",
            json!({"token":{"$secret_handle":"pat"}})
        )
        .unwrap_err()
        .refusal_code,
        "SECRET_DENIED"
    );
    let e = g.call("a__ungranted", json!({})).unwrap_err();
    assert_eq!(e.boundary, "capability_ceiling");
    assert!(!e.retryable);
}
#[test]
fn exact_recourse_changes_decision() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    assert_eq!(
        g.call("a__github_read_issue", json!([]))
            .unwrap_err()
            .refusal_code,
        deny(
            "ARGUMENTS",
            "parameters",
            "arguments must be an object",
            "Supply an object as arguments.",
            true
        )
        .refusal_code
    );
    assert!(g.call("a__github_read_issue", json!({})).is_ok());
}
#[test]
fn secret_mode_and_symlink_refused() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    fs::set_permissions(f.root.join("secret"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        g.call(
            "a__github_read_issue",
            json!({"token":{"$secret_handle":"pat"}})
        )
        .unwrap_err()
        .refusal_code,
        "SECRET_FILE"
    );
    fs::rename(f.root.join("secret"), f.root.join("actual")).unwrap();
    std::os::unix::fs::symlink(f.root.join("actual"), f.root.join("secret")).unwrap();
    assert!(
        g.call(
            "a__github_read_issue",
            json!({"token":{"$secret_handle":"pat"}})
        )
        .is_err()
    );
}
#[test]
fn executable_digest_change_denied() {
    let mut f = Fixture::new();
    f.manifest.servers[0].executable_sha256 = "0".repeat(64);
    assert_eq!(
        Guard::new(f.manifest.clone()).err().unwrap().refusal_code,
        "EXECUTABLE_CHANGED"
    );
}
#[test]
fn ledger_tamper_and_writer_exclusion() {
    let f = Fixture::new();
    let path = f.root.join("ledger");
    {
        let mut l = Ledger::open(&path).unwrap();
        assert!(Ledger::open(&path).is_err());
        l.append(json!({"outcome":"refused"})).unwrap();
        l.append(json!({"outcome":"allowed"})).unwrap();
    }
    assert_eq!(verify_ledger(&path).unwrap().0, 2);
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("refused", "allowed");
    fs::write(&path, text).unwrap();
    assert_eq!(
        verify_ledger(&path).unwrap_err().refusal_code,
        "LEDGER_TAMPER"
    );
    assert!(Ledger::open(&path).is_err());
}
#[test]
fn canonical_order_and_recursive_scrub() {
    assert_eq!(
        hash(&json!({"b":1,"a":{"d":2,"c":3}})),
        hash(&json!({"a":{"c":3,"d":2},"b":1}))
    );
    let secrets = BTreeMap::from([("x".into(), "p\"a\\t".into())]);
    let scrubbed = scrub(json!({"p\"a\\t":["p\"a\\t"]}), &secrets);
    assert_eq!(scrubbed, json!({"[REDACTED]":["[REDACTED]"]}));
}
#[test]
fn cli_lock_proxy_and_ledger() {
    let f = Fixture::new();
    let spec = f.root.join("spec.json");
    let locked = f.root.join("locked.json");
    let ledger = f.root.join("ledger.jsonl");
    fs::write(&spec, serde_json::to_vec(&f.manifest).unwrap()).unwrap();
    let binary = env!("CARGO_BIN_EXE_mcp-identity-guard");
    assert!(
        Command::new(binary)
            .args(["lock", spec.to_str().unwrap(), locked.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        !Command::new(binary)
            .args(["lock", spec.to_str().unwrap(), locked.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut child = Command::new(binary)
        .args(["proxy", locked.to_str().unwrap(), ledger.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for v in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"a__github_read_issue","arguments":{"token":{"$secret_handle":"pat"}}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"github_read_issue"}}),
    ] {
        writeln!(input, "{v}").unwrap();
    }
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("synthetic-PAT-only-7f91"));
    let responses: Vec<Value> = text
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(responses[1]["result"]["injected"], true);
    assert_eq!(
        responses[2]["error"]["data"]["refusal_code"],
        "QUALIFIED_NAME_REQUIRED"
    );
    assert!(verify_ledger(&ledger).unwrap().0 >= 3);
    let audit: Vec<Value> = fs::read_to_string(&ledger)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let pins: Vec<_> = audit
        .iter()
        .filter(|e| e["event"]["phase"] == "binary_verified")
        .collect();
    assert_eq!(pins.len(), 3);
    assert_eq!(
        pins[0]["event"]["attestation"]["sha256"],
        f.manifest.servers[0].executable_sha256
    );
    assert!(
        Command::new(binary)
            .args(["verify-ledger", ledger.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn all_six_benchmark_scenarios() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcp-identity-guard"))
        .arg("bench")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["scenarios_passed"], 6);
    assert_eq!(v["benign_false_positives"], 0);
}

#[test]
fn encoded_secrets_both_payloads_and_no_short_encoding_matches() {
    use hardening::*;
    let secret = "Ab+/09xyZ!\"é-Token";
    assert!(strong_secret(secret));
    let patterns = secret_variants(secret);
    assert!(patterns.len() >= 10);
    let secrets = BTreeMap::from([("pat".into(), secret.into())]);
    for pattern in patterns {
        let payload = json!({"result":{"content":[pattern.clone()]},
            "error":{"message":pattern.clone(),"data":{pattern:"echo"}}});
        let clean = scrub(payload, &secrets);
        assert_eq!(clean["result"]["content"][0], "[REDACTED]");
        assert_eq!(clean["error"]["message"], "[REDACTED]");
        assert_eq!(clean["error"]["data"]["[REDACTED]"], "echo");
    }
    assert_eq!(secret_variants("abc"), BTreeSet::from(["abc".into()]));
    assert!(!strong_secret("aaaaaaaaaaaaaaaaaa"));
    assert!(!strong_secret("abc"));
    let s = BTreeMap::from([("x".into(), "abc".into())]);
    assert_eq!(
        scrub(json!("YWJj normal text"), &s),
        json!("YWJj normal text")
    );
    // Independent published base64/hex vectors, not just generator-vs-generator.
    let v = secret_variants("abcdefghijklmn");
    assert!(v.contains("YWJjZGVmZ2hpamtsbW4="));
    assert!(v.contains("YWJjZGVmZ2hpamtsbW4"));
    assert!(v.contains("6162636465666768696a6b6c6d6e"));
    assert!(secret_variants("abcdefghijk?").contains("YWJjZGVmZ2hpams_"));
}
#[test]
fn weak_secret_is_never_injected() {
    let f = Fixture::new();
    fs::write(f.root.join("secret"), b"abc").unwrap();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    assert_eq!(
        g.call(
            "a__github_read_issue",
            json!({"token":{"$secret_handle":"pat"}})
        )
        .unwrap_err()
        .refusal_code,
        "SECRET_STRENGTH"
    );
}
#[test]
fn unsafe_executable_and_metadata_changes_rejected() {
    let f = Fixture::new();
    let path = fs::canonicalize(&f.root).unwrap().join("server-copy");
    fs::copy(&f.manifest.servers[0].executable, &path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        hardening::attest(&path).unwrap_err().refusal_code,
        "EXECUTABLE_UNSAFE"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let (_, before) = hardening::attest(&path).unwrap();
    let mut server = f.manifest.servers[0].clone();
    server.executable = path.clone();
    server.executable_sha256 = before.sha256.clone();
    server.executable_attestation = Some(before.clone());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(hardening::attest(&path).unwrap().1.sha256, before.sha256);
    assert_eq!(
        Session::spawn(&server, true).err().unwrap().refusal_code,
        "EXECUTABLE_CHANGED"
    );
    let link = path.with_extension("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(
        hardening::attest(&link).unwrap_err().refusal_code,
        "EXECUTABLE_UNSAFE"
    );
}
#[test]
fn exhaustive_weighted_cover_matches_independent_reference() {
    for a in 1..16u8 {
        for b in 1..16u8 {
            for c in 1..16u8 {
                let clauses = [a, b, c];
                let (set, cost) = hardening::solve(&clauses);
                let weights = [1, 4, 16, 64];
                let expected = (0..16u8)
                    .filter(|m| clauses.iter().all(|c| m & c != 0))
                    .map(|m| {
                        (0..4)
                            .map(|i| if m & (1 << i) != 0 { weights[i] } else { 0 })
                            .sum::<u32>()
                    })
                    .min()
                    .unwrap();
                assert_eq!(cost, expected);
                assert!(!set.is_empty());
            }
        }
    }
}
#[test]
fn simultaneous_policy_failures_and_complete_repair() {
    let mut f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    let args = json!({"issue":0,"token":{"$secret_handle":"ungranted"}});
    let r = g.call("a__github_read_issue", args).unwrap_err();
    assert_eq!(
        r.minimal_recourse_set,
        vec!["argument_fix", "secret_scope_grant"]
    );
    assert_eq!(r.total_cost, 17);
    assert!(r.is_globally_minimal);
    // Argument repair alone is insufficient.
    assert!(
        g.call(
            "a__github_read_issue",
            json!({"issue":1,"token":{"$secret_handle":"ungranted"}})
        )
        .is_err()
    );
    // Operator reviews the exact grant, then both repairs suffice.
    let grant = f.manifest.servers[0].secrets["pat"].clone();
    f.manifest.servers[0]
        .secrets
        .insert("ungranted".into(), grant);
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    assert!(
        g.call(
            "a__github_read_issue",
            json!({"issue":0,"token":{"$secret_handle":"ungranted"}})
        )
        .is_err()
    );
    assert!(
        g.call(
            "a__github_read_issue",
            json!({"issue":1,"token":{"$secret_handle":"ungranted"}})
        )
        .is_ok()
    );
    // Catalog, argument and secret clauses are all observed before denial.
    let r = g
        .call("rug__github_read_issue", json!([{"$secret_handle":"pat"}]))
        .unwrap_err();
    assert_eq!(
        r.minimal_recourse_set,
        vec!["argument_fix", "lockfile_refresh", "secret_scope_grant"]
    );
    assert_eq!(r.total_cost, 21);
    assert!(r.violated_clause.contains("catalog"));
}
#[test]
fn oracle_cli_rejects_all_five_spoofs() {
    let output = Command::new(env!("CARGO_BIN_EXE_mcp-identity-guard"))
        .arg("oracle-bench")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["scenarios_passed"], 6);
    assert_eq!(v["optimistic"]["false_accepts"], 5);
    assert_eq!(v["exact"]["false_accepts"], 0);
    assert_eq!(v["exact"]["false_rejects"], 0);
}

#[test]
fn encoded_upstream_results_scrubbed_and_errors_suppressed() {
    let f = Fixture::new();
    let mut g = Guard::new(f.manifest.clone()).unwrap();
    let variants = hardening::secret_variants("synthetic-PAT-only-7f91");
    let result = g
        .call(
            "a__github_read_issue",
            json!({"token":{"$secret_handle":"pat"},"emit_variants":true}),
        )
        .unwrap();
    assert!(g.last_scrubbed);
    for value in result["variants"].as_array().unwrap() {
        assert_eq!(value, "[REDACTED]");
    }
    let e = g
        .call(
            "a__github_read_issue",
            json!({"token":{"$secret_handle":"pat"},"emit_variants":true,"rpc_error":true}),
        )
        .unwrap_err();
    assert_eq!(e.refusal_code, "UPSTREAM_PROTOCOL");
    let response = g.sanitize(json!({"error":e}));
    for variant in variants {
        assert!(!response.to_string().contains(&variant));
    }
}

#[test]
fn cli_lock_refuses_untrusted_extra_metadata_without_dispatch() {
    let f = Fixture::new();
    let binary = env!("CARGO_BIN_EXE_mcp-identity-guard");
    for mode in ["metadata_minimal", "annotations", "unsupported_metadata"] {
        let mut manifest = f.manifest.clone();
        manifest.servers.truncate(1);
        manifest.servers[0].argv = vec!["__mock".into(), mode.into()];
        manifest.servers[0].secrets.clear();
        let spec = f.root.join(format!("{mode}.json"));
        let output_lock = f.root.join(format!("{mode}.lock.json"));
        fs::write(&spec, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let output = Command::new(binary)
            .arg("lock")
            .arg(spec)
            .arg(&output_lock)
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
        assert!(!f.root.join("tool-called").exists());
        assert_eq!(output_lock.exists(), mode == "metadata_minimal");
        assert_eq!(output.status.success(), mode == "metadata_minimal");
        if mode == "metadata_minimal" {
            assert!(output.stderr.is_empty());
        } else {
            let receipt: Receipt = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(receipt.refusal_code, "UNSUPPORTED_TOOL_METADATA");
            assert_eq!(receipt.boundary, "tool_identity");
            assert_eq!(
                receipt.violated_clause,
                "tool metadata permits only name, description and inputSchema"
            );
            assert!(!receipt.retryable);
            assert!(!receipt.is_globally_minimal);
            let diagnostic = String::from_utf8(output.stderr).unwrap();
            assert!(!diagnostic.contains("untrusted-key-marker"));
            assert!(!diagnostic.contains("untrusted-value-marker"));
        }
    }
}
