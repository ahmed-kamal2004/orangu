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

use std::path::Path;

use super::git_commit_hashes;
use crate::git::discover_git_root;

/// The `/bisect` subcommand verbs, in the order they are offered (and ghosted)
/// while the verb is still being typed. Mirrors [`parse_bisect_subcommand`].
const BISECT_VERBS: [&str; 7] = ["start", "good", "bad", "skip", "reset", "log", "status"];

/// Tab completion for `/bisect <subcommand> <commit>`:
/// - while the verb is still being typed (no space yet) offer the subcommand
///   verbs, so `/bisect ` lists them and `/bisect st` completes to `start`;
/// - after `/bisect start`, `/bisect good`, `/bisect bad`, or `/bisect skip`
///   with a trailing space, offer commit hashes from the repository;
/// - the no-argument verbs (`reset`, `log`, `status`) are recognised but offer
///   nothing once a space follows.
pub fn bisect_completion_candidates(
    prefix: &str,
    workspace: &Path,
) -> Option<(usize, Vec<String>)> {
    let rest = prefix.strip_prefix("/bisect ")?;

    // Still typing the verb (no whitespace yet): offer the subcommand names.
    if !rest.contains(char::is_whitespace) {
        let candidates = BISECT_VERBS
            .into_iter()
            .filter(|verb| verb.starts_with(rest))
            .map(str::to_string)
            .collect();
        return Some(("/bisect ".len(), candidates));
    }

    let commit_subcommands = [
        "/bisect start ",
        "/bisect good ",
        "/bisect bad ",
        "/bisect skip ",
    ];
    for cmd in &commit_subcommands {
        if let Some(rest) = prefix.strip_prefix(cmd) {
            // Match candidates against the typed token, trimming any extra
            // spaces after the subcommand. The replacement still starts at the
            // end of the `<subcommand> ` prefix, so those spaces are collapsed
            // when a candidate is accepted — the same convention as
            // `cherry_pick_completion_candidates`.
            let token = rest.trim_start();
            let candidates = discover_git_root(workspace)
                .map(|root| git_commit_hashes(&root, token))
                .unwrap_or_default();
            return Some((cmd.len(), candidates));
        }
    }

    // A no-argument verb (reset/log/status) followed by text: recognised, but
    // nothing to complete.
    Some((prefix.len(), Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::bisect_completion_candidates;
    use tempfile::tempdir;

    #[test]
    fn completes_the_subcommand_verb() {
        let dir = tempdir().expect("tempdir");
        // A bare `/bisect ` lists every verb, in offer order.
        let (start, all) =
            bisect_completion_candidates("/bisect ", dir.path()).expect("verb candidates");
        assert_eq!(start, "/bisect ".len());
        assert_eq!(
            all,
            ["start", "good", "bad", "skip", "reset", "log", "status"]
        );
        // A partial verb narrows to the matching ones (`s` -> start, skip, status).
        let (_, s) = bisect_completion_candidates("/bisect s", dir.path()).expect("candidates");
        assert_eq!(s, ["start", "skip", "status"]);
        // A no-argument verb followed by text is recognised but offers nothing.
        let (_, none) =
            bisect_completion_candidates("/bisect reset ", dir.path()).expect("recognised");
        assert!(none.is_empty());
    }

    #[test]
    fn returns_none_for_non_bisect_prefixes() {
        let dir = tempdir().expect("tempdir");
        // A different command is not recognised.
        assert!(bisect_completion_candidates("/branch good ", dir.path()).is_none());
    }

    #[test]
    fn offsets_to_the_end_of_the_subcommand_prefix() {
        let dir = tempdir().expect("tempdir");
        for cmd in [
            "/bisect start ",
            "/bisect good ",
            "/bisect bad ",
            "/bisect skip ",
        ] {
            let (start, _candidates) =
                bisect_completion_candidates(cmd, dir.path()).expect("a candidates tuple");
            assert_eq!(start, cmd.len(), "offset should point past '{cmd}'");
        }
        // Extra spaces and a partial token still anchor at the subcommand end.
        let (start, candidates) =
            bisect_completion_candidates("/bisect good   ab", dir.path()).expect("some");
        assert_eq!(start, "/bisect good ".len());
        // Outside a repository there are no commit hashes to offer, but the
        // subcommand is still recognised (Some), just with an empty list.
        assert!(candidates.is_empty(), "no candidates outside a git repo");
    }
}
