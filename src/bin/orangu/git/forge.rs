// Copyright (C) 2026 The orangu community
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use anyhow::{Context, Result, anyhow};
use std::{fs, path::Path};

use super::*;
use crate::commands::{CloseTarget, CommentBody, GetCommentsTarget};
use crate::render::{ANSI_FG_RESET, ANSI_FG_SUBTLE};

/// An open pull request (GitHub) or merge request (GitLab), reduced to the number
/// used by `/pull <number>` and its title. Fetched once at startup (see
/// [`fetch_active_pull_requests`]) and cached in memory so `/pull` completion can
/// offer numbers without hitting the network on every keystroke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
}

/// Fetch the open pull/merge requests for the repository containing `workspace`
/// via the forge CLI (`gh pr list` / `glab mr list`), as JSON.
///
/// Returns an empty list — never an error — when the workspace is not a Git
/// repository or the CLI is not installed, so a missing `gh`/`glab` cannot break
/// startup. Only a CLI that runs but exits non-zero (e.g. no forge remote, not
/// authenticated) is surfaced as `Err` for the caller to report.
pub fn fetch_active_pull_requests(workspace: &Path, forge: Forge) -> Result<Vec<PullRequest>> {
    let Some(repo_root) = discover_git_root(workspace) else {
        return Ok(Vec::new());
    };
    let cli = forge.cli();
    let request = match forge {
        Forge::GitHub => "pr",
        Forge::GitLab => "mr",
    };
    // GitHub: `gh pr list --state open --json number,title`.
    // GitLab: `glab mr list --output json` (open merge requests by default).
    let args: Vec<&str> = match forge {
        Forge::GitHub => vec![request, "list", "--state", "open", "--json", "number,title"],
        Forge::GitLab => vec![request, "list", "--output", "json"],
    };
    let output = match std::process::Command::new(cli)
        .args(&args)
        .current_dir(&repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context(format!("failed to run {cli}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "{cli} {request} list failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    parse_pull_request_list(&output.stdout, forge)
}

/// Parse the JSON array printed by `gh pr list --json number,title` /
/// `glab mr list --output json` into [`PullRequest`]s. GitHub names the number
/// `number`, GitLab names it `iid`; both carry a `title`. Empty output yields an
/// empty list, and entries missing the number field are skipped rather than
/// failing the whole parse.
pub fn parse_pull_request_list(stdout: &[u8], forge: Forge) -> Result<Vec<PullRequest>> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).context("failed to parse forge pull request list as JSON")?;
    let number_key = match forge {
        Forge::GitHub => "number",
        Forge::GitLab => "iid",
    };
    let requests = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let number = entry.get(number_key)?.as_u64()?;
            let title = entry
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(PullRequest { number, title })
        })
        .collect();
    Ok(requests)
}

pub fn pull_request_output(
    workspace: &Path,
    pr_number: u64,
    forge: Forge,
) -> Result<Option<String>> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("pull is only available inside a Git repository"))?;
    if try_forge_pr_checkout(&repo_root, pr_number, forge)?.is_none() {
        git_pr_checkout(&repo_root, pr_number)?;
    }
    Ok(pr_sync_advice(&repo_root))
}

/// The number of commits on the default base branch (main/master) that the
/// checked-out branch has not incorporated, together with the base ref name.
/// `0` means the branch is up to date (rebased) against the base. Compared
/// against the locally known base ref — nothing is fetched.
pub fn behind_default_branch(workspace: &Path) -> Result<(usize, String)> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("review is only available inside a Git repository"))?;
    let base_ref = git_find_base_ref(&repo_root)?;
    let behind = git_commit_count(&repo_root, &format!("HEAD..{base_ref}"))?;
    Ok((behind, base_ref))
}

