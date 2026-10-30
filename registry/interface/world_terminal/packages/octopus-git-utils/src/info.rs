use std::collections::BTreeMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use futures::future::join_all;
use octopus_file_system::ExecutorFileSystem;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::PathUri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tokio::process::Command;
use tokio::time::Duration as TokioDuration;
use tokio::time::timeout;
use ts_rs::TS;

use crate::GitSha;

pub fn get_git_repo_root(base_dir: &Path) -> Option<PathBuf> {
    let base = if base_dir.is_dir() {
        base_dir
    } else {
        base_dir.parent()?
    };
    find_ancestor_git_entry(base).map(|(repo_root, _)| repo_root)
}

pub async fn get_git_repo_root_with_fs(
    fs: &dyn ExecutorFileSystem,
    cwd: &AbsolutePathBuf,
) -> Option<AbsolutePathBuf> {
    let cwd_uri = PathUri::from_abs_path(cwd);
    let base = match fs.get_metadata(&cwd_uri, None).await {
        Ok(metadata) if metadata.is_directory => cwd.clone(),
        _ => cwd.parent()?,
    };
    find_ancestor_git_entry_with_fs(fs, &base)
        .await
        .map(|(repo_root, _)| repo_root)
}

const GIT_COMMAND_TIMEOUT: TokioDuration = TokioDuration::from_secs(5);
const DISABLED_HOOKS_PATH: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct GitInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<GitSha>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GitDiffToRemote {
    pub sha: GitSha,
    pub diff: String,
}

pub async fn collect_git_info(cwd: &Path) -> Option<GitInfo> {
    let is_git_repo = run_git_command_with_timeout(&["rev-parse", "--git-dir"], cwd)
        .await?
        .status
        .success();

    if !is_git_repo {
        return None;
    }

    let (commit_result, branch_result, url_result) = tokio::join!(
        run_git_command_with_timeout(&["rev-parse", "HEAD"], cwd),
        run_git_command_with_timeout(&["rev-parse", "--abbrev-ref", "HEAD"], cwd),
        run_git_command_with_timeout(&["remote", "get-url", "origin"], cwd)
    );

    let mut git_info = GitInfo {
        commit_hash: None,
        branch: None,
        repository_url: None,
    };

    if let Some(output) = commit_result
        && output.status.success()
        && let Ok(hash) = String::from_utf8(output.stdout)
    {
        git_info.commit_hash = Some(GitSha::new(hash.trim()));
    }

    if let Some(output) = branch_result
        && output.status.success()
        && let Ok(branch) = String::from_utf8(output.stdout)
    {
        let branch = branch.trim();
        if branch != "HEAD" {
            git_info.branch = Some(branch.to_string());
        }
    }

    if let Some(output) = url_result
        && output.status.success()
        && let Ok(url) = String::from_utf8(output.stdout)
    {
        git_info.repository_url = Some(url.trim().to_string());
    }

    Some(git_info)
}

pub async fn get_git_remote_urls(cwd: &Path) -> Option<BTreeMap<String, String>> {
    let is_git_repo = run_git_command_with_timeout(&["rev-parse", "--git-dir"], cwd)
        .await?
        .status
        .success();
    if !is_git_repo {
        return None;
    }

    get_git_remote_urls_assume_git_repo(cwd).await
}

pub async fn get_git_remote_urls_assume_git_repo(cwd: &Path) -> Option<BTreeMap<String, String>> {
    let output = run_git_command_with_timeout(&["remote", "-v"], cwd).await?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_git_remote_urls(stdout.as_str())
}

pub async fn get_head_commit_hash(cwd: &Path) -> Option<GitSha> {
    let output = run_git_command_with_timeout(&["rev-parse", "HEAD"], cwd).await?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let hash = stdout.trim();
    if hash.is_empty() {
        None
    } else {
        Some(GitSha::new(hash))
    }
}

pub fn canonicalize_git_remote_url(url: &str) -> Option<String> {
    let url = trim_git_suffix(url.trim().trim_end_matches('/'));
    if url.is_empty() {
        return None;
    }

    if let Some((scheme, rest)) = url.split_once("://") {
        return canonicalize_git_url_like_remote(scheme, rest);
    }

    if let Some((host_part, path)) = parse_scp_like_remote(url) {
        return canonicalize_git_remote_host_path(host_part, path, None);
    }

    let (host_part, path) = url.split_once('/')?;
    canonicalize_git_remote_host_path(host_part, path, None)
}

