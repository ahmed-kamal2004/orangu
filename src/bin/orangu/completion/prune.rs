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

use super::{
    session_path_completion_candidates, session_uuids_newest_first, session_workspaces_newest_first,
};
use crate::commands::strip_ascii_prefix;

/// The flag names offered for `/prune -…`, mirroring the long flags that
/// [`crate::commands::parse_prune_args`] accepts. The short aliases (`-w`,
/// `-o`) are intentionally left out of completion so the hint stays readable;
/// the long forms are what the usage message advertises.
const PRUNE_FLAGS: [&str; 2] = ["--workspace", "--older-than"];

/// Tab/ghost completion for the `/prune` argument and its natural-language
/// aliases (`prune session `, `prune sessions in `, `prune sessions older than
/// `), as `(token_start, candidates)`.
///
/// The argument is poly-typed, so each form completes differently:
/// - `--workspace`/`-w` and `prune sessions in ` complete a workspace path —
///   the workspaces seen in past sessions first, then filesystem directory
///   completion so a brand-new workspace can be navigated to (mirroring
///   `/workspace <dir>`).
/// - `--older-than`/`-o` and `prune sessions older than ` take a free-form day
///   count with nothing to complete, so an empty list is returned (recognised,
///   but no candidates) and the user types a number and presses enter.
/// - `/prune -…` offers the flag names, so the inline ghost and Tab finish
///   `--workspace` / `--older-than`.
/// - otherwise the argument is a session UUID — completed against the known
///   sessions, newest first — or the literal `all`, which is offered (and
///   ghosted) whenever it still matches the typed prefix.
///
/// Returns `None` when `prefix` is not a `/prune` argument (or a natural-language
/// prune form), leaving the caller to fall back to the command list.
pub fn prune_completion_candidates(prefix: &str) -> Option<(usize, Vec<String>)> {
    // Workspace-path forms.
    for form in ["/prune --workspace ", "/prune -w ", "prune sessions in "] {
        if let Some(path_prefix) = strip_ascii_prefix(prefix, form) {
            let mut candidates: Vec<String> = session_workspaces_newest_first()
                .into_iter()
                .filter(|w| w.starts_with(path_prefix))
                .collect();
            if candidates.is_empty() {
                candidates = session_path_completion_candidates(path_prefix);
            }
            return Some((prefix.len() - path_prefix.len(), candidates));
        }
    }

    // Free-form day-count forms: recognised, but nothing to complete.
    for form in [
        "/prune --older-than ",
        "/prune -o ",
        "prune sessions older than ",
    ] {
        if strip_ascii_prefix(prefix, form).is_some() {
            return Some((prefix.len(), Vec::new()));
        }
    }

    // Session-UUID forms (the natural alias and the bare slash argument).
    if let Some(uuid_prefix) = strip_ascii_prefix(prefix, "prune session ") {
        return Some((
            prefix.len() - uuid_prefix.len(),
            uuid_candidates(uuid_prefix),
        ));
    }

    if let Some(arg_prefix) = prefix.strip_prefix("/prune ") {
        // `/prune -…`: offer the flag names.
        if arg_prefix.starts_with('-') {
            let candidates = PRUNE_FLAGS
                .into_iter()
                .filter(|flag| flag.starts_with(arg_prefix))
                .map(str::to_string)
                .collect();
            return Some(("/prune ".len(), candidates));
        }
        // Otherwise the argument is a session UUID, or the literal `all` (prune
        // every session). `all` is surfaced whenever it still matches what is
        // typed so it gets a ghost and Tab completion too: once a non-empty
        // prefix is typed (`/prune a`) it leads, so the ghost finishes `all`;
        // for the bare `/prune ` it trails the UUIDs, keeping the common
        // prune-this-session flow first while still listing `all` in the menu.
        let mut candidates = uuid_candidates(arg_prefix);
        if "all".starts_with(arg_prefix) {
            if arg_prefix.is_empty() {
                candidates.push("all".to_string());
            } else {
                candidates.insert(0, "all".to_string());
            }
        }
        return Some(("/prune ".len(), candidates));
    }

    None
}

fn uuid_candidates(prefix: &str) -> Vec<String> {
    session_uuids_newest_first()
        .into_iter()
        .filter(|u| u.starts_with(prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_dash_offers_flag_names() {
        // A lone dash offers both long flags, so the inline ghost can finish one.
        let (start, candidates) = prune_completion_candidates("/prune -").expect("flag completion");
        assert_eq!(start, "/prune ".len());
        assert_eq!(
            candidates,
            vec!["--workspace".to_string(), "--older-than".to_string()]
        );

        // Typing narrows to the matching flag.
        assert_eq!(
            prune_completion_candidates("/prune --w").expect("flag").1,
            vec!["--workspace".to_string()]
        );
        assert_eq!(
            prune_completion_candidates("/prune --o").expect("flag").1,
            vec!["--older-than".to_string()]
        );
    }

    #[test]
    fn older_than_argument_is_recognised_but_empty() {
        // The day count is free-form: recognised (Some) so it does not fall back
        // to the command list, but with no candidates so the user just types a
        // number. The token anchors at the cursor (end of input).
        for form in [
            "/prune --older-than ",
            "/prune --older-than 1",
            "/prune -o ",
            "prune sessions older than ",
            "prune sessions older than 30",
        ] {
            let (start, candidates) =
                prune_completion_candidates(form).expect("older-than recognised");
            assert_eq!(start, form.len(), "{form:?}");
            assert!(candidates.is_empty(), "{form:?}");
        }
    }

    #[test]
    fn workspace_offset_points_past_the_flag() {
        // The replacement anchors at the start of the path token for every
        // workspace form, so an accepted candidate replaces only the path.
        for form in ["/prune --workspace ", "/prune -w ", "prune sessions in "] {
            let (start, _) = prune_completion_candidates(form).expect("workspace form");
            assert_eq!(start, form.len(), "{form:?}");
        }
    }

    #[test]
    fn all_is_offered_and_ghostable() {
        // A non-empty prefix of `all` leads the list, so the inline ghost
        // finishes it (`/prune a` -> `ll`).
        let (start, candidates) = prune_completion_candidates("/prune a").expect("all completion");
        assert_eq!(start, "/prune ".len());
        assert_eq!(candidates.first().map(String::as_str), Some("all"));

        // The bare argument still lists `all` (after any session UUIDs).
        let (_, candidates) = prune_completion_candidates("/prune ").expect("bare argument");
        assert!(candidates.iter().any(|c| c == "all"), "{candidates:?}");

        // A prefix that `all` cannot extend does not spuriously offer it.
        let (_, candidates) = prune_completion_candidates("/prune b").expect("uuid argument");
        assert!(!candidates.iter().any(|c| c == "all"), "{candidates:?}");
    }

    #[test]
    fn non_prune_input_is_not_recognised() {
        assert!(prune_completion_candidates("/prunes x").is_none());
        assert!(prune_completion_candidates("/branch main").is_none());
        // The natural `prune all` is a complete binding with no argument to
        // complete; its ghost comes from the natural-language binding list.
        assert!(prune_completion_candidates("prune all").is_none());
    }
}