/// Build the rebase/squash hint lines for the checked-out branch relative to the
/// default base branch. Each entry is one line such as
/// "branch is 2 commits behind origin/main; run /rebase". Returns an empty `Vec`
/// when the branch is up to date with at most a single commit ahead.
fn pr_sync_notes(repo_root: &Path, base_ref: &str) -> Result<Vec<String>> {
    // Commits on the base branch the branch has not yet incorporated.
    let behind = git_commit_count(repo_root, &format!("HEAD..{base_ref}"))?;
    // Commits the branch adds on top of the merge base.
    let ahead = git_commit_count(repo_root, &format!("{base_ref}..HEAD"))?;

    let mut notes = Vec::new();
    if behind > 0 {
        notes.push(format!(
            "branch is {behind} commit{} behind {base_ref}; run /rebase",
            if behind == 1 { "" } else { "s" }
        ));
    }
    if ahead > 1 {
        notes.push(format!("{ahead} commits ahead of {base_ref}; run /squash"));
    }
    Ok(notes)
}

/// Report whether the checked-out branch would benefit from a rebase and/or
/// squash against the default base branch before it can be merged. Returns
/// `None` when the branch is already up to date with a single commit, or when
/// the base branch cannot be determined.
pub(crate) fn pr_sync_advice(repo_root: &Path) -> Option<String> {
    let base_ref = git_find_base_ref(repo_root).ok()?;
    let notes = pr_sync_notes(repo_root, &base_ref).ok()?;
    if notes.is_empty() {
        return None;
    }
    Some(format!(
        "This pull request needs attention:\n- {}",
        notes.join("\n- ")
    ))
}