fn canonicalize_git_url_like_remote(scheme: &str, rest: &str) -> Option<String> {
    let default_port = match scheme {
        "git" => Some("9418"),
        "http" => Some("80"),
        "https" => Some("443"),
        "ssh" => Some("22"),
        _ => return None,
    };

    let rest = rest
        .find(['?', '#'])
        .map_or(rest, |suffix_index| &rest[..suffix_index]);
    let (host_part, path) = rest.split_once('/')?;
    canonicalize_git_remote_host_path(host_part, path, default_port)
}

fn parse_scp_like_remote(remote: &str) -> Option<(&str, &str)> {
    if remote.contains('/')
        && remote
            .find('/')
            .is_some_and(|slash| remote.find(':').is_none_or(|colon| slash < colon))
    {
        return None;
    }

    let (host_part, path) = remote.split_once(':')?;
    if host_part.is_empty() || path.is_empty() {
        return None;
    }
    Some((host_part, path))
}

fn canonicalize_git_remote_host_path(
    host_part: &str,
    path: &str,
    default_port: Option<&str>,
) -> Option<String> {
    let host = normalize_remote_host(
        host_part
            .rsplit_once('@')
            .map_or(host_part, |(_, host)| host)
            .trim()
            .trim_end_matches('/'),
        default_port,
    );
    if host.is_empty() {
        return None;
    }

    let path = trim_git_suffix(path.trim().trim_matches('/'));
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let [owner, repo, ..] = components.as_slice() else {
        return None;
    };
    if matches!((*owner, *repo), ("." | "..", _) | (_, "." | "..")) {
        return None;
    }
    let path = components.join("/");

    if host == "github.com" {
        Some(format!("{host}/{}", path.to_ascii_lowercase()))
    } else {
        Some(format!("{host}/{path}"))
    }
}

fn normalize_remote_host(host: &str, default_port: Option<&str>) -> String {
    let host = host.to_ascii_lowercase();
    if let Some(default_port) = default_port
        && let Some((host_without_port, port)) = host.rsplit_once(':')
        && port == default_port
    {
        return host_without_port.to_string();
    }
    host
}

fn trim_git_suffix(value: &str) -> &str {
    value.strip_suffix(".git").unwrap_or(value)
}

pub async fn get_has_changes(cwd: &Path) -> Option<bool> {
    let git = Path::new("git");
    let fsmonitor = detect_local_fsmonitor_override(git, cwd).await;
    let output =
        run_git_command_with_timeout_from(git, &["status", "--porcelain"], cwd, fsmonitor).await?;
    if !output.status.success() {
        return None;
    }

    Some(!output.stdout.is_empty())
}

fn parse_git_remote_urls(stdout: &str) -> Option<BTreeMap<String, String>> {
    let mut remotes = BTreeMap::new();
    for line in stdout.lines() {
        let Some(fetch_line) = line.strip_suffix(" (fetch)") else {
            continue;
        };

        let Some((name, url_part)) = fetch_line
            .split_once('\t')
            .or_else(|| fetch_line.split_once(' '))
        else {
            continue;
        };

        let url = url_part.trim_start();
        if !url.is_empty() {
            remotes.insert(name.to_string(), url.to_string());
        }
    }

    if remotes.is_empty() {
        None
    } else {
        Some(remotes)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitLogEntry {
    pub sha: String,
    pub timestamp: i64,
    pub subject: String,
}

pub async fn recent_commits(cwd: &Path, limit: usize) -> Vec<CommitLogEntry> {
    let Some(out) = run_git_command_with_timeout(&["rev-parse", "--git-dir"], cwd).await else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }

    let fmt = "%H%x1f%ct%x1f%s";
    let limit_arg = (limit > 0).then(|| limit.to_string());
    let mut args: Vec<String> = vec!["log".to_string()];
    if let Some(n) = &limit_arg {
        args.push("-n".to_string());
        args.push(n.clone());
    }
    args.push(format!("--pretty=format:{fmt}"));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(log_out) = run_git_command_with_timeout(&arg_refs, cwd).await else {
        return Vec::new();
    };
    if !log_out.status.success() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&log_out.stdout);
    let mut entries: Vec<CommitLogEntry> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\u{001f}');
        let sha = parts.next().unwrap_or("").trim();
        let ts_s = parts.next().unwrap_or("").trim();
        let subject = parts.next().unwrap_or("").trim();
        if sha.is_empty() || ts_s.is_empty() {
            continue;
        }
        let timestamp = ts_s.parse::<i64>().unwrap_or(0);
        entries.push(CommitLogEntry {
            sha: sha.to_string(),
            timestamp,
            subject: subject.to_string(),
        });
    }

    entries
}

