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

use rustyline::{
    CompletionType, Config, Context, EditMode, Helper,
    completion::{Completer, FilenameCompleter, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::{ValidationContext, ValidationResult, Validator},
};
use terminal_size::{Height, Width, terminal_size};

pub fn editor_config() -> Config {
    Config::builder()
        .history_ignore_space(true)
        .completion_type(CompletionType::Circular)
        .edit_mode(EditMode::Emacs)
        .build()
}

const CLIENT_LOGO_ART: &[&str] = &[
    " ██████  ██████   █████  ███    ██  ██████  ██    ██ ",
    "██    ██ ██   ██ ██   ██ ████   ██ ██       ██    ██ ",
    "██    ██ ██████  ███████ ██ ██  ██ ██   ███ ██    ██ ",
    "██    ██ ██   ██ ██   ██ ██  ██ ██ ██    ██ ██    ██ ",
    " ██████  ██   ██ ██   ██ ██   ████  ██████   ██████  ",
];

pub fn render_header(
    version: &str,
    current_model: &str,
    endpoint: &str,
    workspace: &std::path::Path,
) -> String {
    let status_lines = [
        format!("Version: {version}"),
        String::new(),
        format!("Workspace: {}", workspace.display()),
        format!("Server: {endpoint}"),
        format!("Model: {current_model}"),
        String::new(),
        "Help: /help".to_string(),
    ];
    let logo_width = CLIENT_LOGO_ART
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let status_width = status_lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let gap_width = 2;
    let width = logo_width + gap_width + status_width;
    let top_border = format!("┏{}┓", "━".repeat(width + 2));
    let bottom_border = format!("┗{}┛", "━".repeat(width + 2));

    let line_count = CLIENT_LOGO_ART.len().max(status_lines.len());
    let mut lines = Vec::with_capacity(line_count + 2);
    lines.push(top_border);

    for index in 0..line_count {
        let logo_line = CLIENT_LOGO_ART.get(index).copied().unwrap_or_default();
        let status_line = status_lines
            .get(index)
            .map(String::as_str)
            .unwrap_or_default();
        let content = format!(
            "{}{}{}",
            logo_line,
            " ".repeat(logo_width.saturating_sub(logo_line.chars().count()) + gap_width),
            status_line
        );
        let padding = width.saturating_sub(content.chars().count());
        lines.push(format!("┃ {content}{} ┃", " ".repeat(padding)));
    }

    lines.push(bottom_border);
    lines.join("\r\n")
}

pub fn help_text() -> &'static str {
    r#"/help           Show available commands
/list-models    List configured models
/tools          List local tools exposed to the model
/model          Show the active model and configured profiles
/model <name>   Switch to a configured model profile
/clear          Clear the current conversation, keeping the system prompt
/quit           Exit the client

The prompt uses standard Unix shell keys, including Ctrl+A, Ctrl+E, Ctrl+K, Ctrl+U, Ctrl+W, and Tab completion."#
}

pub fn render_screen(
    version: &str,
    current_model: &str,
    endpoint: &str,
    workspace: &std::path::Path,
    transcript: &[String],
    pending_line: Option<&str>,
    input: &str,
    cursor: usize,
) -> String {
    let header = render_header(version, current_model, endpoint, workspace);
    let header_line_count = header.lines().count();
    let width = terminal_width().max(1);
    let input_lines = wrapped_input_lines(input, width);
    let prompt_frame_height = input_lines.len() + 3;
    let height = terminal_height().max(header_line_count + prompt_frame_height + 1);
    let output_start_row = header_line_count + 2;
    let available_output_rows = height
        .saturating_sub(prompt_frame_height)
        .saturating_sub(output_start_row)
        .saturating_add(1);
    let mut output_lines = transcript.to_vec();
    if let Some(pending_line) = pending_line {
        output_lines.push(pending_line.to_string());
    }
    let visible_lines = if output_lines.len() > available_output_rows {
        &output_lines[output_lines.len() - available_output_rows..]
    } else {
        &output_lines
    };

    let mut screen = String::new();
    screen.push_str(&header);
    screen.push_str("\r\n\r\n");
    if !visible_lines.is_empty() {
        screen.push_str(&visible_lines.join("\r\n"));
        screen.push_str("\r\n");
    }
    screen.push_str(&render_prompt_frame(
        header_line_count,
        current_model,
        input,
        cursor,
        width,
        height,
    ));
    screen
}