pub fn try_forge_pr_checkout(
    repo_root: &Path,
    pr_number: u64,
    forge: Forge,
) -> Result<Option<String>> {
    let cli = forge.cli();
    // `gh pr checkout N` and `glab mr checkout N` are spelled the same way
    // apart from the request noun.
    let request = match forge {
        Forge::GitHub => "pr",
        Forge::GitLab => "mr",
    };
    let output = match std::process::Command::new(cli)
        .args([request, "checkout", &pr_number.to_string()])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context(format!("failed to run {cli}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "{cli} {request} checkout failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut combined = stdout;
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok(Some(if combined.is_empty() {
        format!("Checked out pull request #{pr_number}")
    } else {
        combined
    }))
}

pub fn git_pr_checkout(repo_root: &Path, pr_number: u64) -> Result<String> {
    let branch = format!("pr-{pr_number}");
    let fetch = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "fetch",
            "origin",
            "--force",
            &format!("pull/{pr_number}/head:{branch}"),
        ])
        .output()
        .context("failed to run git fetch")?;
    if !fetch.status.success() {
        let stderr = String::from_utf8_lossy(&fetch.stderr).trim().to_string();
        return Err(anyhow!(
            "git fetch failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let checkout = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["checkout", &branch])
        .output()
        .context("failed to run git checkout")?;
    if !checkout.status.success() {
        let stderr = String::from_utf8_lossy(&checkout.stderr).trim().to_string();
        return Err(anyhow!(
            "git checkout failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let mut parts = Vec::new();
    let fetch_stderr = String::from_utf8_lossy(&fetch.stderr).trim().to_string();
    if !fetch_stderr.is_empty() {
        parts.push(fetch_stderr);
    }
    let checkout_stderr = String::from_utf8_lossy(&checkout.stderr).trim().to_string();
    if !checkout_stderr.is_empty() {
        parts.push(checkout_stderr);
    }
    Ok(if parts.is_empty() {
        format!("Switched to branch '{branch}'")
    } else {
        parts.join("\n")
    })
}

/// The last `/review` summary and `/auto_review` report (Markdown), offered
/// to `/comment` as comment bodies (`with review`, `with auto review`).
/// `None` until the matching mode has been run in this session.
#[derive(Clone, Copy, Default)]
pub struct ReviewReports<'a> {
    pub review: Option<&'a str>,
    pub auto_review: Option<&'a str>,
}

pub fn comment_output(
    workspace: &Path,
    issue_number: u64,
    body: &CommentBody<'_>,
    reports: ReviewReports<'_>,
    forge: Forge,
) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("comment is only available inside a Git repository"))?;
    let body_text: String = match body {
        CommentBody::Inline(s) => s.to_string(),
        CommentBody::File(filename) => {
            let path = home::home_dir()
                .ok_or_else(|| anyhow!("failed to resolve home directory"))?
                .join(".orangu/comments")
                .join(filename.as_ref());
            fs::read_to_string(&path)
                .with_context(|| format!("failed to read comment file {}", path.display()))?
        }
        CommentBody::Review => reports
            .review
            .ok_or_else(|| anyhow!("no review report available — run /review first"))?
            .to_string(),
        CommentBody::AutoReview => reports
            .auto_review
            .ok_or_else(|| anyhow!("no auto review report available — run /auto_review first"))?
            .to_string(),
    };
    let cli = forge.cli();
    let number = issue_number.to_string();
    // GitHub: `gh issue comment N --body B`. GitLab: `glab issue note N --message B`.
    let args: Vec<&str> = match forge {
        Forge::GitHub => vec!["issue", "comment", &number, "--body", &body_text],
        Forge::GitLab => vec!["issue", "note", &number, "--message", &body_text],
    };
    let output = match std::process::Command::new(cli)
        .args(&args)
        .current_dir(&repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!("comment requires the {cli} CLI to be installed"));
        }
        Err(err) => return Err(err).context(format!("failed to run {cli}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "{cli} issue comment failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Added comment on issue #{issue_number}")
    } else {
        stdout
    })
}

pub fn close_output(workspace: &Path, target: &CloseTarget, forge: Forge) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("close is only available inside a Git repository"))?;
    let cli = forge.cli();
    let number = match target {
        CloseTarget::Issue(n) | CloseTarget::PullRequest(n) => n.to_string(),
    };
    // GitHub: `gh issue close N` / `gh pr close N`
    // GitLab: `glab issue close N` / `glab mr close N`
    let args: Vec<&str> = match (forge, target) {
        (Forge::GitHub, CloseTarget::Issue(_)) => vec!["issue", "close", &number],
        (Forge::GitHub, CloseTarget::PullRequest(_)) => vec!["pr", "close", &number],
        (Forge::GitLab, CloseTarget::Issue(_)) => vec!["issue", "close", &number],
        (Forge::GitLab, CloseTarget::PullRequest(_)) => vec!["mr", "close", &number],
    };
    let output = match std::process::Command::new(cli)
        .args(&args)
        .current_dir(&repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!("close requires the {cli} CLI to be installed"));
        }
        Err(err) => return Err(err).context(format!("failed to run {cli}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "{cli} close failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// One comment on an issue or pull/merge request, as fetched by
/// [`get_comments_output`].
#[derive(Debug, PartialEq, Eq)]
pub struct IssueComment {
    pub author: String,
    pub date: String,
    pub body: String,
}

pub fn get_comments_output(
    workspace: &Path,
    target: &GetCommentsTarget,
    forge: Forge,
) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("get_comments is only available inside a Git repository"))?;
    let cli = forge.cli();
    let number = match target {
        GetCommentsTarget::Issue(n) | GetCommentsTarget::PullRequest(n) => n.to_string(),
    };
    // Neither CLI has a comment listing subcommand that covers everything, so go
    // through `gh api` / `glab api`, which resolve `{owner}/{repo}` / `:id` to
    // the current repository. A GitHub pull request keeps its conversation
    // comments on the issues endpoint and its inline review comments on the
    // pulls endpoint, so both are fetched and merged; GitLab keeps inline diff
    // notes in the same notes list as discussion notes.
    let endpoints: Vec<String> = match (forge, target) {
        (Forge::GitHub, GetCommentsTarget::Issue(_)) => vec![format!(
            "repos/{{owner}}/{{repo}}/issues/{number}/comments?per_page=100"
        )],
        (Forge::GitHub, GetCommentsTarget::PullRequest(_)) => vec![
            format!("repos/{{owner}}/{{repo}}/issues/{number}/comments?per_page=100"),
            format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments?per_page=100"),
        ],
        (Forge::GitLab, GetCommentsTarget::Issue(_)) => {
            vec![format!("projects/:id/issues/{number}/notes?per_page=100")]
        }
        (Forge::GitLab, GetCommentsTarget::PullRequest(_)) => vec![format!(
            "projects/:id/merge_requests/{number}/notes?per_page=100"
        )],
    };
    let mut comments = Vec::new();
    for endpoint in &endpoints {
        let output = match std::process::Command::new(cli)
            .args(["api", endpoint])
            .current_dir(&repo_root)
            .output()
        {
            Ok(output) => output,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!(
                    "get_comments requires the {cli} CLI to be installed"
                ));
            }
            Err(err) => return Err(err).context(format!("failed to run {cli}")),
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(anyhow!(
                "{cli} get_comments failed{}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            ));
        }
        comments.extend(parse_comment_list(&output.stdout, forge)?);
    }
    // Conversation and review comments come from separate endpoints; interleave
    // them chronologically. The formatted dates sort lexicographically.
    comments.sort_by(|a, b| a.date.cmp(&b.date));
    let label = match target {
        GetCommentsTarget::Issue(_) => "issue",
        GetCommentsTarget::PullRequest(_) => "pull request",
    };
    if comments.is_empty() {
        return Ok(format!("No comments on {label} #{number}"));
    }
    Ok(format_comment_blocks(&comments))
}