pub async fn git_diff_to_remote(cwd: &Path) -> Option<GitDiffToRemote> {
    get_git_repo_root(cwd)?;

    let remotes = get_git_remotes(cwd).await?;
    let branches = branch_ancestry(cwd).await?;
    let base_sha = find_closest_sha(cwd, &branches, &remotes).await?;
    let diff = diff_against_sha(cwd, &base_sha).await?;

    Some(GitDiffToRemote {
        sha: base_sha,
        diff,
    })
}

async fn run_git_command_with_timeout(args: &[&str], cwd: &Path) -> Option<std::process::Output> {
    run_git_command_with_timeout_from(
        Path::new("git"),
        args,
        cwd,
        crate::FsmonitorOverride::Disabled,
    )
    .await
}

struct LocalFsmonitorProbeRunner<'a> {
    git: &'a Path,
    cwd: &'a Path,
}

impl crate::FsmonitorProbeRunner for LocalFsmonitorProbeRunner<'_> {
    async fn run_probe(&mut self, args: &[&str]) -> Option<Vec<u8>> {
        let mut command = Command::new(self.git);
        command.args(args).current_dir(self.cwd).kill_on_drop(true);
        match timeout(GIT_COMMAND_TIMEOUT, command.output()).await {
            Ok(Ok(output)) if output.status.success() => Some(output.stdout),
            _ => None,
        }
    }
}

async fn detect_local_fsmonitor_override(git: &Path, cwd: &Path) -> crate::FsmonitorOverride {
    let mut runner = LocalFsmonitorProbeRunner { git, cwd };
    crate::detect_fsmonitor_override(&mut runner).await
}

async fn run_git_command_with_timeout_from(
    git: &Path,
    args: &[&str],
    cwd: &Path,
    fsmonitor: crate::FsmonitorOverride,
) -> Option<std::process::Output> {
    let mut command = Command::new(git);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", &format!("core.hooksPath={DISABLED_HOOKS_PATH}")])
        .args(["-c", fsmonitor.git_config_arg()])
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true);
    let result = timeout(GIT_COMMAND_TIMEOUT, command.output()).await;

    match result {
        Ok(Ok(output)) => Some(output),
        _ => None,
    }
}

async fn get_git_remotes(cwd: &Path) -> Option<Vec<String>> {
    let output = run_git_command_with_timeout(&["remote"], cwd).await?;
    if !output.status.success() {
        return None;
    }
    let mut remotes: Vec<String> = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::to_string)
        .collect();
    if let Some(pos) = remotes.iter().position(|r| r == "origin") {
        let origin = remotes.remove(pos);
        remotes.insert(0, origin);
    }
    Some(remotes)
}

async fn get_default_branch(cwd: &Path) -> Option<String> {
    let remotes = get_git_remotes(cwd).await.unwrap_or_default();
    for remote in remotes {
        if let Some(symref_output) = run_git_command_with_timeout(
            &[
                "symbolic-ref",
                "--quiet",
                &format!("refs/remotes/{remote}/HEAD"),
            ],
            cwd,
        )
        .await
            && symref_output.status.success()
            && let Ok(sym) = String::from_utf8(symref_output.stdout)
        {
            let trimmed = sym.trim();
            if let Some((_, name)) = trimmed.rsplit_once('/') {
                return Some(name.to_string());
            }
        }

        if let Some(show_output) =
            run_git_command_with_timeout(&["remote", "show", &remote], cwd).await
            && show_output.status.success()
            && let Ok(text) = String::from_utf8(show_output.stdout)
        {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("HEAD branch:") {
                    let name = rest.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }

    get_default_branch_local(cwd).await
}

pub async fn default_branch_name(cwd: &Path) -> Option<String> {
    get_default_branch(cwd).await
}

async fn get_default_branch_local(cwd: &Path) -> Option<String> {
    for candidate in ["main", "master"] {
        if let Some(verify) = run_git_command_with_timeout(
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{candidate}"),
            ],
            cwd,
        )
        .await
            && verify.status.success()
        {
            return Some(candidate.to_string());
        }
    }

    None
}

async fn branch_ancestry(cwd: &Path) -> Option<Vec<String>> {
    let current_branch = run_git_command_with_timeout(&["rev-parse", "--abbrev-ref", "HEAD"], cwd)
        .await
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| s != "HEAD");

    let default_branch = get_default_branch(cwd).await;

    let mut ancestry: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(cb) = current_branch.clone() {
        seen.insert(cb.clone());
        ancestry.push(cb);
    }
    if let Some(db) = default_branch
        && !seen.contains(&db)
    {
        seen.insert(db.clone());
        ancestry.push(db);
    }

    let remotes = get_git_remotes(cwd).await.unwrap_or_default();
    for remote in remotes {
        if let Some(output) = run_git_command_with_timeout(
            &[
                "for-each-ref",
                "--format=%(refname:short)",
                "--contains=HEAD",
                &format!("refs/remotes/{remote}"),
            ],
            cwd,
        )
        .await
            && output.status.success()
            && let Ok(text) = String::from_utf8(output.stdout)
        {
            for line in text.lines() {
                let short = line.trim();
                if let Some(stripped) = short.strip_prefix(&format!("{remote}/"))
                    && !stripped.is_empty()
                    && !seen.contains(stripped)
                {
                    seen.insert(stripped.to_string());
                    ancestry.push(stripped.to_string());
                }
            }
        }
    }

    Some(ancestry)
}

