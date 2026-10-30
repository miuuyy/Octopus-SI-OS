mod worktree_driver;

pub use worktree_driver::{
    GIT_COMMON_ROOT_SLOT, GitWorktreeEvidence, GitWorktreeEvidenceLimits, PROTECTED_BRANCH_SLOT,
    REPO_ROOT_SLOT, WORKTREE_ROOT_SLOT, abandon_git_worktree, capture_git_worktree_evidence,
    instance_worktree_path, lease_git_worktree, lease_git_worktree_with_root_slots,
    git_worktree_driver, release_git_worktree,
};
