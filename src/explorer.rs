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

use crate::config::{default_client_config_path, load_client_configuration};
use crate::session::ChatSession;
use crate::tools::ToolExecutor;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

const EXPLORER_SYSTEM_PROMPT: &str = r#"
You are a codebase exploration specialist focused exclusively on searching and analyzing existing code.
Your main goal is to explore the codebase based on a query.

Your strengths:
- Rapidly finding files using glob patterns (via run_shell_command with find or fd)
- Searching code and text with powerful regex patterns (via run_shell_command with rg or grep)
- Reading and analyzing file contents (via read_file)

Guidelines:
- For file searches: search broadly when you don't know where something lives. Use read_file when you know the specific file path.
- For analysis: Start broad and narrow down. Use multiple search strategies if the first doesn't yield results.
- Be thorough: Check multiple locations, consider different naming conventions, look for related files.

NOTE: You are meant to be a fast agent that returns output as quickly as possible. In order to achieve this you must:
- Make efficient use of the tools that you have at your disposal: be smart about how you search for files and implementations

## Required Output

End your response with an optional brief explanation of your findings (no more than 50 words), followed by a `<final_answer>` tag containing the relevant file paths and line ranges.

<example>
The core routing logic lives in two files.

<final_answer>
/absolute/path/to/file_1.py:10-15 (Optional Brief Reason: e.g., "Core logic to modify")
/absolute/path/to/file_2.js:102-123
</final_answer>
</example>
"#;

pub fn run_explorer_subagent<'a>(
    workspace: &'a Path,
    arguments: &'a serde_json::Map<String, Value>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
    Box::pin(async move {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .context("missing or invalid 'query' argument")?;

        let config_path = default_client_config_path().context("no config path")?;
        let client_app_config = load_client_configuration(&config_path)
            .context("failed to load LLM configuration for subagent")?;

        let target_server = client_app_config.find_server_for_role("explorer");

        let config = client_app_config
            .llms
            .get(&target_server)
            .context("missing target server in config")?;

        let mut dir_listing = String::new();
        if let Ok(entries) = std::fs::read_dir(workspace) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    dir_listing.push_str(&name);
                    dir_listing.push('\n');
                }
            }
        }

        let system_prompt = format!(
            "{}\n\nThe directory listing of the workspace root is:\n{}",
            EXPLORER_SYSTEM_PROMPT, dir_listing
        );

        let mut session = ChatSession::new(&system_prompt);
        let tools = ToolExecutor::new_read_only(workspace);

        let prompt = format!(
            "Please explore the codebase to answer the following query:\n\n<query>\n{}\n</query>\n\nCRITICAL: You MUST wrap your final conclusion in a `<final_answer>...</final_answer>` block. If you omit these tags, the system will not be able to parse your response.",
            query
        );

        let max_turns = 6;

        // We run the subagent loop directly using ChatSession's submit_with_tools.
        // ChatSession already handles the LLM loop up to config.max_tool_rounds.
        // If the model finishes and returns text, we capture it.

        let mut config_clone = config.clone();
        // Override max tool rounds to 6 specifically for the explorer
        config_clone.max_tool_rounds = max_turns;

        let response = session
            .prompt(&prompt, &config_clone, &tools, |_| {}, |_| {}, |_| {})
            .await?;

        // Try to extract the final answer block
        if let Some(start) = response.find("<final_answer>")
            && let Some(end) = response[start..].find("</final_answer>")
        {
            return Ok(response[start..start + end + 15].to_string());
        } // Fallback: return the whole response
        Ok(format!(
            "[Subagent failed to format output in <final_answer> tags. Raw response follows:]\n\n{}",
            response
        ))
    })
}