async fn branch_remote_and_distance(
    cwd: &Path,
    branch: &str,
    remotes: &[String],
) -> Option<(Option<GitSha>, usize)> {
    let mut found_remote_sha: Option<GitSha> = None;
    let mut found_remote_ref: Option<String> = None;
    for remote in remotes {
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        let Some(verify_output) =
            run_git_command_with_timeout(&["rev-parse", "--verify", "--quiet", &remote_ref], cwd)
                .await
        else {
            return None;
        };
        if !verify_output.status.success() {
            continue;
        }
        let Ok(sha) = String::from_utf8(verify_output.stdout) else {
            return None;
        };
        found_remote_sha = Some(GitSha::new(sha.trim()));
        found_remote_ref = Some(remote_ref);
        break;
    }

    let count_output = if let Some(local_count) =
        run_git_command_with_timeout(&["rev-list", "--count", &format!("{branch}..HEAD")], cwd)
            .await
    {
        if local_count.status.success() {
            local_count
        } else if let Some(remote_ref) = &found_remote_ref {
            match run_git_command_with_timeout(
                &["rev-list", "--count", &format!("{remote_ref}..HEAD")],
                cwd,
            )
            .await
            {
                Some(remote_count) => remote_count,
                None => return None,
            }
        } else {
            return None;
        }
    } else if let Some(remote_ref) = &found_remote_ref {
        match run_git_command_with_timeout(
            &["rev-list", "--count", &format!("{remote_ref}..HEAD")],
            cwd,
        )
        .await
        {
            Some(remote_count) => remote_count,
            None => return None,
        }
    } else {
        return None;
    };

    if !count_output.status.success() {
        return None;
    }
    let Ok(distance_str) = String::from_utf8(count_output.stdout) else {
        return None;
    };
    let Ok(distance) = distance_str.trim().parse::<usize>() else {
        return None;
    };

    Some((found_remote_sha, distance))
}

async fn find_closest_sha(cwd: &Path, branches: &[String], remotes: &[String]) -> Option<GitSha> {
    let mut closest_sha: Option<(GitSha, usize)> = None;
    for branch in branches {
        let Some((maybe_remote_sha, distance)) =
            branch_remote_and_distance(cwd, branch, remotes).await
        else {
            continue;
        };
        let Some(remote_sha) = maybe_remote_sha else {
            continue;
        };
        match &closest_sha {
            None => closest_sha = Some((remote_sha, distance)),
            Some((_, best_distance)) if distance < *best_distance => {
                closest_sha = Some((remote_sha, distance));
            }
            _ => {}
        }
    }
    closest_sha.map(|(sha, _)| sha)
}

