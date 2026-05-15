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
use std::time::Duration;
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
const ORANGU_BROWN: &str = "\x1b[38;2;139;90;43m";
const STATUS_GREEN: &str = "\x1b[38;2;80;200;120m";
const STATUS_RED: &str = "\x1b[38;2;220;80;80m";
const THINKING_TIMER: &str = "\x1b[2m";
const ANSI_RESET: &str = "\x1b[0m";
const THINKING_TEXT: &str = "Thinking";
const THINKING_SHADE_LEVELS: &[u8] = &[230, 210, 190, 170, 150, 130, 110, 90];

#[derive(Debug, Clone, Copy)]
pub struct HeaderStatus {
    pub workspace_ok: bool,
    pub server_ok: bool,
    pub model_ok: bool,
}

pub fn render_header(
    version: &str,
    current_model: &str,
    endpoint: &str,
    workspace: &std::path::Path,
    status: HeaderStatus,
) -> String {
    let status_lines = [
        status_text_line(&format!("Version: {version}")),
        status_text_line(""),
        status_indicator_line(
            &format!("Workspace: {}", workspace.display()),
            status.workspace_ok,
        ),
        status_indicator_line(&format!("Server: {endpoint}"), status.server_ok),
        status_indicator_line(&format!("Model: {current_model}"), status.model_ok),
        status_text_line(""),
        status_text_line("Help: /help"),
    ];
    let logo_width = CLIENT_LOGO_ART
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let status_width = status_lines
        .iter()
        .map(|line| line.visible_width)
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
        let colored_logo_line = format!("{ORANGU_BROWN}{logo_line}{ANSI_RESET}");
        let status_line = status_lines.get(index).cloned().unwrap_or_default();
        let visible_content_width = logo_line.chars().count()
            + logo_width.saturating_sub(logo_line.chars().count())
            + gap_width
            + status_line.visible_width;
        let content = format!(
            "{}{}{}",
            colored_logo_line,
            " ".repeat(logo_width.saturating_sub(logo_line.chars().count()) + gap_width),
            status_line.rendered
        );
        let padding = width.saturating_sub(visible_content_width);
        lines.push(format!("┃ {content}{} ┃", " ".repeat(padding)));
    }

    lines.push(bottom_border);
    lines.join("\r\n")
}

pub fn help_text() -> &'static str {
    r#"/help           Show available commands
/connect        Connect to the configured server target
/connect <url>  Connect to a specific server target
/disconnect     Disconnect from the current server target
/reload         Restore the startup model and server target
/list-models    List configured models
/tools          List local tools exposed to the model
/model          Show the active model and configured profiles
/model <name>   Switch to a configured model profile
/open_file <path> Open a workspace file in $EDITOR
/clear          Clear the current conversation, keeping the system prompt
/quit           Exit the client
 
Natural-language forms such as `open README.md`, `list models`, and `show help` are also handled locally.

The prompt uses standard Unix shell keys, including Ctrl+A, Ctrl+E, Ctrl+K, Ctrl+U, Ctrl+W, and Tab completion."#
}

pub fn render_screen(
    version: &str,
    current_model: &str,
    endpoint: &str,
    workspace: &std::path::Path,
    status: HeaderStatus,
    transcript: &[String],
    pending_line: Option<&str>,
    input: &str,
    cursor: usize,
) -> String {
    let header = render_header(version, current_model, endpoint, workspace, status);
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

pub fn render_thinking_frame(frame: usize, elapsed: Duration) -> String {
    let mut rendered = String::new();
    let offset = frame % THINKING_SHADE_LEVELS.len();

    for (index, ch) in THINKING_TEXT.chars().enumerate() {
        let shade_index =
            (index + THINKING_SHADE_LEVELS.len() - offset) % THINKING_SHADE_LEVELS.len();
        let shade = THINKING_SHADE_LEVELS[shade_index];
        rendered.push_str(&format!("\x1b[38;2;{shade};{shade};{shade}m{ch}"));
    }
    rendered.push_str(ANSI_RESET);
    rendered.push(' ');
    rendered.push_str(THINKING_TIMER);
    rendered.push_str(&format_elapsed_timer(elapsed));
    rendered.push_str(ANSI_RESET);

    rendered
}

fn format_elapsed_timer(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("({hours}h{minutes}m{seconds}s)")
    } else if minutes > 0 {
        format!("({minutes}m{seconds}s)")
    } else {
        format!("({seconds}s)")
    }
}

fn indicator(ok: bool) -> String {
    if ok {
        format!("{STATUS_GREEN}●{ANSI_RESET}")
    } else {
        format!("{STATUS_RED}●{ANSI_RESET}")
    }
}

#[derive(Clone, Default)]
struct HeaderLine {
    rendered: String,
    visible_width: usize,
}

fn status_text_line(text: &str) -> HeaderLine {
    HeaderLine {
        rendered: text.to_string(),
        visible_width: text.chars().count(),
    }
}

fn status_indicator_line(text: &str, ok: bool) -> HeaderLine {
    HeaderLine {
        rendered: format!("{text} {}", indicator(ok)),
        visible_width: text.chars().count() + 2,
    }
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
                "/connect".to_string(),
                "/disconnect".to_string(),
                "/reload".to_string(),
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

#[cfg(test)]
mod tests {
    use super::{ANSI_RESET, THINKING_TEXT, render_thinking_frame};
    use std::time::Duration;

    #[test]
    fn thinking_frames_roll_across_characters() {
        let frame_zero = render_thinking_frame(0, Duration::from_secs(61));
        let frame_one = render_thinking_frame(1, Duration::from_secs(61));

        assert!(frame_zero.contains('T'));
        assert!(frame_zero.contains('g'));
        assert!(frame_zero.ends_with(ANSI_RESET));
        assert!(frame_one.ends_with(ANSI_RESET));
        assert_ne!(frame_zero, frame_one);
        assert!(frame_zero.contains("(1m1s)"));
        assert!(frame_one.contains("(1m1s)"));
        for ch in THINKING_TEXT.chars() {
            assert!(frame_zero.contains(ch));
        }
    }
}
