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

//! A single open workspace tab and its live runtime state.
//!
//! Each tab is its own session: its own conversation, scrollback, pending
//! queue, command history and usage. The active tab's fields are used by the
//! run loop exactly like the single-workspace locals were before tabs existed;
//! switching tabs swaps which [`WorkspaceTab`] is active. The model and server
//! are shared across tabs (per-workspace `/server`/`/model` come later).

use crate::*;
use orangu::workspaces::WorkspacePath;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Everything that belongs to one workspace tab. The run loop reads and mutates
/// the active tab's fields; the [`WorkspaceManager`](orangu::workspaces::WorkspaceManager)
/// owns the ordered list and tracks which tab is active.
pub(crate) struct WorkspaceTab {
    pub(crate) workspace: PathBuf,
    pub(crate) tools: ToolExecutor,
    pub(crate) skills: orangu::skills::SkillRegistry,
    pub(crate) session: ChatSession,
    pub(crate) output_state: OutputState,
    pub(crate) pending_commands: VecDeque<String>,
    pub(crate) usage_stats: UsageStats,
    pub(crate) history: Vec<String>,
    pub(crate) session_id: String,
    pub(crate) session_dir: PathBuf,
    pub(crate) session_hist_path: PathBuf,
    pub(crate) session_messages_path: PathBuf,
    pub(crate) session_metadata_path: PathBuf,
    pub(crate) current_branch: Option<String>,
    /// The last `/review` summary and `/auto_review` report (Markdown), kept so
    /// `/comment <number> with [auto] review` can post them. Per tab, since a
    /// review is of that tab's branch.
    pub(crate) last_review_report: Option<String>,
    pub(crate) last_auto_review_report: Option<String>,
    /// The last `/review` and `/auto_review` source appendices (per-finding code
    /// windows), kept alongside their reports so `/export [auto] review` can add
    /// the appendix.
    pub(crate) last_review_appendix: Option<Vec<crate::export::AutoReviewAppendixEntry>>,
    pub(crate) last_auto_review_appendix: Option<Vec<crate::export::AutoReviewAppendixEntry>>,
    pub(crate) last_review_was_auto: bool,
    /// While set and in the future, the status bar shows "Resuming session …"
    /// for a freshly auto-resumed tab.
    pub(crate) startup_notice_until: Option<Instant>,
    /// An LLM response running in a background tokio task because the user
    /// switched tabs mid-stream. `session` is a placeholder while this is `Some`.
    pub(crate) pending_response: Option<PendingResponse>,
}

impl WorkspacePath for WorkspaceTab {
    fn workspace_path(&self) -> &Path {
        &self.workspace
    }
}

