use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use octopus_worker_env::{
    AbsPath, EnvContext, InterfaceId, WorkerEnvCaps, WorkerEnvError, WorkerEnvKindRegistry,
    WorkerEnvKindSpec, WorkerEnvironment, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};

use super::{
    AbandonedFormation, GitWorktreeEvidenceLimits, abandon_git_worktree, abandoned_formation_path,
    capture_git_worktree_evidence, instance_worktree_path, lease_git_worktree,
    materialize_worktree, release_git_worktree, write_abandoned_formation,
};
use octopus_workspace_substrate::boundary::project_sandbox;

fn worktree_wek_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "worker_environment_kind_id": "local.coder.worktree",
        "substrate_family": "process_exec",
        "door_driver_refs": ["system_coder_env"],
        "tool_process_bins": [
            "target/debug/octopus-coder-exec-tool",
            "target/debug/octopus-coder-fs-tool"
        ],
        "worker_consent": "explicit_ceiling",
        "supported_executor_kinds": ["exec"],
        "capabilities": ["filesystem", "process_exec", "git"],
        "boundary_defaults": {
            "read_root_refs": [{ "path": "repo_root", "access": ["read"] }],
            "write_root_refs": [
                { "path": "git_common_root", "access": ["read", "write"] },
                { "path": "worktree_root", "access": ["read", "write"] }
            ],
            "protected_read_only_subpaths": [{ "path": "protected_branch_ref", "access": ["read"] }],
            "network_ceiling": "denied"
        },
        "lifecycle": { "requires_release": true }
    }))
    .unwrap()
}

fn registry() -> WorkerEnvKindRegistry {
    let mut reg = WorkerEnvKindRegistry::default();
    reg.upsert(&worktree_wek_json()).unwrap();
    reg
}