/// Parse a JSON comment array printed by `gh api` (issue conversation comments
/// or pull request review comments; the author is under `user`) or by
/// `glab api .../notes` (the author is under `author`) into [`IssueComment`]s.
/// GitLab system notes (label changes, assignments, ...) are skipped; only
/// comments written by a person remain.
pub fn parse_comment_list(stdout: &[u8], forge: Forge) -> Result<Vec<IssueComment>> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).context("failed to parse forge comment list as JSON")?;
    let (author_key, name_key) = match forge {
        Forge::GitHub => ("user", "login"),
        Forge::GitLab => ("author", "username"),
    };
    let Some(entries) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter(|entry| {
            forge == Forge::GitHub
                || !entry
                    .get("system")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
        })
        .map(|entry| IssueComment {
            author: entry
                .get(author_key)
                .and_then(|author| author.get(name_key))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            date: format_comment_date(
                entry
                    .get("created_at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ),
            body: entry
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
        })
        .collect())
}

/// Render comments as blocks separated by blank lines: a subtle
/// `● <date> <author>` header line, then the body indented two spaces so it
/// aligns under the date and stands out against the grey header.
fn format_comment_blocks(comments: &[IssueComment]) -> String {
    comments
        .iter()
        .map(|comment| {
            let body = comment
                .body
                .lines()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{ANSI_FG_SUBTLE}● {} {}{ANSI_FG_RESET}\n{body}",
                comment.date, comment.author
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Turn an ISO 8601 timestamp (`2026-06-01T12:30:45Z`, with or without
/// fractional seconds) into `2026-06-01 12:30:45`. Anything that does not look
/// like one is returned unchanged.
fn format_comment_date(date: &str) -> String {
    let bytes = date.as_bytes();
    if bytes.len() >= 19 && bytes[10] == b'T' {
        format!("{} {}", &date[..10], &date[11..19])
    } else {
        date.to_string()
    }
}

pub fn create_pull_request_output(
    workspace: &Path,
    auto_rebase: bool,
    auto_squash: bool,
    forge: Forge,
) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("pull_request is only available inside a Git repository"))?;
    let current = git_current_branch(&repo_root)?;
    if is_protected_branch(&current) {
        return Err(anyhow!(
            "cannot create a pull request from the '{}' branch",
            current
        ));
    }
    let base_ref = git_find_base_ref(&repo_root)?;
    let ahead = git_commit_count(&repo_root, &format!("{base_ref}..HEAD"))?;
    if ahead == 0 {
        return Err(anyhow!(
            "no commits ahead of {base_ref}; make at least one commit before opening a pull request"
        ));
    }
    // Apply any configured auto-fixes before re-checking the branch state.
    let behind = git_commit_count(&repo_root, &format!("HEAD..{base_ref}"))?;
    if behind > 0 && auto_rebase {
        rebase_output(workspace, None, forge)?;
    }
    let ahead = git_commit_count(&repo_root, &format!("{base_ref}..HEAD"))?;
    if ahead > 1 && auto_squash {
        squash_output(workspace)?;
    }
    // Anything still outstanding blocks PR creation with the shared hint.
    let notes = pr_sync_notes(&repo_root, &base_ref)?;
    if !notes.is_empty() {
        return Err(anyhow!(
            "This pull request needs attention:\n- {}",
            notes.join("\n- ")
        ));
    }
    try_forge_create_pr(&repo_root, &current, &base_ref, forge)
}

pub(crate) fn git_commit_count(repo_root: &Path, range: &str) -> Result<usize> {
    Ok(String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-list", "--count", range])
            .output()
            .context("failed to run git rev-list")?
            .stdout,
    )
    .trim()
    .parse()
    .unwrap_or(0))
}

pub fn try_forge_create_pr(
    repo_root: &Path,
    branch: &str,
    base_ref: &str,
    forge: Forge,
) -> Result<String> {
    let full_message = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["log", "-1", "--format=%B"])
            .output()
            .context("failed to run git log")?
            .stdout,
    )
    .trim()
    .to_string();
    let (title, body) = match full_message.split_once('\n') {
        Some((subject, rest)) => (subject.trim().to_string(), rest.trim().to_string()),
        None => (full_message.clone(), String::new()),
    };
    if title.is_empty() {
        return Err(anyhow!(
            "commit message is empty; cannot derive a pull request title"
        ));
    }
    let push = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["push", "--set-upstream", "origin", branch])
        .output()
        .context("failed to run git push")?;
    if !push.status.success() {
        let stderr = String::from_utf8_lossy(&push.stderr).trim().to_string();
        return Err(anyhow!(
            "git push failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let base = base_ref.trim_start_matches("origin/");
    let cli = forge.cli();
    // GitHub: `gh pr create --title T --body B --base BASE`.
    // GitLab: `glab mr create --title T --description B --source-branch BRANCH
    //          --target-branch BASE --yes` (`--yes` skips the interactive prompt).
    let args: Vec<&str> = match forge {
        Forge::GitHub => vec![
            "pr", "create", "--title", &title, "--body", &body, "--base", base,
        ],
        Forge::GitLab => vec![
            "mr",
            "create",
            "--title",
            &title,
            "--description",
            &body,
            "--source-branch",
            branch,
            "--target-branch",
            base,
            "--yes",
        ],
    };
    let output = match std::process::Command::new(cli)
        .args(&args)
        .current_dir(repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!(
                "pull_request requires the {cli} CLI to be installed"
            ));
        }
        Err(err) => return Err(err).context(format!("failed to run {cli}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let request = if forge == Forge::GitHub { "pr" } else { "mr" };
        return Err(anyhow!(
            "{cli} {request} create failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Created pull request from '{branch}'")
    } else {
        stdout
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_env_lock;
    use tempfile::tempdir;

    #[test]
    fn parses_github_pull_request_list() {
        let json =
            br#"[{"number":90,"title":"Add pull completion"},{"number":58,"title":"Fix rebase"}]"#;
        let requests = parse_pull_request_list(json, Forge::GitHub).expect("parse");
        assert_eq!(
            requests,
            vec![
                PullRequest {
                    number: 90,
                    title: "Add pull completion".to_string(),
                },
                PullRequest {
                    number: 58,
                    title: "Fix rebase".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parses_gitlab_merge_request_list_by_iid() {
        // GitLab keys the user-facing number as `iid`, not `number`.
        let json = br#"[{"iid":7,"title":"Tidy docs"}]"#;
        let requests = parse_pull_request_list(json, Forge::GitLab).expect("parse");
        assert_eq!(
            requests,
            vec![PullRequest {
                number: 7,
                title: "Tidy docs".to_string(),
            }]
        );
    }

    #[test]
    fn empty_pull_request_list_is_empty() {
        assert!(
            parse_pull_request_list(b"", Forge::GitHub)
                .expect("parse")
                .is_empty()
        );
        assert!(
            parse_pull_request_list(b"[]\n", Forge::GitHub)
                .expect("parse")
                .is_empty()
        );
    }

    #[test]
    fn parses_github_comment_list() {
        // The REST shape served by `gh api .../comments`; conversation comments
        // and pull request review comments both look like this.
        let json = br#"[{"user":{"login":"alice"},"created_at":"2026-06-01T12:30:45Z","body":"Looks good!\n"},{"user":{"login":"bob"},"created_at":"2026-06-02T08:00:00Z","body":"Merged."}]"#;
        let comments = parse_comment_list(json, Forge::GitHub).expect("parse");
        assert_eq!(
            comments,
            vec![
                IssueComment {
                    author: "alice".to_string(),
                    date: "2026-06-01 12:30:45".to_string(),
                    body: "Looks good!".to_string(),
                },
                IssueComment {
                    author: "bob".to_string(),
                    date: "2026-06-02 08:00:00".to_string(),
                    body: "Merged.".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parses_gitlab_note_list_and_skips_system_notes() {
        // GitLab notes mix human comments with system notes (label changes,
        // assignments, ...); only the human ones must survive.
        let json = br#"[{"author":{"username":"alice"},"created_at":"2026-06-01T12:30:45.123Z","body":"Looks good!","system":false},{"author":{"username":"bot"},"created_at":"2026-06-01T13:00:00.000Z","body":"changed the label","system":true}]"#;
        let comments = parse_comment_list(json, Forge::GitLab).expect("parse");
        assert_eq!(
            comments,
            vec![IssueComment {
                author: "alice".to_string(),
                date: "2026-06-01 12:30:45".to_string(),
                body: "Looks good!".to_string(),
            }]
        );
    }

    #[test]
    fn empty_comment_list_is_empty() {
        assert!(
            parse_comment_list(b"", Forge::GitHub)
                .expect("parse")
                .is_empty()
        );
        assert!(
            parse_comment_list(b"[]\n", Forge::GitHub)
                .expect("parse")
                .is_empty()
        );
        assert!(
            parse_comment_list(b"[]\n", Forge::GitLab)
                .expect("parse")
                .is_empty()
        );
    }

    #[test]
    fn pull_request_entry_without_number_is_skipped() {
        let json = br#"[{"title":"no number"},{"number":3,"title":"ok"}]"#;
        let requests = parse_pull_request_list(json, Forge::GitHub).expect("parse");
        assert_eq!(
            requests,
            vec![PullRequest {
                number: 3,
                title: "ok".to_string(),
            }]
        );
    }

    #[test]
    fn pr_sync_advice_flags_rebase_and_squash() {
        let _env_lock = process_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        init_git_for_test(workspace.path());

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .status()
                .expect("git command");
        };
        let commit = |name: &str, content: &str, msg: &str| {
            std::fs::write(workspace.path().join(name), content).expect("write");
            git(&["add", "."]);
            git(&["commit", "-m", msg]);
        };

        git(&["checkout", "-B", "main"]);
        commit("base.txt", "base\n", "Base commit");

        // Feature branch with two commits (squash needed).
        git(&["checkout", "-b", "pr-1"]);
        commit("feature.txt", "feat1\n", "First feature commit");
        commit("feature.txt", "feat2\n", "Second feature commit");

        // Advance main so the branch is also behind (rebase needed).
        git(&["checkout", "main"]);
        commit("base.txt", "base2\n", "Base advances");
        git(&["checkout", "pr-1"]);

        let advice = pr_sync_advice(workspace.path()).expect("advice expected");
        assert!(advice.contains("run /rebase"), "missing rebase: {advice}");
        assert!(advice.contains("run /squash"), "missing squash: {advice}");
        assert!(
            advice.contains("1 commit behind main"),
            "behind text: {advice}"
        );
        assert!(
            advice.contains("2 commits ahead of main"),
            "ahead text: {advice}"
        );
    }

    #[test]
    fn pr_sync_advice_silent_when_up_to_date() {
        let _env_lock = process_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        init_git_for_test(workspace.path());

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .status()
                .expect("git command");
        };

        git(&["checkout", "-B", "main"]);
        std::fs::write(workspace.path().join("base.txt"), "base\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-m", "Base commit"]);

        // Single commit, fully up to date with main: no advice.
        git(&["checkout", "-b", "pr-2"]);
        std::fs::write(workspace.path().join("feature.txt"), "feat\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-m", "Lone feature commit"]);

        assert!(pr_sync_advice(workspace.path()).is_none());
    }

    #[test]
    fn create_pull_request_blocks_with_combined_hint() {
        let _env_lock = process_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        init_git_for_test(workspace.path());

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .status()
                .expect("git command");
        };
        let commit = |name: &str, content: &str, msg: &str| {
            std::fs::write(workspace.path().join(name), content).expect("write");
            git(&["add", "."]);
            git(&["commit", "-m", msg]);
        };

        git(&["checkout", "-B", "main"]);
        commit("base.txt", "base\n", "Base commit");

        git(&["checkout", "-b", "feature/pr"]);
        commit("feature.txt", "feat1\n", "First feature commit");
        commit("feature.txt", "feat2\n", "Second feature commit");

        git(&["checkout", "main"]);
        commit("base.txt", "base2\n", "Base advances");
        git(&["checkout", "feature/pr"]);

        // Auto-fixes disabled: creation is blocked with the shared hint.
        let result = create_pull_request_output(workspace.path(), false, false, Forge::GitHub);
        let msg = result
            .expect_err("PR creation should be blocked")
            .to_string();
        assert!(msg.contains("This pull request needs attention"), "{msg}");
        assert!(msg.contains("run /rebase"), "{msg}");
        assert!(msg.contains("run /squash"), "{msg}");
    }

    #[test]
    fn behind_default_branch_counts_unincorporated_base_commits() {
        let _env_lock = process_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        init_git_for_test(workspace.path());

        git_run(workspace.path(), &["checkout", "-B", "main"]);
        std::fs::write(workspace.path().join("base.txt"), "base\n").expect("write base");
        git_run(workspace.path(), &["add", "."]);
        git_run(workspace.path(), &["commit", "-m", "Base commit"]);

        // A fresh feature branch is up to date with main.
        git_run(workspace.path(), &["checkout", "-b", "feature/behind-test"]);
        let (behind, base_ref) = behind_default_branch(workspace.path()).expect("behind");
        assert_eq!((behind, base_ref.as_str()), (0, "main"));

        // A commit landing on main afterwards leaves the branch behind.
        git_run(workspace.path(), &["checkout", "main"]);
        std::fs::write(workspace.path().join("newer.txt"), "newer\n").expect("write newer");
        git_run(workspace.path(), &["add", "."]);
        git_run(workspace.path(), &["commit", "-m", "Newer base commit"]);
        git_run(workspace.path(), &["checkout", "feature/behind-test"]);

        let (behind, base_ref) = behind_default_branch(workspace.path()).expect("behind");
        assert_eq!((behind, base_ref.as_str()), (1, "main"));
    }
    #[test]
    fn comment_report_keywords_error_without_a_stored_report() {
        use crate::commands::CommentBody;
        use crate::git::{ReviewReports, comment_output, git_init};

        let workspace = tempdir().expect("workspace");
        git_init(workspace.path()).expect("git init");

        // `with review` / `with auto review` need a report from this session.
        let err = comment_output(
            workspace.path(),
            48,
            &CommentBody::Review,
            ReviewReports::default(),
            crate::git::Forge::GitHub,
        )
        .expect_err("no review report");
        assert!(err.to_string().contains("run /review first"), "{err:#}");

        let err = comment_output(
            workspace.path(),
            48,
            &CommentBody::AutoReview,
            ReviewReports {
                review: Some("**Patch approved**"),
                auto_review: None,
            },
            crate::git::Forge::GitHub,
        )
        .expect_err("no auto review report");
        assert!(
            err.to_string().contains("run /auto_review first"),
            "{err:#}"
        );
    }
}