impl WorkspaceTab {
    /// Open a workspace tab on `workspace`, resolving its session the same way a
    /// standalone client start does: an explicit `resume` UUID is honoured,
    /// otherwise the matching workspace+branch session is auto-resumed, else a
    /// fresh session is created. `system_prompt` seeds the conversation (the
    /// model is shared across tabs, so every tab starts from the same prompt).
    ///
    /// When `auto_resume` is false and no explicit `resume` is given, a brand
    /// new session is always started rather than auto-resuming the matching one
    /// — used by `Alt+Insert`, which opens a fresh tab the user then points
    /// somewhere with `/workspace`.
    pub(crate) fn open(
        workspace: PathBuf,
        resume: Option<&str>,
        auto_resume: bool,
        system_prompt: &str,
        compression_enabled: bool,
        auto_downsample_lines: usize,
        diff_file_cap: usize,
    ) -> Result<Self> {
        let workspace_created = if !workspace.exists() {
            std::fs::create_dir_all(&workspace)
                .with_context(|| format!("Failed to create workspace {}", workspace.display()))?;
            true
        } else {
            false
        };
        let tools = ToolExecutor::with_config(
            &workspace,
            compression_enabled,
            auto_downsample_lines,
            diff_file_cap,
        );
        let skills = orangu::skills::SkillRegistry::discover(&workspace);
        let current_branch = workspace_branch_name(&workspace);

        let (session_id, is_resumed) = match resume {
            Some(id) => (id.to_string(), true),
            None if auto_resume => {
                let workspace_str = workspace.display().to_string();
                match find_session_for_workspace_branch(
                    &workspace_str,
                    current_branch.as_deref().unwrap_or(""),
                ) {
                    Some(existing_id) => (existing_id, true),
                    None => (Uuid::new_v4().to_string(), false),
                }
            }
            None => (Uuid::new_v4().to_string(), false),
        };
        let session_dir = session_dir_path(&session_id)?;
        std::fs::create_dir_all(&session_dir).with_context(|| {
            format!(
                "failed to create session directory {}",
                session_dir.display()
            )
        })?;
        let session_hist_path = session_dir.join("history");
        let session_messages_path = session_dir.join("messages");
        let session_metadata_path = session_dir.join("metadata");

        if !is_resumed {
            save_session_metadata(
                &session_metadata_path,
                &SessionMetadata {
                    started_at: current_unix_timestamp(),
                    last_updated_at: current_unix_timestamp(),
                    workspace: workspace.display().to_string(),
                    branch: current_branch.clone().unwrap_or_default(),
                },
            )?;
        }

        let enhanced_prompt = {
            let index = skills.system_prompt_index();
            let mut ep = if index.is_empty() {
                system_prompt.to_string()
            } else {
                format!("{system_prompt}\n\n{index}")
            };

            ep.push_str(&orangu::config::load_agents_instructions(&workspace));
            ep
        };
        let mut session = ChatSession::new(&enhanced_prompt);
        if is_resumed {
            session.restore(load_session_messages(&session_messages_path)?);
        }

        let usage_stats = UsageStats::new().with_session(&session_id);
        let history = load_history(&session_hist_path)?;
        let mut output_state = OutputState::default();
        // The auto-resume notice only shows for a session restored implicitly on
        // open, not for an explicit `--resume`/`/session <uuid>` target.
        let startup_notice_until = if is_resumed && resume.is_none() {
            Some(Instant::now() + Duration::from_secs(5))
        } else {
            None
        };
        if workspace_created {
            output_state.push_text(&format!("Created workspace {}", workspace.display()));
        }

        Ok(Self {
            workspace,
            tools,
            skills,
            session,
            output_state,
            pending_commands: VecDeque::new(),
            usage_stats,
            history,
            session_id,
            session_dir,
            session_hist_path,
            session_messages_path,
            session_metadata_path,
            current_branch,
            last_review_report: None,
            last_auto_review_report: None,
            last_review_appendix: None,
            last_auto_review_appendix: None,
            last_review_was_auto: false,
            startup_notice_until,
            pending_response: None,
        })
    }

    /// Persist this tab's conversation and bump its session's last-updated
    /// timestamp. Called when leaving a tab (switch/close) and on quit, so every
    /// tab a run touched is resumable afterwards.
    pub(crate) fn save(&self) -> Result<()> {
        // While a background streaming task owns the real session, `self.session`
        // is a placeholder. Skip the message write so we don't overwrite the last
        // good save with an empty placeholder conversation.
        if self.pending_response.is_none() {
            save_session_messages(&self.session_messages_path, self.session.messages())?;
        }
        update_session_metadata_branch(
            &self.session_metadata_path,
            self.current_branch.as_deref(),
        )?;
        Ok(())
    }

    /// Finish this tab on exit: print its resume command, or silently delete its
    /// session directory when the session is ephemeral (no LLM interaction on
    /// `main`/`master` or outside a Git repository). Run for every open tab so
    /// each one a run touched can be resumed afterwards.
    pub(crate) fn finish(&self) {
        let branch = self.current_branch.as_deref().unwrap_or("");
        if self.usage_stats.total_tokens == 0 && is_ephemeral_branch(branch) {
            delete_session_dir(&self.session_dir);
        } else {
            eprintln!("orangu --resume {}", self.session_id);
        }
    }

    pub(crate) fn dot_status(&self) -> TabStatus {
        if let Some(pr) = &self.pending_response {
            return if pr.handle.is_finished() {
                TabStatus::Valid
            } else {
                TabStatus::Working
            };
        }
        if !self.workspace.is_dir() {
            return TabStatus::BranchGone;
        }
        TabStatus::Valid
    }
}