fn form_room(
    reg: &WorkerEnvKindRegistry,
    spec: &WorkerEnvKindSpec,
    ctx: &EnvContext,
    door: &str,
    worker: &str,
    tool: &str,
) -> WorkerEnvironmentFormation {
    let kind = spec.worker_environment_kind_id.clone();
    let kinds = vec![kind];
    let tools = vec![
        "coder.exec".to_string(),
        "coder.apply_patch".to_string(),
        "coder.write_file".to_string(),
        "coder.worktree.snapshot".to_string(),
        "coder.worktree.abandon".to_string(),
    ];
    let tool_hashes = tools
        .iter()
        .map(|tool| (tool.clone(), format!("hash-{tool}")))
        .collect::<BTreeMap<_, _>>();
    let tool_process_bins = tools
        .iter()
        .map(|tool| {
            (
                tool.clone(),
                if tool == "coder.exec" {
                    "target/debug/octopus-coder-exec-tool"
                } else {
                    "target/debug/octopus-coder-fs-tool"
                }
                .to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let door = InterfaceId::new(door);
    let worker = InterfaceId::new(worker);
    let door_root_bindings = ctx.provided_root_bindings();
    reg.form(&WorkerEnvironmentFormationRequest {
        door_additional_root_bindings: &Default::default(),
        door_interface: &door,
        door_descriptor_hash: "hash-door",
        door_driver_ref: "system_coder_env",
        tool,
        tool_descriptor_hashes: &tool_hashes,
        tool_process_bins: &tool_process_bins,
        tool_worker_environment_kinds: &kinds,
        worker: &worker,
        ee: "ee_x",
        run: "run_y",
        granted_worker_environment_kinds: &kinds,
        granted_tools: &tools,
        door_root_bindings: &door_root_bindings,
        door_instance_dir: std::path::Path::new("."),
        ee_root_slot_allowance: None,
    })
    .expect("worker environment forms")
}

fn unique_git_repo_and_worktree_dir() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)
{
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    static FIXTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "octo_wt_{}_{}_{}",
        std::process::id(),
        nanos,
        unique
    ));
    let repo = root.join("repo");
    let worktree = root.join("leased");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init"]);
    git(&repo, &["checkout", "-B", "main"]);
    std::fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Octopus Test",
            "-c",
            "user.email=octopus-test@example.invalid",
            "commit",
            "-m",
            "seed",
        ],
    );

    (root, repo, worktree)
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawning git {args:?} failed: {e}"));
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn worktree_materialization_recovers_an_unattached_formation_branch() {
    let (test_root, repo_dir, wt_dir) = unique_git_repo_and_worktree_dir();
    let branch = "run/recovery-fixture";
    git(&repo_dir, &["branch", branch, "HEAD"]);
    let repo_root = AbsPath::new(&repo_dir).unwrap();
    let worktree_root = AbsPath::new(&wt_dir).unwrap();

    materialize_worktree(&repo_root, &worktree_root, branch)
        .expect("existing formation branch attaches to its deterministic worktree");
    let head = git(&wt_dir, &["symbolic-ref", "--quiet", "HEAD"]);
    assert_eq!(
        std::str::from_utf8(&head).unwrap().trim(),
        format!("refs/heads/{branch}")
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

#[test]
fn worktree_materialization_preserves_an_unowned_existing_directory() {
    let (test_root, repo_dir, wt_dir) = unique_git_repo_and_worktree_dir();
    std::fs::create_dir_all(&wt_dir).unwrap();
    let sentinel = wt_dir.join("sentinel");
    std::fs::write(&sentinel, b"owned elsewhere\n").unwrap();
    let repo_root = AbsPath::new(&repo_dir).unwrap();
    let worktree_root = AbsPath::new(&wt_dir).unwrap();

    assert!(materialize_worktree(&repo_root, &worktree_root, "run/refuse-existing").is_err());
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"owned elsewhere\n");

    let _ = std::fs::remove_dir_all(&test_root);
}

#[test]
fn worktree_materialization_preserves_the_live_checkout_snapshot() {
    let (test_root, repo_dir, wt_dir) = unique_git_repo_and_worktree_dir();
    std::fs::create_dir_all(repo_dir.join("nested")).unwrap();
    std::fs::write(repo_dir.join("nested/clean.txt"), "clean\n").unwrap();
    std::fs::write(repo_dir.join("deleted.txt"), "delete me\n").unwrap();
    std::fs::write(repo_dir.join("sparse.txt"), "sparse content\n").unwrap();
    std::fs::write(
        repo_dir.join(".gitignore"),
        "ignored.txt\nignored-directory/\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("clean.txt", repo_dir.join("nested/clean-link")).unwrap();
    git(&repo_dir, &["add", "."]);
    git(
        &repo_dir,
        &[
            "-c",
            "user.name=Octopus Test",
            "-c",
            "user.email=octopus-test@example.invalid",
            "commit",
            "-m",
            "snapshot fixture",
        ],
    );

    std::fs::write(repo_dir.join("README.md"), "dirty live checkout\n").unwrap();
    std::fs::remove_file(repo_dir.join("deleted.txt")).unwrap();
    std::fs::write(repo_dir.join("untracked.txt"), "untracked\n").unwrap();
    std::fs::write(repo_dir.join("ignored.txt"), "ignored\n").unwrap();
    std::fs::create_dir(repo_dir.join("ignored-directory")).unwrap();
    std::fs::write(repo_dir.join("ignored-directory/hidden.txt"), "ignored\n").unwrap();
    let nested_repo = repo_dir.join("nested-repository");
    std::fs::create_dir(&nested_repo).unwrap();
    git(&nested_repo, &["init"]);
    std::fs::write(nested_repo.join("private.txt"), "nested repository\n").unwrap();
    git(
        &repo_dir,
        &["update-index", "--skip-worktree", "sparse.txt"],
    );
    std::fs::remove_file(repo_dir.join("sparse.txt")).unwrap();

    let repo_root = AbsPath::new(&repo_dir).unwrap();
    let worktree_root = AbsPath::new(&wt_dir).unwrap();
    materialize_worktree(&repo_root, &worktree_root, "run/live-snapshot-fixture")
        .expect("the isolated worktree snapshots the live checkout");

    assert_eq!(
        std::fs::read_to_string(wt_dir.join("README.md")).unwrap(),
        "dirty live checkout\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt_dir.join("nested/clean.txt")).unwrap(),
        "clean\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt_dir.join("sparse.txt")).unwrap(),
        "sparse content\n",
        "clean tracked files absent from a sparse live checkout come from the run branch index"
    );
    assert!(!wt_dir.join("deleted.txt").exists());
    assert_eq!(
        std::fs::read_to_string(wt_dir.join("untracked.txt")).unwrap(),
        "untracked\n"
    );
    assert!(!wt_dir.join("ignored.txt").exists());
    assert!(!wt_dir.join("ignored-directory").exists());
    assert!(wt_dir.join("nested-repository").is_dir());
    assert_eq!(
        std::fs::read_dir(wt_dir.join("nested-repository"))
            .unwrap()
            .count(),
        0,
        "a nested repository remains an empty directory instead of leaking its private .git tree"
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_link(wt_dir.join("nested/clean-link")).unwrap(),
        PathBuf::from("clean.txt")
    );

    let status = String::from_utf8(git(
        &wt_dir,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    ))
    .unwrap();
    assert!(status.contains(" M README.md"));
    assert!(status.contains(" D deleted.txt"));
    assert!(status.contains("?? untracked.txt"));
    assert!(!status.contains("sparse.txt"));
    assert!(!status.contains("ignored.txt"));

    std::fs::write(wt_dir.join("nested/clean.txt"), "run-owned\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("nested/clean.txt")).unwrap(),
        "clean\n",
        "writes to the materialized worktree remain isolated from the live checkout"
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

async fn lease_coder_worktree(
    reg: &WorkerEnvKindRegistry,
    repo: &Path,
    wt: &Path,
) -> WorkerEnvironment {
    lease_coder_worktree_result(reg, repo, wt)
        .await
        .expect("git worktree room leases")
}

async fn lease_coder_worktree_result(
    reg: &WorkerEnvKindRegistry,
    repo: &Path,
    wt: &Path,
) -> Result<WorkerEnvironment, WorkerEnvError> {
    let kind_id = WorkerEnvironmentKindId::from("local.coder.worktree");
    let (spec, _) = reg.get(&kind_id).unwrap();
    let ctx = EnvContext {
        additional_root_slots: Default::default(),
        root_slots: BTreeMap::from([
            ("repo_root".to_string(), AbsPath::new(repo).unwrap()),
            ("worktree_root".to_string(), AbsPath::new(wt).unwrap()),
            (
                "git_common_root".to_string(),
                AbsPath::new(&repo.join(".git")).unwrap(),
            ),
        ]),
        env: BTreeMap::new(),
    };
    let formation = form_room(
        reg,
        &spec,
        &ctx,
        "system_coder_env",
        "system_worker_coder",
        "coder.exec",
    );
    lease_git_worktree(
        &spec,
        &formation,
        repo,
        wt,
        &repo.parent().unwrap().join("private-worktree-state"),
        None,
    )
    .await
}

fn leased_worktree_path(base: &Path, env: &WorkerEnvironment) -> PathBuf {
    instance_worktree_path(base, env.id()).unwrap()
}

#[tokio::test]
async fn worktree_room_projects_boundary_and_applies_real_patches() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);

    let head = git(&wt_dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    assert!(
        String::from_utf8_lossy(&head).trim().starts_with("run/"),
        "isolated branch"
    );

    let sandbox = env.sandbox();
    let wt_str = wt_dir.to_str().unwrap().to_string();
    let dot_git_str = format!("{wt_str}/.git");
    assert!(
        sandbox
            .writable_roots()
            .iter()
            .any(|r| r.as_str() == wt_str),
        "worktree is writable"
    );
    assert!(
        sandbox
            .read_only_subpaths()
            .iter()
            .any(|r| r.as_str() == dot_git_str),
        ".git is a protected read-only subpath"
    );

    let fs = env.fs();
    let patch = "*** Begin Patch\n*** Add File: src/main.rs\n+new\n*** End Patch\n";
    let outcome = fs
        .apply_patch(patch)
        .await
        .expect("apply_patch dialect lands inside the worktree");
    let edited = wt_dir.join("src/main.rs");
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "new\n",
        "apply_patch wrote the new content (a real edit, not a no-op)"
    );
    assert!(
        outcome
            .changed_paths
            .iter()
            .any(|p| p.as_str() == edited.to_str().unwrap()),
        "the changed path is reported"
    );

    let update = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-new\n+newer\n*** End Patch\n";
    fs.apply_patch(update).await.expect("update patch lands");
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "newer\n",
        "update hunks patch the file in place instead of rewriting it wholesale"
    );

    let dot_git_patch = "*** Begin Patch\n*** Add File: .git/config\n+y\n*** End Patch\n";
    assert!(
        matches!(
            fs.apply_patch(dot_git_patch).await,
            Err(WorkerEnvError::WriteOutsideRoots(_))
        ),
        "patch touching .git fails closed before any write"
    );

    let unified = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-newer\n+nope\n";
    assert!(
        matches!(
            fs.apply_patch(unified).await,
            Err(WorkerEnvError::PatchFailed(_))
        ),
        "raw unified diffs are refused (the old hand parser rewrote whole files from hunk lines)"
    );
    assert_eq!(std::fs::read_to_string(&edited).unwrap(), "newer\n");

    let diff = git(&wt_dir, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(
        String::from_utf8_lossy(&diff).contains("src/main.rs"),
        "the untracked edit shows up in package-owned git evidence"
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn file_write_creates_parent_dirs_in_worktree_and_escape_fails_closed() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);
    let fs = env.fs();

    let nested = wt_dir.join("a/b/c.txt");
    assert!(!wt_dir.join("a/b").exists());
    let nested_abs = AbsPath::new(&nested).unwrap();
    fs.write(&nested_abs, b"hello\n")
        .await
        .expect("writing into a not-yet-existing subdir succeeds (parents created in-worktree)");
    assert_eq!(std::fs::read_to_string(&nested).unwrap(), "hello\n");

    let escape = wt_dir.join("../octo_escape_x.txt");
    let escape_abs = AbsPath::new(&escape).unwrap();
    assert!(
        matches!(
            fs.write(&escape_abs, b"nope").await,
            Err(WorkerEnvError::WriteOutsideRoots(_))
        ),
        "a write escaping the worktree fails closed"
    );
    assert!(!escape_abs.as_path().exists());

    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn tracked_edits_show_in_unified_diff_and_release_removes_the_worktree() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);

    let readme = AbsPath::new(&wt_dir.join("README.md")).unwrap();
    env.fs()
        .write(&readme, b"seed\nedited\n")
        .await
        .expect("tracked file edit lands");
    let diff = git(&wt_dir, &["diff", "HEAD", "--"]);
    let diff = String::from_utf8_lossy(&diff);
    assert!(
        diff.contains("+edited"),
        "the unified diff carries the real tracked change, not an empty stub"
    );
    git(&wt_dir, &["add", "README.md"]);
    git(
        &wt_dir,
        &[
            "-c",
            "user.name=Octopus Test",
            "-c",
            "user.email=octopus-test@example.invalid",
            "commit",
            "-m",
            "handoff",
        ],
    );
    let branch = String::from_utf8_lossy(&git(
        &wt_dir,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    ))
    .trim()
    .to_string();

    env.release().await.expect("release hook runs");
    assert!(
        !wt_dir.exists(),
        "release genuinely removes the leased worktree (no lying Ok)"
    );
    let branch_ref = format!("refs/heads/{branch}");
    let branch_preserved = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(["show-ref", "--verify", "--quiet", &branch_ref])
        .status()
        .expect("git show-ref runs")
        .success();
    assert!(
        branch_preserved,
        "release detaches the substrate but preserves the run branch as handoff"
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn dirty_release_fails_closed_without_destroying_the_worktree() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);
    std::fs::write(wt_dir.join("README.md"), "seed\nnot-yet-handed-off\n").unwrap();

    assert!(matches!(
        env.release().await,
        Err(WorkerEnvError::ReleaseFailed { .. })
    ));
    assert!(wt_dir.exists());
    assert_eq!(
        std::fs::read_to_string(wt_dir.join("README.md")).unwrap(),
        "seed\nnot-yet-handed-off\n"
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn attached_worktree_recovery_preserves_the_run_diff() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);
    let worker_environment_id = env.id().as_str().to_string();
    std::fs::write(wt_dir.join("README.md"), "seed\nrun-owned\n").unwrap();
    std::fs::write(repo_dir.join("README.md"), "seed\nlive-checkout\n").unwrap();

    let recovered = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    assert_eq!(
        std::fs::read_to_string(wt_dir.join("README.md")).unwrap(),
        "seed\nrun-owned\n"
    );

    abandon_git_worktree(
        &repo_dir,
        &test_root.join("private-worktree-state"),
        &wt_dir,
        &worker_environment_id,
        GitWorktreeEvidenceLimits::new(1024 * 1024, 100).unwrap(),
    )
    .unwrap();
    env.release().await.unwrap();
    recovered.release().await.unwrap();
    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn restarted_release_detaches_the_existing_clean_formation() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);
    let worker_environment_id = env.id().as_str().to_string();
    let branch = String::from_utf8_lossy(&git(
        &wt_dir,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    ))
    .trim()
    .to_string();
    drop(env);

    release_git_worktree(
        &repo_dir,
        &test_root.join("private-worktree-state"),
        &wt_dir,
        &worker_environment_id,
    )
    .expect("a restarted door releases the existing clean worktree");
    release_git_worktree(
        &repo_dir,
        &test_root.join("private-worktree-state"),
        &wt_dir,
        &worker_environment_id,
    )
    .expect("release replay accepts the preserved unattached branch");
    assert!(!wt_dir.exists());
    let branch_ref = format!("refs/heads/{branch}");
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_dir)
            .args(["show-ref", "--verify", "--quiet", &branch_ref])
            .status()
            .expect("git show-ref runs")
            .success()
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn abandon_refuses_a_diff_changed_after_its_durable_precommit() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);
    let worker_environment_id = env.id().as_str().to_string();
    let state_root = AbsPath::new(&test_root.join("private-worktree-state")).unwrap();
    let limits = GitWorktreeEvidenceLimits::new(1024 * 1024, 100).unwrap();
    std::fs::write(wt_dir.join("README.md"), "seed\nprecommitted\n").unwrap();
    let evidence = capture_git_worktree_evidence(&wt_dir, limits).unwrap();
    let marker = abandoned_formation_path(&state_root, &worker_environment_id);
    write_abandoned_formation(
        &marker,
        &AbandonedFormation {
            schema_version: 1,
            worker_environment_id: worker_environment_id.clone(),
            evidence: evidence.clone(),
        },
    )
    .unwrap();

    std::fs::write(wt_dir.join("README.md"), "seed\nchanged-after-precommit\n").unwrap();
    assert!(
        abandon_git_worktree(
            &repo_dir,
            state_root.as_path(),
            &wt_dir,
            &worker_environment_id,
            limits,
        )
        .is_err()
    );
    assert!(wt_dir.exists());

    std::fs::write(wt_dir.join("README.md"), "seed\nprecommitted\n").unwrap();
    let replayed = abandon_git_worktree(
        &repo_dir,
        state_root.as_path(),
        &wt_dir,
        &worker_environment_id,
        limits,
    )
    .expect("restoring the exact precommitted diff permits terminal cleanup");
    assert_eq!(replayed, evidence);
    env.release().await.unwrap();

    let _ = std::fs::remove_dir_all(&test_root);
}