async fn diff_against_sha(cwd: &Path, sha: &GitSha) -> Option<String> {
    let git = Path::new("git");
    let fsmonitor = detect_local_fsmonitor_override(git, cwd).await;
    let output = run_git_command_with_timeout_from(
        git,
        &["diff", "--no-textconv", "--no-ext-diff", &sha.0],
        cwd,
        fsmonitor,
    )
    .await?;
    let exit_ok = output.status.code().is_some_and(|c| c == 0 || c == 1);
    if !exit_ok {
        return None;
    }
    let mut diff = String::from_utf8(output.stdout).ok()?;

    if let Some(untracked_output) = run_git_command_with_timeout_from(
        git,
        &["ls-files", "--others", "--exclude-standard"],
        cwd,
        fsmonitor,
    )
    .await
        && untracked_output.status.success()
    {
        let untracked: Vec<String> = String::from_utf8(untracked_output.stdout)
            .ok()?
            .lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect();

        if !untracked.is_empty() {
            let null_device: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };
            let futures_iter = untracked.into_iter().map(|file| async move {
                let file_owned = file;
                let args_vec: Vec<&str> = vec![
                    "diff",
                    "--no-textconv",
                    "--no-ext-diff",
                    "--binary",
                    "--no-index",
                    "--",
                    null_device,
                    &file_owned,
                ];
                run_git_command_with_timeout_from(git, &args_vec, cwd, fsmonitor).await
            });
            let results = join_all(futures_iter).await;
            for extra in results.into_iter().flatten() {
                if extra.status.code().is_some_and(|c| c == 0 || c == 1)
                    && let Ok(s) = String::from_utf8(extra.stdout)
                {
                    diff.push_str(&s);
                }
            }
        }
    }

    Some(diff)
}

pub async fn resolve_root_git_project_for_trust(
    fs: &dyn ExecutorFileSystem,
    cwd: &AbsolutePathBuf,
) -> Option<AbsolutePathBuf> {
    let repo_root = get_git_repo_root_with_fs(fs, cwd).await?;
    let dot_git = repo_root.join(".git");
    let dot_git_uri = PathUri::from_abs_path(&dot_git);
    if fs.get_metadata(&dot_git_uri, None).await.ok()?.is_directory {
        return Some(repo_root);
    }

    let git_dir_s = fs.read_file_text(&dot_git_uri, None).await.ok()?;
    let git_dir_rel = git_dir_s.trim().strip_prefix("gitdir:")?.trim();
    if git_dir_rel.is_empty() {
        return None;
    }

    let git_dir_path = AbsolutePathBuf::resolve_path_against_base(git_dir_rel, repo_root.as_path());
    let worktrees_dir = git_dir_path.parent()?;
    if worktrees_dir.as_path().file_name() != Some(OsStr::new("worktrees")) {
        return None;
    }

    let common_dir = worktrees_dir.parent()?;
    common_dir.parent()
}

fn find_ancestor_git_entry(base_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut dir = base_dir.to_path_buf();

    loop {
        let dot_git = dir.join(".git");
        if dot_git.exists() {
            return Some((dir, dot_git));
        }

        if !dir.pop() {
            break;
        }
    }

    None
}

async fn find_ancestor_git_entry_with_fs(
    fs: &dyn ExecutorFileSystem,
    base_dir: &AbsolutePathBuf,
) -> Option<(AbsolutePathBuf, AbsolutePathBuf)> {
    for dir in base_dir.ancestors() {
        let dot_git = dir.join(".git");
        let dot_git_uri = PathUri::from_abs_path(&dot_git);
        if fs.get_metadata(&dot_git_uri, None).await.is_ok() {
            return Some((dir, dot_git));
        }
    }
    None
}