/// The open workspace tabs other than the active one, plus where the active tab
/// sits among them.
///
/// The active tab's live state lives in the run loop's locals (so the loop body
/// is unchanged from the single-workspace days); this ring holds the rest. A
/// switch *parks* the current active here and hands back the tab to make active,
/// which the caller unpacks back into its locals. The same invariants as
/// [`WorkspaceManager`](orangu::workspaces::WorkspaceManager) hold: there is
/// always at least one tab, and closing renumbers the rest.
pub(crate) struct WorkspaceRing {
    others: Vec<WorkspaceTab>,
    /// Index the active tab occupies in the full left-to-right order.
    active_pos: usize,
}

impl WorkspaceRing {
    /// A ring for a single active tab and no others.
    pub(crate) fn new() -> Self {
        Self {
            others: Vec::new(),
            active_pos: 0,
        }
    }

    /// Total number of open tabs, including the active one.
    pub(crate) fn total(&self) -> usize {
        self.others.len() + 1
    }

    /// The active tab's 0-based position in the full left-to-right order.
    pub(crate) fn active_pos(&self) -> usize {
        self.active_pos
    }

    /// Park `active` and move focus by `delta` tabs (wrapping), returning the tab
    /// that should become active. `delta` of `1` is the next tab on the right,
    /// `-1` the previous on the left.
    pub(crate) fn rotate(&mut self, active: WorkspaceTab, delta: isize) -> WorkspaceTab {
        self.others.insert(self.active_pos, active);
        let total = self.others.len() as isize;
        self.active_pos = (self.active_pos as isize + delta).rem_euclid(total) as usize;
        self.others.remove(self.active_pos)
    }

    /// Park `active` and open `new_tab` as the rightmost tab, making it active.
    pub(crate) fn open(&mut self, active: WorkspaceTab, new_tab: WorkspaceTab) -> WorkspaceTab {
        self.others.insert(self.active_pos, active);
        self.active_pos = self.others.len();
        new_tab
    }

    /// Switch to the tab at full-order `index`, parking `active`. Returns the tab
    /// to make active, or gives `active` back unchanged when `index` is out of
    /// range or already the active position.
    pub(crate) fn switch_to(&mut self, active: WorkspaceTab, index: usize) -> WorkspaceTab {
        if index == self.active_pos || index >= self.total() {
            return active;
        }
        self.others.insert(self.active_pos, active);
        self.active_pos = index;
        self.others.remove(index)
    }

    /// Close the active tab (which the caller has already saved) and focus a
    /// neighbour — its right neighbour, or the new last tab when it was the
    /// rightmost. Returns `None` (dropping nothing) when it is the only tab open;
    /// only `/quit` ends orangu.
    pub(crate) fn close(&mut self, _active: WorkspaceTab) -> Option<WorkspaceTab> {
        if self.others.is_empty() {
            return None;
        }
        self.active_pos = self.active_pos.min(self.others.len() - 1);
        Some(self.others.remove(self.active_pos))
    }

    /// The parked (non-active) tabs, for saving every tab a run touched on quit.
    pub(crate) fn parked(&self) -> &[WorkspaceTab] {
        &self.others
    }

    /// Append an already-open tab to the right without changing focus. Used at
    /// startup by `orangu -a` to restore the previously open tabs behind the
    /// active one.
    pub(crate) fn park(&mut self, tab: WorkspaceTab) {
        self.others.push(tab);
    }

    /// The full-order index of the tab open on `path`, given the active tab's
    /// path. Used so `/workspace <path>` switches to an already-open directory
    /// rather than opening a second tab for it.
    pub(crate) fn position_of(&self, active: &Path, path: &Path) -> Option<usize> {
        if active == path {
            return Some(self.active_pos);
        }
        self.others
            .iter()
            .position(|t| t.workspace == path)
            .map(|i| if i >= self.active_pos { i + 1 } else { i })
    }
}