#[tokio::test]
async fn worktree_evidence_covers_untracked_content_and_abandon_is_terminal() {
    let reg = registry();
    let (test_root, repo_dir, worktree_base) = unique_git_repo_and_worktree_dir();
    let env = lease_coder_worktree(&reg, &repo_dir, &worktree_base).await;
    let wt_dir = leased_worktree_path(&worktree_base, &env);
    let worker_environment_id = env.id().as_str().to_string();
    std::fs::write(wt_dir.join("README.md"), "seed\nedited\n").unwrap();
    std::fs::create_dir_all(wt_dir.join("src")).unwrap();
    std::fs::write(wt_dir.join("src/new.rs"), "pub fn added() {}\n").unwrap();
    let limits = GitWorktreeEvidenceLimits::new(1024 * 1024, 100).unwrap();

    let evidence = capture_git_worktree_evidence(&wt_dir, limits).unwrap();
    assert!(evidence.branch.starts_with("run/"));
    assert!(evidence.unified_diff.contains("+edited"));
    assert!(evidence.unified_diff.contains("+pub fn added() {}"));
    assert_eq!(
        evidence.changed_paths,
        vec!["README.md".to_string(), "src/new.rs".to_string()]
    );
    assert_eq!(
        evidence.diff_blake3,
        blake3::hash(evidence.unified_diff.as_bytes())
            .to_hex()
            .to_string()
    );

    let abandoned = abandon_git_worktree(
        &repo_dir,
        &test_root.join("private-worktree-state"),
        &wt_dir,
        &worker_environment_id,
        limits,
    )
    .unwrap();
    assert_eq!(abandoned, evidence);
    assert!(!wt_dir.exists());
    let branch_ref = format!("refs/heads/{}", abandoned.branch);
    let branch_exists = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(["show-ref", "--verify", "--quiet", &branch_ref])
        .status()
        .expect("git show-ref runs")
        .success();
    assert!(!branch_exists, "abandon deletes the run branch");
    let replayed = abandon_git_worktree(
        &repo_dir,
        &test_root.join("private-worktree-state"),
        &wt_dir,
        &worker_environment_id,
        limits,
    )
    .expect("abandon replays the precommitted evidence after substrate deletion");
    assert_eq!(replayed, evidence);
    env.release()
        .await
        .expect("release observes explicit abandon as already terminal");

    let reopened = lease_coder_worktree_result(&reg, &repo_dir, &worktree_base).await;
    assert!(
        matches!(reopened, Err(WorkerEnvError::OpenFailed { .. })),
        "an abandoned formation cannot silently recreate its branch"
    );

    let _ = std::fs::remove_dir_all(&test_root);
}

#[test]
fn projection_rejects_caps_beyond_the_kind_boundary() {
    let reg = registry();
    let (spec, _) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();
    let tmp = std::env::temp_dir();
    let tmp_abs = AbsPath::new(&tmp).unwrap();
    let ctx = EnvContext {
        additional_root_slots: Default::default(),
        root_slots: BTreeMap::from([
            ("repo_root".to_string(), tmp_abs.clone()),
            ("worktree_root".to_string(), tmp_abs.clone()),
            ("git_common_root".to_string(), tmp_abs.clone()),
            ("protected_branch_ref".to_string(), tmp_abs),
        ]),
        env: BTreeMap::new(),
    };

    let bad = WorkerEnvCaps::top().intersect_writable(&["/etc"]);
    assert!(
        matches!(
            project_sandbox(&spec, &bad, &ctx),
            Err(WorkerEnvError::CapExceedsBoundary(_))
        ),
        "a cap cannot invent a writable root"
    );

    let derived = WorkerEnvCaps::from_kind_boundary(&spec);
    let projected = project_sandbox(&spec, &derived, &ctx).unwrap();
    assert!(!projected.writable_roots().is_empty());
}