pub async fn local_git_branches(cwd: &Path) -> Vec<String> {
    let mut branches: Vec<String> = if let Some(out) =
        run_git_command_with_timeout(&["branch", "--format=%(refname:short)"], cwd).await
        && out.status.success()
    {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    branches.sort_unstable();

    if let Some(base) = get_default_branch_local(cwd).await
        && let Some(pos) = branches.iter().position(|name| name == &base)
    {
        let base_branch = branches.remove(pos);
        branches.insert(0, base_branch);
    }

    branches
}

pub async fn current_branch_name(cwd: &Path) -> Option<String> {
    let out = run_git_command_with_timeout(&["branch", "--show-current"], cwd).await?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn canonicalize_git_remote_url_normalizes_github_variants() {
        for remote in [
            "git@github.com:OpenAI/Codex.git",
            "ssh://git@github.com/openai/codex.git",
            "ssh://git@github.com:22/OpenAI/Codex.git",
            "https://github.com/openai/codex.git",
            "https://github.com:443/openai/codex.git",
            "https://token@github.com/openai/codex/",
            "github.com/OpenAI/Codex.git",
        ] {
            assert_eq!(
                canonicalize_git_remote_url(remote),
                Some("github.com/openai/codex".to_string())
            );
        }
    }

    #[test]
    fn canonicalize_git_remote_url_handles_ghe_without_lowercasing_path() {
        assert_eq!(
            canonicalize_git_remote_url("git@ghe.company.com:Org/Repo.git"),
            Some("ghe.company.com/Org/Repo".to_string())
        );
        assert_eq!(
            canonicalize_git_remote_url("ssh://git@ghe.company.com:2222/Org/Repo.git"),
            Some("ghe.company.com:2222/Org/Repo".to_string())
        );
    }

    #[test]
    fn canonicalize_git_remote_url_rejects_non_repository_values() {
        for remote in ["", "file:///tmp/repo", "github.com/openai", "/tmp/repo"] {
            assert_eq!(canonicalize_git_remote_url(remote), None);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fsmonitor_override_rejects_configured_helper() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let git = temp_dir.path().join("git");
        let log = temp_dir.path().join("git.log");
        std::fs::write(
            &git,
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >>\"$0.log\"\n\
             case \"$1\" in\n\
             config) printf '/tmp/fsmonitor-helper\\000' ;;\n\
             *) printf 'worktree output\\n' ;;\n\
             esac\n",
        )
        .expect("write fake Git");
        let mut permissions = std::fs::metadata(&git)
            .expect("read fake Git metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("mark fake Git executable");

        let fsmonitor = detect_local_fsmonitor_override(&git, temp_dir.path()).await;
        let output = run_git_command_with_timeout_from(
            &git,
            &["status", "--porcelain"],
            temp_dir.path(),
            fsmonitor,
        )
        .await
        .expect("run fake Git");

        assert_eq!(
            (output.status.code(), output.stdout),
            (Some(0), b"worktree output\n".to_vec())
        );
        let disabled_hooks = format!("core.hooksPath={DISABLED_HOOKS_PATH}");
        assert_eq!(
            std::fs::read_to_string(log)
                .expect("read fake Git log")
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            vec![
                "config --null --get core.fsmonitor".to_string(),
                "config --null --type=bool --fixed-value --get core.fsmonitor /tmp/fsmonitor-helper"
                    .to_string(),
                format!("-c {disabled_hooks} -c core.fsmonitor=false status --porcelain"),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fsmonitor_override_uses_effective_layered_config_value() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let repo = temp_dir.path().join("repo");
        std::fs::create_dir(&repo).expect("create repository directory");
        let init_status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .expect("initialize test repository");
        assert_eq!(init_status.code(), Some(0), "initialize test repository");

        let git = temp_dir.path().join("git");
        let global_config = temp_dir.path().join("git.global");
        let log = temp_dir.path().join("git.log");
        std::fs::write(
            &git,
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >>\"$0.log\"\n\
             case \"$1\" in\n\
             config)\n\
               GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=\"$0.global\" exec git \"$@\"\n\
               ;;\n\
             version) printf 'feature: fsmonitor--daemon\\n' ;;\n\
             *) printf 'worktree output\\n' ;;\n\
             esac\n",
        )
        .expect("write layered-config Git");
        let mut permissions = std::fs::metadata(&git)
            .expect("read layered-config Git metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("mark layered-config Git executable");

        let global_status = std::process::Command::new("git")
            .args([
                "config",
                "--file",
                global_config.to_str().expect("global config path"),
                "core.fsmonitor",
                "/tmp/fsmonitor-helper",
            ])
            .status()
            .expect("write global fsmonitor helper");
        assert_eq!(
            global_status.code(),
            Some(0),
            "write global fsmonitor helper"
        );
        let local_status = std::process::Command::new("git")
            .args(["config", "core.fsmonitor", "true"])
            .current_dir(&repo)
            .status()
            .expect("write local built-in fsmonitor config");
        assert_eq!(
            local_status.code(),
            Some(0),
            "write local built-in fsmonitor config"
        );

        let fsmonitor = detect_local_fsmonitor_override(&git, repo.as_path()).await;
        let output = run_git_command_with_timeout_from(
            &git,
            &["status", "--porcelain"],
            repo.as_path(),
            fsmonitor,
        )
        .await
        .expect("run Git with layered config");
        assert_eq!(
            (output.status.code(), output.stdout),
            (Some(0), b"worktree output\n".to_vec())
        );

        let actual = std::fs::read_to_string(log).expect("read layered-config Git log");
        let disabled_hooks = format!("core.hooksPath={DISABLED_HOOKS_PATH}");
        assert_eq!(
            actual.lines().map(str::to_string).collect::<Vec<_>>(),
            vec![
                "config --null --get core.fsmonitor".to_string(),
                "version --build-options".to_string(),
                format!("-c {disabled_hooks} -c core.fsmonitor=true status --porcelain"),
            ]
        );
    }
}