/// A change to which workspace tab is active, applied to the run loop's tab
/// ring. Produced by the workspace key bindings and by the `/workspace` command.
pub(crate) enum TabAction {
    /// Focus the next tab on the right (`Alt+.`), wrapping.
    Next,
    /// Focus the previous tab on the left (`Alt+,`), wrapping.
    Previous,
    /// Focus the tab at the given full-order index (`/workspace <number>`).
    SwitchTo(usize),
    /// Start a fresh tab on the active workspace's directory (`Alt+Insert`); the
    /// user re-points it with `/workspace` afterwards.
    New,
    /// Close the active tab (`Alt+Delete`); the last tab is never closed.
    Close,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_tab(workspace: &str) -> WorkspaceTab {
        WorkspaceTab {
            workspace: PathBuf::from(workspace),
            tools: ToolExecutor::new(std::path::Path::new(workspace)),
            skills: orangu::skills::SkillRegistry::discover(std::path::Path::new(workspace)),
            session: orangu::session::ChatSession::new(""),
            output_state: OutputState::default(),
            pending_commands: VecDeque::new(),
            usage_stats: UsageStats::new(),
            history: vec![],
            session_id: workspace.to_string(),
            session_dir: PathBuf::from(workspace),
            session_hist_path: PathBuf::from(workspace),
            session_messages_path: PathBuf::from(workspace),
            session_metadata_path: PathBuf::from(workspace),
            current_branch: None,
            last_review_report: None,
            last_auto_review_report: None,
            last_review_appendix: None,
            last_auto_review_appendix: None,
            last_review_was_auto: false,
            startup_notice_until: None,
            pending_response: None,
        }
    }

    #[test]
    fn rotate_next_wraps_at_end() {
        let mut ring = WorkspaceRing::new();
        let a = stub_tab("/a");
        let b = stub_tab("/b");
        let c = stub_tab("/c");
        // Build ring: active=/a, parked=[/b, /c] at positions 1,2
        let active = ring.open(a, b); // active=/b, parked=[/a]
        let active = ring.open(active, c); // active=/c, parked=[/a,/b]
        assert_eq!(ring.active_pos(), 2);

        let active = ring.rotate(active, 1); // wraps to 0 → /a
        assert_eq!(active.session_id, "/a");
        assert_eq!(ring.active_pos(), 0);
    }

    #[test]
    fn rotate_previous_wraps_at_start() {
        let mut ring = WorkspaceRing::new();
        let a = stub_tab("/a");
        let b = stub_tab("/b");
        let c = stub_tab("/c");
        let active = ring.open(a, b);
        let active = ring.open(active, c);
        // active=/c at pos 2; rotate to pos 0 first
        let active = ring.rotate(active, 1); // /a at pos 0
        assert_eq!(active.session_id, "/a");

        let active = ring.rotate(active, -1); // wraps 0 → 2 → /c
        assert_eq!(active.session_id, "/c");
        assert_eq!(ring.active_pos(), 2);
    }

    #[test]
    fn switch_to_out_of_range_returns_active_unchanged() {
        let mut ring = WorkspaceRing::new();
        let a = stub_tab("/a");
        let b = stub_tab("/b");
        let active = ring.open(a, b);
        assert_eq!(ring.active_pos(), 1);

        let active = ring.switch_to(active, 5); // out of range
        assert_eq!(active.session_id, "/b");
        assert_eq!(ring.active_pos(), 1);
    }

    #[test]
    fn close_last_tab_returns_none() {
        let mut ring = WorkspaceRing::new();
        let a = stub_tab("/a");
        assert!(ring.close(a).is_none());
    }

    #[test]
    fn close_renumbers_and_focuses_right_neighbour() {
        let mut ring = WorkspaceRing::new();
        let a = stub_tab("/a");
        let b = stub_tab("/b");
        let c = stub_tab("/c");
        let active = ring.open(a, b);
        let active = ring.open(active, c);
        // layout: [/a, /b, /c], active=/c at pos 2
        let active = ring.switch_to(active, 1); // switch to /b
        assert_eq!(active.session_id, "/b");

        let new_active = ring.close(active).expect("two tabs remain");
        // /b was at pos 1; right neighbour is /c (now at pos 1)
        assert_eq!(new_active.session_id, "/c");
        assert_eq!(ring.active_pos(), 1);
        assert_eq!(ring.total(), 2);
    }

    #[test]
    fn position_of_finds_active_and_parked() {
        let mut ring = WorkspaceRing::new();
        let a = stub_tab("/a");
        let b = stub_tab("/b");
        let active = ring.open(a, b); // active=/b at pos 1, parked=[/a]

        assert_eq!(
            ring.position_of(&active.workspace, std::path::Path::new("/b")),
            Some(1)
        );
        assert_eq!(
            ring.position_of(&active.workspace, std::path::Path::new("/a")),
            Some(0)
        );
        assert_eq!(
            ring.position_of(&active.workspace, std::path::Path::new("/z")),
            None
        );
    }
}