fn render_prompt_frame(
    header_height: usize,
    current_model: &str,
    input: &str,
    cursor: usize,
    width: usize,
    height: usize,
) -> String {
    let input_lines = wrapped_input_lines(input, width);
    let input_height = input_lines.len();
    let height = height.max(header_height + input_height + 3);
    let top_row = (height.saturating_sub(input_height + 2)).max(header_height + 1);
    let input_start_row = top_row + 1;
    let bottom_row = input_start_row + input_height;
    let model_row = bottom_row + 1;
    let line = "━".repeat(width);
    let model_width = current_model.chars().count();
    let padding = width.saturating_sub(model_width);
    let mut frame = format!("\x1b[{top_row};1H{line}");

    for (index, input_line) in input_lines.iter().enumerate() {
        let row = input_start_row + index;
        let content = truncate_to_width(input_line, width.saturating_sub(2));
        let content_width = content.chars().count();
        frame.push_str(&format!("\x1b[{row};1H> {}", content));
        if width > content_width + 2 {
            frame.push_str(&" ".repeat(width - content_width - 2));
        }
    }

    let (cursor_row_offset, cursor_col_offset) = cursor_position(input, cursor, width);
    let cursor_row = input_start_row + cursor_row_offset;
    let cursor_col = 3 + cursor_col_offset;

    frame.push_str(&format!(
        "\x1b[{bottom_row};1H{line}\x1b[{model_row};1H{}{current_model}\x1b[{cursor_row};{cursor_col}H",
        " ".repeat(padding)
    ));
    frame
}

fn wrapped_input_lines(input: &str, width: usize) -> Vec<String> {
    let input_width = width.saturating_sub(2).max(1);
    if input.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();

    for ch in input.chars() {
        current.push(ch);
        if current.chars().count() == input_width {
            lines.push(current);
            current = String::new();
        }
    }

    if current.is_empty() {
        lines.push(String::new());
    } else {
        lines.push(current);
    }

    lines
}

fn cursor_position(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    let input_width = width.saturating_sub(2).max(1);
    let prefix_chars = input[..cursor.min(input.len())].chars().count();
    (prefix_chars / input_width, prefix_chars % input_width)
}

fn truncate_to_width(input: &str, width: usize) -> String {
    input.chars().take(width).collect()
}

fn terminal_width() -> usize {
    terminal_size()
        .map(|(Width(width), _)| usize::from(width))
        .filter(|width| *width > 0)
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|width| *width > 0)
        })
        .unwrap_or(80)
}

fn terminal_height() -> usize {
    terminal_size()
        .map(|(_, Height(height))| usize::from(height))
        .filter(|height| *height > 0)
        .or_else(|| {
            std::env::var("LINES")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|height| *height > 0)
        })
        .unwrap_or(24)
}

pub struct OranguHelper {
    file_completer: FilenameCompleter,
    commands: Vec<String>,
    models: Vec<String>,
}

impl OranguHelper {
    pub fn new(models: Vec<String>) -> Self {
        Self {
            file_completer: FilenameCompleter::new(),
            commands: vec![
                "/help".to_string(),
                "/list-models".to_string(),
                "/tools".to_string(),
                "/model".to_string(),
                "/clear".to_string(),
                "/quit".to_string(),
            ],
            models,
        }
    }
}

impl Helper for OranguHelper {}

impl Validator for OranguHelper {
    fn validate(&self, _: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        Ok(ValidationResult::Valid(None))
    }
}

impl Highlighter for OranguHelper {}

impl Hinter for OranguHelper {
    type Hint = String;
}

impl Completer for OranguHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if let Some(remainder) = line.strip_prefix("/model ") {
            let prefix = &remainder[..pos.saturating_sub(7)];
            let matches = self
                .models
                .iter()
                .filter(|model| model.starts_with(prefix))
                .map(|model| Pair {
                    display: model.clone(),
                    replacement: model.clone(),
                })
                .collect();
            return Ok((7, matches));
        }

        if line.starts_with('/') {
            let prefix = &line[..pos];
            let matches = self
                .commands
                .iter()
                .filter(|command| command.starts_with(prefix))
                .map(|command| Pair {
                    display: command.clone(),
                    replacement: command.clone(),
                })
                .collect();
            return Ok((0, matches));
        }

        self.file_completer.complete(line, pos, ctx)
    }
}
