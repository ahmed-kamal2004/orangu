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

use anyhow::{Context, Error, Result, anyhow};
use clap::Parser;
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use markdown::{
    ParseOptions,
    mdast::{List, ListItem, Node},
    to_mdast,
};
use orangu::{
    config::{LlmConfiguration, default_client_config_path, load_client_configuration},
    llm::{StreamMetrics, normalized_openai_endpoint},
    session::ChatSession,
    tools::{ToolExecutor, resolve_workspace_path},
    tui::{
        HeaderStatus, ScreenRenderArgs, StatusFragment, TranscriptLine, help_text,
        output_view_rows, render_screen, render_thinking_status, render_working_status,
    },
};
use serde::Deserialize;
use std::borrow::Cow;
use std::{
    collections::HashMap,
    collections::VecDeque,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
    util::{LinesWithEndings, as_24_bit_terminal_escaped},
};
use terminal_size::{Width, terminal_size};
use tiktoken_rs::cl100k_base;
use walkdir::WalkDir;

const CLEAR_TERMINAL_SEQUENCE: &str = "\x1b[2J\x1b[H";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const TERMINAL_TITLE: &str = "orangu";
const CTRL_C_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const ESC_CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
const IDLE_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const CTRL_C_EXIT_MESSAGE: &str = "Press Ctrl+c again to quit";
const ANSI_BOLD_ON: &str = "\x1b[1m";
const ANSI_BOLD_OFF: &str = "\x1b[22m";
const ANSI_ITALIC_ON: &str = "\x1b[3m";
const ANSI_ITALIC_OFF: &str = "\x1b[23m";
const ANSI_UNDERLINE_ON: &str = "\x1b[4m";
const ANSI_UNDERLINE_OFF: &str = "\x1b[24m";
const ANSI_STRIKETHROUGH_ON: &str = "\x1b[9m";
const ANSI_STRIKETHROUGH_OFF: &str = "\x1b[29m";
const ANSI_FG_CODE: &str = "\x1b[38;2;255;215;120m";
const ANSI_FG_LINK: &str = "\x1b[38;2;102;178;255m";
const ANSI_FG_LIGHT_GREEN: &str = "\x1b[38;2;170;255;170m";
const ANSI_FG_LIGHT_RED: &str = "\x1b[38;2;255;170;170m";
const ANSI_FG_SUBTLE: &str = "\x1b[38;2;180;190;205m";
const ANSI_FG_RESET: &str = "\x1b[39m";
const ANSI_RESET: &str = "\x1b[0m";
const THINKING_FRAME_INTERVAL: Duration = Duration::from_millis(120);
const WAIT_LOOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TRANSCRIPT_MAX_LINES: usize = 10_000;
const HISTORY_DIRECTORY: &str = ".orangu";
const HISTORY_FILE: &str = "orangu.history";
const COMMANDS: &[&str] = &[
    "/help",
    "/connect",
    "/disconnect",
    "/reload",
    "/list_models",
    "/list_files",
    "/show_file",
    "/tools",
    "/model",
    "/diff",
    "/status",
    "/log",
    "/pull",
    "/rebase",
    "/merge",
    "/checkout",
    "/add_file",
    "/remove_file",
    "/move_file",
    "/cherry_pick",
    "/commit",
    "/push",
    "/init_repo",
    "/delete",
    "/open_file",
    "/clear",
    "/quit",
];

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    workspace: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let _terminal_title_guard = TerminalTitleGuard::new(TERMINAL_TITLE);
    let args = Args::parse();
    let config_path = match args.config.or_else(default_client_config_path) {
        Some(path) => path,
        None => {
            return Err(anyhow!(
                "Missing config file; pass --config or add ./orangu.conf or ~/.orangu/orangu.conf"
            ));
        }
    };
    let config = load_client_configuration(&config_path)?;
    let workspace = resolve_workspace_root(args.workspace)?;
    let tools = ToolExecutor::new(&workspace);

    let model_names = sorted_model_names(&config.llms);
    let startup_model = config.default_model.clone();
    let startup_endpoint = config
        .llms
        .get(&startup_model)
        .ok_or_else(|| anyhow!("missing configured profile {}", startup_model))?
        .endpoint
        .clone();
    let mut active_model = startup_model.clone();
    let mut session = ChatSession::new(system_prompt(
        config
            .llms
            .get(&active_model)
            .ok_or_else(|| anyhow!("missing configured profile {}", active_model))?,
    ));
    let mut current_endpoint = Some(startup_endpoint.clone());
    let _terminal_ui_guard = TerminalUiGuard::new()?;

    let mut output_state = OutputState::default();
    let mut interrupt_state = InterruptState::default();
    let mut input_state = InputState::default();
    let mut pending_commands = VecDeque::new();
    let history_path = history_file_path()?;
    let mut history = load_history(&history_path)?;
    let status_http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    loop {
        let prompt_branch = workspace_branch_name(tools.workspace());
        let active_profile = config
            .llms
            .get(&active_model)
            .ok_or_else(|| anyhow!("missing configured profile {}", active_model))?;
        let header_status = probe_header_status(
            &status_http_client,
            tools.workspace(),
            &active_model,
            active_profile,
            current_endpoint.as_deref(),
        )
        .await;
        let render = RenderContext {
            current_model: &active_model,
            endpoint: current_endpoint.as_deref().unwrap_or("(disconnected)"),
            workspace: tools.workspace(),
            prompt_branch: prompt_branch.as_deref(),
            header_status,
        };
        print_screen(
            render,
            ScreenState {
                transcript: output_state.lines(),
                scroll_offset: output_state.scroll_offset(),
                left_status: None,
                pending_count: pending_commands.len(),
                pending_line: None,
                input: input_state.as_str(),
                cursor: input_state.cursor(),
            },
        );
        std::io::stdout().flush()?;

        let next_input = if let Some(queued) = pending_commands.pop_front() {
            queued
        } else {
            match read_input(
                &mut input_state,
                &mut interrupt_state,
                &mut output_state,
                pending_commands.len(),
                InputContext {
                    history: &history,
                    workspace: &workspace,
                    model_names: &model_names,
                    render,
                },
            )? {
                InputResult::Submitted(line) => {
                    let Some(trimmed) = prepare_submitted_input(
                        &line,
                        &mut history,
                        &history_path,
                        &mut output_state,
                        None,
                    )?
                    else {
                        continue;
                    };
                    trimmed
                }
                InputResult::Refresh => continue,
                InputResult::Quit => {
                    print!("{CLEAR_TERMINAL_SEQUENCE}");
                    std::io::stdout().flush()?;
                    break;
                }
            }
        };

        output_state.push_input(&format!("> {next_input}"));
        output_state.reset_scroll();
        print_screen(
            render,
            ScreenState {
                transcript: output_state.lines(),
                scroll_offset: output_state.scroll_offset(),
                left_status: None,
                pending_count: pending_commands.len(),
                pending_line: None,
                input: input_state.as_str(),
                cursor: input_state.cursor(),
            },
        );
        std::io::stdout().flush()?;

        match handle_command(
            &next_input,
            CommandState {
                active_model: &mut active_model,
                current_endpoint: &mut current_endpoint,
                session: &mut session,
            },
            CommandContext {
                startup_model: &startup_model,
                startup_endpoint: &startup_endpoint,
                llms: &config.llms,
                tools: &tools,
                workspace: &workspace,
            },
        )? {
            CommandOutcome::Quit => {
                print!("{CLEAR_TERMINAL_SEQUENCE}");
                std::io::stdout().flush()?;
                break;
            }
            CommandOutcome::Quiet => continue,
            CommandOutcome::Cleared => {
                output_state.clear();
                continue;
            }
            CommandOutcome::Output(output) => {
                output_state.push_text(&output);
                output_state.reset_scroll();
                continue;
            }
            CommandOutcome::Unhandled => {}
        }

        let profile = config
            .llms
            .get(&active_model)
            .ok_or_else(|| anyhow!("unknown model profile '{active_model}'"))?;
        let Some(endpoint) = current_endpoint.as_deref() else {
            output_state.push_text("Error: Not connected to an LLM server");
            output_state.reset_scroll();
            continue;
        };
        if !header_status.model_ok {
            continue;
        }
        if let Some(message) = llm_prompt_block_reason(current_endpoint.as_deref(), header_status) {
            output_state.push_text(message);
            output_state.reset_scroll();
            continue;
        }
        let mut prompt_profile = profile.clone();
        prompt_profile.endpoint = endpoint.to_string();
        match wait_for_response(
            &mut session,
            &next_input,
            &prompt_profile,
            &tools,
            WaitContext {
                render: RenderContext {
                    current_model: &active_model,
                    endpoint,
                    workspace: tools.workspace(),
                    prompt_branch: prompt_branch.as_deref(),
                    header_status,
                },
                history: &mut history,
                history_path: &history_path,
                model_names: &model_names,
                interrupt_state: &mut interrupt_state,
                output_state: &mut output_state,
                input_state: &mut input_state,
                pending_commands: &mut pending_commands,
            },
        )
        .await
        {
            Ok(WaitResult::Response(answer)) => output_state.push_markdown(&answer),
            Ok(WaitResult::Cancelled(partial_output)) => {
                preserve_cancelled_output(&mut output_state, &partial_output);
            }
            Ok(WaitResult::Quit) => {
                print!("{CLEAR_TERMINAL_SEQUENCE}");
                std::io::stdout().flush()?;
                break;
            }
            Err(err) => output_state.push_text(&format!("Error: {err:#}")),
        }
        output_state.reset_scroll();
    }

    Ok(())
}

#[derive(Default)]
struct OutputState {
    transcript: Vec<TranscriptLine>,
    scroll_offset: usize,
}

impl OutputState {
    fn lines(&self) -> &[TranscriptLine] {
        &self.transcript
    }

    fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    fn clear(&mut self) {
        self.transcript.clear();
        self.scroll_offset = 0;
    }

    fn push_text(&mut self, text: &str) {
        self.push_lines(
            text.lines()
                .map(|line| TranscriptLine::Plain(line.to_owned())),
        );
    }

    fn push_input(&mut self, text: &str) {
        self.push_lines(
            text.lines()
                .map(|line| TranscriptLine::UserInput(line.to_owned())),
        );
    }

    fn push_lines<I>(&mut self, lines: I)
    where
        I: Iterator<Item = TranscriptLine>,
    {
        let collected = lines.collect::<Vec<_>>();
        let added_lines = collected.len();
        self.transcript.extend(collected);
        if self.scroll_offset > 0 {
            self.scroll_offset = self.scroll_offset.saturating_add(added_lines);
        }

        let excess = self.transcript.len().saturating_sub(TRANSCRIPT_MAX_LINES);
        if excess > 0 {
            self.transcript.drain(0..excess);
            self.scroll_offset = self.scroll_offset.saturating_sub(excess);
        }
    }

    fn push_markdown(&mut self, text: &str) {
        self.push_text(&render_markdown_for_console(text));
    }

    fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }

    fn page_up(&mut self, rows: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(rows.max(1));
    }

    fn page_down(&mut self, rows: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(rows.max(1));
    }
}

enum InterruptAction {
    Continue,
    Exit,
}

#[derive(Debug, Default)]
struct InterruptState {
    last_interrupt: Option<Instant>,
}

impl InterruptState {
    fn reset(&mut self) {
        self.last_interrupt = None;
    }

    fn handle_interrupt(&mut self, now: Instant) -> InterruptAction {
        if let Some(last_interrupt) = self.last_interrupt
            && now.duration_since(last_interrupt) <= CTRL_C_EXIT_TIMEOUT
        {
            self.last_interrupt = None;
            return InterruptAction::Exit;
        }

        self.last_interrupt = Some(now);
        InterruptAction::Continue
    }
}

struct TerminalTitleGuard;

impl TerminalTitleGuard {
    fn new(title: &str) -> Self {
        set_terminal_title(Some(title));
        Self
    }
}

impl Drop for TerminalTitleGuard {
    fn drop(&mut self) {
        set_terminal_title(None);
    }
}

fn set_terminal_title(title: Option<&str>) {
    match title {
        Some(title) => print!("\x1b]0;{title}\x07"),
        None => print!("\x1b]0;\x07"),
    }
}

struct TerminalUiGuard;

impl TerminalUiGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalUiGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

struct RawModePauseGuard;

impl RawModePauseGuard {
    fn new() -> Result<Self> {
        disable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModePauseGuard {
    fn drop(&mut self) {
        let _ = enable_raw_mode();
    }
}

struct SyntaxHighlightAssets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

#[derive(Clone, Copy, Default)]
struct ShowFileOptions {
    show_hash: bool,
    show_author: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitLineMetadata {
    hash: String,
    author: String,
}

fn syntax_highlight_assets() -> &'static SyntaxHighlightAssets {
    static ASSETS: OnceLock<SyntaxHighlightAssets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .or_else(|| themes.themes.values().next().cloned())
            .unwrap_or_default();
        SyntaxHighlightAssets { syntaxes, theme }
    })
}

fn show_file_output(workspace: &Path, raw_args: &str) -> Result<String> {
    let (path, options) = parse_show_file_arguments(raw_args)?;
    let resolved_path = resolve_workspace_path(workspace, &path)?;
    if !options.show_hash
        && !options.show_author
        && let Some(output) = show_file_output_with_bat(&resolved_path)?
    {
        return Ok(output);
    }
    let content = fs::read_to_string(&resolved_path)
        .with_context(|| format!("failed to read {}", resolved_path.display()))?;
    let blame = if options.show_hash || options.show_author {
        Some(git_blame_metadata(workspace, &resolved_path)?)
    } else {
        None
    };

    render_show_file_content(&resolved_path, &content, blame.as_deref(), options)
}

fn show_file_output_with_bat(path: &Path) -> Result<Option<String>> {
    let output = match std::process::Command::new("bat")
        .arg("--paging=never")
        .arg("--color=always")
        .arg("--style=numbers")
        .arg("--terminal-width")
        .arg(current_terminal_width().to_string())
        .arg(path)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to run bat for {}", path.display()));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "bat failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }

    String::from_utf8(output.stdout)
        .map(Some)
        .with_context(|| format!("bat output for {} was not UTF-8", path.display()))
}

fn parse_show_file_arguments(raw_args: &str) -> Result<(String, ShowFileOptions)> {
    let args = shell_words(raw_args)
        .map_err(|_| LocalError::Usage(show_file_usage_message().to_string()))?;
    let mut options = ShowFileOptions::default();
    let mut path = None;

    for arg in args {
        match arg.as_str() {
            "--hash" => options.show_hash = true,
            "--author" => options.show_author = true,
            _ if arg.starts_with('-') => {
                return Err(LocalError::Usage(format!(
                    "Unknown option '{arg}'. {}",
                    show_file_usage_message()
                ))
                .into());
            }
            _ if path.is_none() => path = Some(arg),
            _ => {
                return Err(LocalError::Usage(show_file_usage_message().to_string()).into());
            }
        }
    }

    let path = path.ok_or_else(|| LocalError::Usage(show_file_usage_message().to_string()))?;
    Ok((path, options))
}

fn open_file_usage_message() -> &'static str {
    "Usage: /open_file <path>. Use /help to see available commands."
}

fn show_file_usage_message() -> &'static str {
    "Usage: /show_file [--hash] [--author] <path>. Use /help to see available commands."
}

fn model_usage_message() -> &'static str {
    "Usage: /model <name>. Use /help to see available commands."
}

fn connect_usage_message() -> &'static str {
    "Usage: /connect <endpoint>. Use /help to see available commands."
}

fn pull_usage_message() -> &'static str {
    "Usage: /pull <number>. Use /help to see available commands."
}

fn merge_usage_message() -> &'static str {
    "Usage: /merge <branch>. Use /help to see available commands."
}

fn checkout_usage_message() -> &'static str {
    "Usage: /checkout <branch|file>. Use /help to see available commands."
}

fn add_file_usage_message() -> &'static str {
    "Usage: /add_file <path>. Use /help to see available commands."
}

fn remove_file_usage_message() -> &'static str {
    "Usage: /remove_file <path>. Use /help to see available commands."
}

fn move_file_usage_message() -> &'static str {
    "Usage: /move_file <source> <destination>. Use /help to see available commands."
}

fn cherry_pick_usage_message() -> &'static str {
    "Usage: /cherry_pick <commit>. Use /help to see available commands."
}

fn commit_usage_message() -> &'static str {
    "Usage: /commit <message>. Use /help to see available commands."
}

#[derive(Debug)]
enum LocalError {
    Usage(String),
}

impl std::fmt::Display for LocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalError::Usage(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for LocalError {}

fn local_command_error(err: Error) -> CommandOutcome {
    if err.is::<LocalError>() {
        CommandOutcome::Output(format!("{err}"))
    } else {
        CommandOutcome::Output(format!("Error: {err:#}"))
    }
}

fn render_show_file_content(
    path: &Path,
    content: &str,
    blame: Option<&[GitLineMetadata]>,
    options: ShowFileOptions,
) -> Result<String> {
    let assets = syntax_highlight_assets();
    let syntax = assets
        .syntaxes
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .unwrap_or_else(|| assets.syntaxes.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, &assets.theme);
    let line_count = content.lines().count().max(1);
    let line_number_width = line_count.to_string().len();
    let mut rendered = Vec::new();

    if content.is_empty() {
        rendered.push(format_show_file_line(
            1,
            "",
            blame.and_then(|metadata| metadata.first()),
            options,
            line_number_width,
        ));
        return Ok(rendered.join("\n"));
    }

    for (index, line) in LinesWithEndings::from(content).enumerate() {
        let line_no = index + 1;
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        let highlighted = highlight_source_line(&mut highlighter, &assets.syntaxes, line)?;
        let highlighted = highlighted.trim_end_matches(['\r', '\n']);
        let rendered_line = if line_without_newline.is_empty() {
            String::new()
        } else {
            highlighted.to_string()
        };
        rendered.push(format_show_file_line(
            line_no,
            &rendered_line,
            blame.and_then(|metadata| metadata.get(index)),
            options,
            line_number_width,
        ));
    }

    Ok(rendered.join("\n"))
}

fn highlight_source_line(
    highlighter: &mut HighlightLines<'_>,
    syntaxes: &SyntaxSet,
    line: &str,
) -> Result<String> {
    let ranges = highlighter
        .highlight_line(line, syntaxes)
        .map_err(|err| anyhow!("failed to highlight source line: {err}"))?;
    Ok(as_24_bit_terminal_escaped(&ranges, false))
}

fn format_show_file_line(
    line_no: usize,
    line: &str,
    metadata: Option<&GitLineMetadata>,
    options: ShowFileOptions,
    line_number_width: usize,
) -> String {
    let mut parts = vec![format!("{line_no:>line_number_width$}")];
    if options.show_hash
        && let Some(metadata) = metadata
    {
        parts.push(metadata.hash.clone());
    }
    if options.show_author
        && let Some(metadata) = metadata
    {
        parts.push(metadata.author.clone());
    }
    if !line.is_empty() {
        parts.push(format!("{ANSI_RESET}{line}{ANSI_RESET}"));
    }
    parts.join(" ")
}

fn git_blame_metadata(workspace: &Path, path: &Path) -> Result<Vec<GitLineMetadata>> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("Git blame metadata is only available inside a Git repository"))?;
    let relative_path = path
        .strip_prefix(&repo_root)
        .with_context(|| format!("{} is outside the Git repository", path.display()))?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .arg("blame")
        .arg("--line-porcelain")
        .arg("--abbrev=8")
        .arg("--")
        .arg(relative_path)
        .output()
        .context("failed to run git blame")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git blame failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }

    let stdout = String::from_utf8(output.stdout).context("git blame output was not UTF-8")?;
    let mut metadata = Vec::new();
    let mut current_hash = String::new();
    let mut current_author = String::new();
    for line in stdout.lines() {
        if let Some(content) = line.strip_prefix('\t') {
            let _ = content;
            metadata.push(GitLineMetadata {
                hash: current_hash.clone(),
                author: current_author.clone(),
            });
            continue;
        }

        if let Some(author) = line.strip_prefix("author ") {
            current_author = author.to_string();
            continue;
        }

        let mut parts = line.split_whitespace();
        if let (Some(hash), Some(_orig), Some(_final)) = (parts.next(), parts.next(), parts.next())
            && hash.chars().all(|ch| ch.is_ascii_hexdigit())
            && hash.len() >= 8
        {
            current_hash = hash.chars().take(8).collect();
            current_author.clear();
        }
    }

    Ok(metadata)
}

fn render_markdown_for_console(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    match to_mdast(text, &ParseOptions::default()) {
        Ok(tree) => render_markdown_node(&tree),
        Err(_) => text.to_string(),
    }
}

fn render_markdown_node(node: &Node) -> String {
    match node {
        Node::Root(root) => render_block_nodes(&root.children, false),
        Node::Paragraph(paragraph) => render_inline_nodes(&paragraph.children),
        Node::Heading(heading) => format!(
            "{ANSI_BOLD_ON}{} {}{ANSI_BOLD_OFF}",
            "#".repeat(heading.depth.into()),
            render_inline_nodes(&heading.children)
        ),
        Node::Blockquote(blockquote) => {
            prefix_lines(&render_block_nodes(&blockquote.children, false), "> ")
        }
        Node::List(list) => render_list(list),
        Node::ListItem(item) => render_list_item(item, "-", 2),
        Node::Code(code) => render_code_block(code.lang.as_deref(), &code.value),
        Node::ThematicBreak(_) => "-".repeat(40),
        Node::Table(table) => render_table(&table.children),
        Node::Definition(_) => String::new(),
        Node::Break(_) => "\n".to_string(),
        _ => render_inline_node(node),
    }
}

fn render_block_nodes(nodes: &[Node], compact: bool) -> String {
    let separator = if compact { "\n" } else { "\n\n" };
    nodes
        .iter()
        .map(render_markdown_node)
        .filter(|rendered| !rendered.trim().is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

fn render_inline_nodes(nodes: &[Node]) -> String {
    nodes.iter().map(render_inline_node).collect()
}

fn render_inline_node(node: &Node) -> String {
    match node {
        Node::Text(text) => text.value.clone(),
        Node::Strong(strong) => format!(
            "{ANSI_BOLD_ON}{}{ANSI_BOLD_OFF}",
            render_inline_nodes(&strong.children)
        ),
        Node::Emphasis(emphasis) => format!(
            "{ANSI_ITALIC_ON}{}{ANSI_ITALIC_OFF}",
            render_inline_nodes(&emphasis.children)
        ),
        Node::Delete(delete) => format!(
            "{ANSI_STRIKETHROUGH_ON}{}{ANSI_STRIKETHROUGH_OFF}",
            render_inline_nodes(&delete.children)
        ),
        Node::InlineCode(code) => {
            format!("{ANSI_FG_CODE}`{}{ANSI_FG_RESET}`", code.value)
        }
        Node::InlineMath(math) => {
            format!("{ANSI_FG_CODE}${}{ANSI_FG_RESET}$", math.value)
        }
        Node::Link(link) => render_link(&render_inline_nodes(&link.children), &link.url),
        Node::LinkReference(link) => render_inline_nodes(&link.children),
        Node::Image(image) => format!("[image: {}] ({})", image.alt, image.url),
        Node::ImageReference(image) => format!("[image: {}]", image.alt),
        Node::FootnoteReference(reference) => format!("[^{}]", reference.identifier),
        Node::Break(_) => "\n".to_string(),
        Node::Html(html) => html.value.clone(),
        Node::Math(math) => math.value.clone(),
        Node::MdxFlowExpression(expression) => expression.value.clone(),
        Node::MdxTextExpression(expression) => expression.value.clone(),
        Node::MdxjsEsm(esm) => esm.value.clone(),
        Node::Toml(toml) => toml.value.clone(),
        Node::Yaml(yaml) => yaml.value.clone(),
        _ => render_markdown_node(node),
    }
}

fn render_link(label: &str, url: &str) -> String {
    if label.is_empty() || label == url {
        return format!(
            "{ANSI_FG_LINK}{ANSI_UNDERLINE_ON}{url}{ANSI_UNDERLINE_OFF}{ANSI_FG_RESET}"
        );
    }

    format!(
        "{ANSI_FG_LINK}{ANSI_UNDERLINE_ON}{label}{ANSI_UNDERLINE_OFF}{ANSI_FG_RESET}{ANSI_FG_SUBTLE} ({url}){ANSI_FG_RESET}"
    )
}

fn render_list(list: &List) -> String {
    let start = list.start.unwrap_or(1);
    list.children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| match child {
            Node::ListItem(item) => {
                let marker = if list.ordered {
                    format!("{}.", start + index as u32)
                } else {
                    "-".to_string()
                };
                Some(render_list_item(item, &marker, marker.len() + 1))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_list_item(item: &ListItem, marker: &str, indent: usize) -> String {
    let body = render_block_nodes(&item.children, !item.spread);
    indent_lines(&body, &format!("{marker} "), &" ".repeat(indent))
}

fn render_code_block(language: Option<&str>, value: &str) -> String {
    let mut lines = Vec::new();
    let opener = match language {
        Some(language) if !language.is_empty() => format!("```{language}"),
        _ => "```".to_string(),
    };
    lines.push(format!("{ANSI_FG_CODE}{opener}{ANSI_FG_RESET}"));
    if value.is_empty() {
        lines.push(String::new());
    } else {
        lines.extend(render_syntax_highlighted_code(language, value));
    }
    lines.push(format!("{ANSI_FG_CODE}```{ANSI_FG_RESET}"));
    lines.join("\n")
}

fn render_syntax_highlighted_code(language: Option<&str>, value: &str) -> Vec<String> {
    let language = language.and_then(|language| {
        let trimmed = language.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    });
    let Some(language) = language else {
        return render_plain_code_lines(value);
    };

    let assets = syntax_highlight_assets();
    let Some(syntax) = assets
        .syntaxes
        .find_syntax_by_token(language)
        .or_else(|| assets.syntaxes.find_syntax_by_extension(language))
    else {
        return render_plain_code_lines(value);
    };

    let mut highlighter = HighlightLines::new(syntax, &assets.theme);
    let mut rendered = Vec::new();
    for line in LinesWithEndings::from(value) {
        match highlighter.highlight_line(line, &assets.syntaxes) {
            Ok(ranges) => {
                let mut escaped = as_24_bit_terminal_escaped(&ranges, false);
                while escaped.ends_with('\n') {
                    escaped.pop();
                }
                rendered.push(escaped);
            }
            Err(_) => return render_plain_code_lines(value),
        }
    }
    if rendered.is_empty() {
        render_plain_code_lines(value)
    } else {
        rendered
    }
}

fn render_plain_code_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }

    value
        .lines()
        .map(|line| format!("{ANSI_FG_CODE}{line}{ANSI_FG_RESET}"))
        .collect()
}

fn render_table(rows: &[Node]) -> String {
    let rendered_rows = rows
        .iter()
        .filter_map(|row| match row {
            Node::TableRow(row) => Some(
                row.children
                    .iter()
                    .filter_map(|cell| match cell {
                        Node::TableCell(cell) => Some(render_inline_nodes(&cell.children)),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();

    if rendered_rows.is_empty() {
        return String::new();
    }

    let mut lines = Vec::with_capacity(rendered_rows.len() + 1);
    for (index, row) in rendered_rows.iter().enumerate() {
        lines.push(format!("| {} |", row.join(" | ")));
        if index == 0 {
            lines.push(format!(
                "| {} |",
                row.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
            ));
        }
    }
    lines.join("\n")
}

fn prefix_lines(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn indent_lines(text: &str, first_prefix: &str, rest_prefix: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                first_prefix
            } else {
                rest_prefix
            };
            format!("{prefix}{line}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct InputState {
    buffer: String,
    cursor: usize,
    completion: Option<CompletionState>,
    history_index: Option<usize>,
    history_draft: String,
}

impl InputState {
    fn as_str(&self) -> &str {
        &self.buffer
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.completion = None;
        self.history_index = None;
        self.history_draft.clear();
    }

    fn set_buffer(&mut self, buffer: String) {
        self.buffer = buffer;
        self.cursor = self.buffer.len();
        self.completion = None;
    }

    fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.completion = None;
    }

    fn insert_str(&mut self, text: &str) {
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.completion = None;
    }

    fn backspace(&mut self) {
        if let Some(previous) = previous_boundary(&self.buffer, self.cursor) {
            self.buffer.drain(previous..self.cursor);
            self.cursor = previous;
            self.completion = None;
        }
    }

    fn delete(&mut self) {
        if let Some(next) = next_boundary(&self.buffer, self.cursor) {
            self.buffer.drain(self.cursor..next);
            self.completion = None;
        }
    }

    fn move_left(&mut self) {
        if let Some(previous) = previous_boundary(&self.buffer, self.cursor) {
            self.cursor = previous;
            self.completion = None;
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = next_boundary(&self.buffer, self.cursor) {
            self.cursor = next;
            self.completion = None;
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
        self.completion = None;
    }

    fn move_end(&mut self) {
        self.cursor = self.buffer.len();
        self.completion = None;
    }

    fn kill_to_end(&mut self) {
        self.buffer.truncate(self.cursor);
        self.completion = None;
    }

    fn kill_to_start(&mut self) {
        self.buffer.drain(..self.cursor);
        self.cursor = 0;
        self.completion = None;
    }

    fn delete_prev_word(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let mut start = self.cursor;
        while let Some(previous) = previous_boundary(&self.buffer, start) {
            if !self.buffer[previous..start]
                .chars()
                .all(char::is_whitespace)
            {
                start = previous;
                break;
            }
            start = previous;
            if start == 0 {
                break;
            }
        }

        while let Some(previous) = previous_boundary(&self.buffer, start) {
            if self.buffer[previous..start]
                .chars()
                .all(char::is_whitespace)
            {
                break;
            }
            start = previous;
            if start == 0 {
                break;
            }
        }

        self.buffer.drain(start..self.cursor);
        self.cursor = start;
        self.completion = None;
    }

    fn delete_backward_readline_word(&mut self) {
        let start = readline_word_start(&self.buffer, self.cursor);
        if start < self.cursor {
            self.buffer.drain(start..self.cursor);
            self.cursor = start;
            self.completion = None;
        }
    }

    fn delete_forward_readline_word(&mut self) {
        let end = readline_word_end(&self.buffer, self.cursor);
        if end > self.cursor {
            self.buffer.drain(self.cursor..end);
            self.completion = None;
        }
    }

    fn move_backward_readline_word(&mut self) {
        let start = readline_word_start(&self.buffer, self.cursor);
        if start != self.cursor {
            self.cursor = start;
            self.completion = None;
        }
    }

    fn move_forward_readline_word(&mut self) {
        let end = readline_word_end(&self.buffer, self.cursor);
        if end != self.cursor {
            self.cursor = end;
            self.completion = None;
        }
    }
}

struct CompletionState {
    start: usize,
    end: usize,
    original: String,
    candidates: Vec<String>,
    index: usize,
}

enum InputResult {
    Submitted(String),
    Refresh,
    Quit,
}

enum WaitResult {
    Response(String),
    Cancelled(String),
    Quit,
}

struct InputEventResult {
    redraw: bool,
    outcome: Option<InputResult>,
}

#[derive(Clone, Copy)]
struct RenderContext<'a> {
    current_model: &'a str,
    endpoint: &'a str,
    workspace: &'a Path,
    prompt_branch: Option<&'a str>,
    header_status: HeaderStatus,
}

#[derive(Clone)]
struct ScreenState<'a> {
    transcript: &'a [TranscriptLine],
    scroll_offset: usize,
    left_status: Option<StatusFragment>,
    pending_count: usize,
    pending_line: Option<&'a str>,
    input: &'a str,
    cursor: usize,
}

#[derive(Clone, Default)]
struct StreamRenderState {
    output: String,
    metrics: StreamMetrics,
}

#[derive(Debug, Default)]
struct EscapeCancelState {
    last_escape: Option<Instant>,
}

impl EscapeCancelState {
    fn reset(&mut self) {
        self.last_escape = None;
    }

    fn handle_escape(&mut self, now: Instant) -> bool {
        if let Some(last_escape) = self.last_escape
            && now.duration_since(last_escape) <= ESC_CANCEL_TIMEOUT
        {
            self.last_escape = None;
            return true;
        }

        self.last_escape = Some(now);
        false
    }
}

#[derive(Clone, Copy)]
struct InputContext<'a> {
    history: &'a [String],
    workspace: &'a Path,
    model_names: &'a [String],
    render: RenderContext<'a>,
}

struct WaitContext<'a> {
    render: RenderContext<'a>,
    history: &'a mut Vec<String>,
    history_path: &'a Path,
    model_names: &'a [String],
    interrupt_state: &'a mut InterruptState,
    output_state: &'a mut OutputState,
    input_state: &'a mut InputState,
    pending_commands: &'a mut VecDeque<String>,
}

fn read_input(
    input_state: &mut InputState,
    interrupt_state: &mut InterruptState,
    output_state: &mut OutputState,
    pending_count: usize,
    input_context: InputContext<'_>,
) -> Result<InputResult> {
    let refresh_deadline = Instant::now() + IDLE_STATUS_REFRESH_INTERVAL;

    loop {
        let timeout = idle_status_refresh_timeout(refresh_deadline, Instant::now());
        if !event::poll(timeout)? {
            return Ok(InputResult::Refresh);
        }

        let result = handle_input_event(
            event::read()?,
            input_state,
            interrupt_state,
            output_state,
            input_context,
        );

        if let Some(outcome) = result.outcome {
            return Ok(outcome);
        }

        if Instant::now() >= refresh_deadline {
            return Ok(InputResult::Refresh);
        }

        if result.redraw {
            print_screen(
                input_context.render,
                ScreenState {
                    transcript: output_state.lines(),
                    scroll_offset: output_state.scroll_offset(),
                    left_status: None,
                    pending_count,
                    pending_line: None,
                    input: input_state.as_str(),
                    cursor: input_state.cursor(),
                },
            );
            std::io::stdout().flush()?;
        }
    }
}

fn idle_status_refresh_timeout(refresh_deadline: Instant, now: Instant) -> Duration {
    refresh_deadline
        .checked_duration_since(now)
        .unwrap_or(Duration::ZERO)
}

fn handle_input_event(
    event: Event,
    input_state: &mut InputState,
    interrupt_state: &mut InterruptState,
    output_state: &mut OutputState,
    input_context: InputContext<'_>,
) -> InputEventResult {
    let mut redraw = false;

    match event {
        Event::Paste(text) => {
            interrupt_state.reset();
            input_state.insert_str(&text);
            redraw = true;
        }
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) if kind == KeyEventKind::Press || kind == KeyEventKind::Repeat => {
            match (code, modifiers) {
                (KeyCode::Left, modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    interrupt_state.reset();
                    input_state.move_backward_readline_word();
                    redraw = true;
                }
                (KeyCode::Right, modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    interrupt_state.reset();
                    input_state.move_forward_readline_word();
                    redraw = true;
                }
                (KeyCode::Backspace, modifiers)
                    if modifiers.contains(KeyModifiers::ALT)
                        && !modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    interrupt_state.reset();
                    input_state.delete_backward_readline_word();
                    redraw = true;
                }
                (KeyCode::Char(ch), modifiers)
                    if modifiers.contains(KeyModifiers::ALT)
                        && !modifiers.contains(KeyModifiers::CONTROL)
                        && ch.eq_ignore_ascii_case(&'d') =>
                {
                    interrupt_state.reset();
                    input_state.delete_forward_readline_word();
                    redraw = true;
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    match interrupt_state.handle_interrupt(Instant::now()) {
                        InterruptAction::Continue => {
                            output_state.push_text(CTRL_C_EXIT_MESSAGE);
                            output_state.reset_scroll();
                            input_state.clear();
                            return InputEventResult {
                                redraw: true,
                                outcome: Some(InputResult::Submitted(String::new())),
                            };
                        }
                        InterruptAction::Exit => {
                            return InputEventResult {
                                redraw: false,
                                outcome: Some(InputResult::Quit),
                            };
                        }
                    }
                }
                (KeyCode::Char('d'), KeyModifiers::CONTROL) if input_state.as_str().is_empty() => {
                    return InputEventResult {
                        redraw: false,
                        outcome: Some(InputResult::Quit),
                    };
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    interrupt_state.reset();
                    let input = input_state.buffer.clone();
                    input_state.clear();
                    return InputEventResult {
                        redraw: false,
                        outcome: Some(InputResult::Submitted(input)),
                    };
                }
                (KeyCode::Backspace, _) => {
                    interrupt_state.reset();
                    input_state.backspace();
                    redraw = true;
                }
                (KeyCode::Delete, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    interrupt_state.reset();
                    input_state.delete();
                    redraw = true;
                }
                (KeyCode::Left, _) => {
                    interrupt_state.reset();
                    input_state.move_left();
                    redraw = true;
                }
                (KeyCode::Right, _) => {
                    interrupt_state.reset();
                    input_state.move_right();
                    redraw = true;
                }
                (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                    interrupt_state.reset();
                    input_state.move_home();
                    redraw = true;
                }
                (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                    interrupt_state.reset();
                    input_state.move_end();
                    redraw = true;
                }
                (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                    interrupt_state.reset();
                    input_state.kill_to_end();
                    redraw = true;
                }
                (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    interrupt_state.reset();
                    input_state.kill_to_start();
                    redraw = true;
                }
                (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                    interrupt_state.reset();
                    input_state.delete_prev_word();
                    redraw = true;
                }
                (KeyCode::Up, _) => {
                    interrupt_state.reset();
                    history_previous(input_state, input_context.history);
                    redraw = true;
                }
                (KeyCode::Down, _) => {
                    interrupt_state.reset();
                    history_next(input_state, input_context.history);
                    redraw = true;
                }
                (KeyCode::Tab, _) => {
                    interrupt_state.reset();
                    apply_completion(
                        input_state,
                        input_context.workspace,
                        input_context.model_names,
                    );
                    redraw = true;
                }
                (KeyCode::PageUp, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                    interrupt_state.reset();
                    output_state.page_up(output_view_rows(
                        VERSION,
                        input_context.render.current_model,
                        input_context.render.endpoint,
                        input_context.workspace,
                        input_context.render.prompt_branch,
                        input_context.render.header_status,
                        input_state.as_str(),
                    ));
                    redraw = true;
                }
                (KeyCode::PageDown, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                    interrupt_state.reset();
                    output_state.page_down(output_view_rows(
                        VERSION,
                        input_context.render.current_model,
                        input_context.render.endpoint,
                        input_context.workspace,
                        input_context.render.prompt_branch,
                        input_context.render.header_status,
                        input_state.as_str(),
                    ));
                    redraw = true;
                }
                (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                    interrupt_state.reset();
                    input_state.insert_char(ch);
                    redraw = true;
                }
                _ => {}
            }
        }
        _ => {}
    }

    InputEventResult {
        redraw,
        outcome: None,
    }
}

fn history_previous(input_state: &mut InputState, history: &[String]) {
    if history.is_empty() {
        return;
    }

    let new_index = match input_state.history_index {
        Some(0) => 0,
        Some(index) => index.saturating_sub(1),
        None => {
            input_state.history_draft = input_state.buffer.clone();
            history.len() - 1
        }
    };

    input_state.history_index = Some(new_index);
    input_state.set_buffer(history[new_index].clone());
}

fn history_next(input_state: &mut InputState, history: &[String]) {
    let Some(index) = input_state.history_index else {
        return;
    };

    if index + 1 >= history.len() {
        input_state.history_index = None;
        let draft = std::mem::take(&mut input_state.history_draft);
        input_state.set_buffer(draft);
        return;
    }

    let new_index = index + 1;
    input_state.history_index = Some(new_index);
    input_state.set_buffer(history[new_index].clone());
}

fn apply_completion(
    input_state: &mut InputState,
    workspace: &std::path::Path,
    model_names: &[String],
) {
    if let Some(state) = input_state.completion.as_mut()
        && !state.candidates.is_empty()
    {
        state.index = (state.index + 1) % state.candidates.len();
        let start = state.start;
        let end = state.end;
        let original = state.original.clone();
        let candidate = state.candidates[state.index].clone();
        apply_completion_candidate(input_state, start, end, &original, &candidate);
        return;
    }

    let Some((start, end, candidates)) = completion_candidates(
        input_state.as_str(),
        input_state.cursor(),
        workspace,
        model_names,
    ) else {
        return;
    };
    if candidates.is_empty() {
        return;
    }

    let original = input_state.buffer.clone();
    let candidate = candidates[0].clone();
    apply_completion_candidate(input_state, start, end, &original, &candidate);
    input_state.completion = Some(CompletionState {
        start,
        end,
        original,
        candidates,
        index: 0,
    });
}

fn apply_completion_candidate(
    input_state: &mut InputState,
    start: usize,
    end: usize,
    original: &str,
    candidate: &str,
) {
    let mut buffer = String::new();
    buffer.push_str(&original[..start]);
    buffer.push_str(candidate);
    buffer.push_str(&original[end..]);
    input_state.buffer = buffer;
    input_state.cursor = start + candidate.len();
}

fn completion_candidates(
    input: &str,
    cursor: usize,
    workspace: &std::path::Path,
    model_names: &[String],
) -> Option<(usize, usize, Vec<String>)> {
    let cursor = cursor.min(input.len());
    let prefix = &input[..cursor];

    if let Some((start, candidates)) = show_file_completion_candidates(prefix, workspace) {
        return Some((start, cursor, candidates));
    }

    if let Some((start, path_prefix)) = open_file_completion_prefix(prefix) {
        return Some((
            start,
            cursor,
            open_file_completion_candidates(path_prefix, workspace),
        ));
    }

    if let Some((start, path_prefix)) = natural_show_file_completion_prefix(prefix) {
        return Some((
            start,
            cursor,
            open_file_completion_candidates(path_prefix, workspace),
        ));
    }

    if let Some(model_prefix) = prefix.strip_prefix("/model ") {
        return Some((
            7,
            cursor,
            model_names
                .iter()
                .filter(|model| model.starts_with(model_prefix))
                .cloned()
                .collect(),
        ));
    }

    if let Some((start, candidates)) = checkout_completion_candidates(prefix, workspace) {
        return Some((start, cursor, candidates));
    }

    if let Some((start, candidates)) = add_file_completion_candidates(prefix, workspace) {
        return Some((start, cursor, candidates));
    }

    if let Some((start, candidates)) = remove_file_completion_candidates(prefix, workspace) {
        return Some((start, cursor, candidates));
    }

    if let Some((start, candidates)) = move_file_completion_candidates(prefix, workspace) {
        return Some((start, cursor, candidates));
    }

    if let Some((start, candidates)) = cherry_pick_completion_candidates(prefix, workspace) {
        return Some((start, cursor, candidates));
    }

    if let Some((start, branch_prefix)) = merge_completion_prefix(prefix) {
        let branches = discover_git_root(workspace)
            .map(|root| git_branch_names(&root))
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.starts_with(branch_prefix))
            .collect();
        return Some((start, cursor, branches));
    }

    if let Some((start, branch_prefix)) = delete_branch_completion_prefix(prefix) {
        let branches = discover_git_root(workspace)
            .map(|root| git_local_branch_names(&root))
            .unwrap_or_default()
            .into_iter()
            .filter(|b| !is_protected_branch(b) && b.starts_with(branch_prefix))
            .collect();
        return Some((start, cursor, branches));
    }

    if prefix.starts_with('/') {
        return Some((
            0,
            cursor,
            COMMANDS
                .iter()
                .filter(|command| command.starts_with(prefix))
                .map(|command| (*command).to_string())
                .collect(),
        ));
    }

    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    let token = &prefix[start..];
    Some((start, cursor, file_completion_candidates(token, workspace)))
}

fn file_completion_candidates(token: &str, workspace: &std::path::Path) -> Vec<String> {
    let (directory, prefix) = match token.rsplit_once('/') {
        Some((directory, prefix)) => (directory, prefix),
        None => ("", token),
    };
    let gitignore = workspace_gitignore(workspace);
    let search_dir = if directory.is_empty() {
        workspace.to_path_buf()
    } else {
        workspace.join(directory)
    };

    let Ok(entries) = fs::read_dir(search_dir) else {
        return Vec::new();
    };

    let mut matches = entries
        .flatten()
        .filter_map(|entry| {
            let entry_type = entry.file_type().ok()?;
            if !should_include_completion_path(
                workspace,
                &entry.path(),
                entry_type.is_dir(),
                gitignore.as_ref(),
            ) {
                return None;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            if !file_name.starts_with(prefix) {
                return None;
            }

            let suffix = if entry_type.is_dir() { "/" } else { "" };
            Some(if directory.is_empty() {
                format!("{file_name}{suffix}")
            } else {
                format!("{directory}/{file_name}{suffix}")
            })
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
}

fn show_file_completion_candidates(prefix: &str, workspace: &Path) -> Option<(usize, Vec<String>)> {
    let remainder = prefix.strip_prefix("/show_file ")?;
    let (token_start, token) = last_shell_token(remainder);
    let previous = remainder[..token_start].trim_end();
    let previous_tokens = if previous.is_empty() {
        Vec::new()
    } else {
        shell_words(previous).unwrap_or_default()
    };
    let has_path = previous_tokens.iter().any(|value| !value.starts_with('-'));

    let mut candidates = if token.starts_with('-') {
        show_file_flag_candidates(token)
    } else if has_path {
        Vec::new()
    } else {
        open_file_completion_candidates(token, workspace)
    };
    candidates.sort();
    candidates.dedup();
    Some(("/show_file ".len() + token_start, candidates))
}

fn last_shell_token(input: &str) -> (usize, &str) {
    let mut quote = None;
    let mut escaped = false;
    let mut token_start = 0;
    let mut in_token = false;

    for (index, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else if active_quote == '"' && ch == '\\' {
                escaped = true;
            }
            continue;
        }

        if ch.is_whitespace() {
            in_token = false;
            token_start = index + ch.len_utf8();
            continue;
        }

        if !in_token {
            token_start = index;
            in_token = true;
        }

        if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch == '\\' {
            escaped = true;
        }
    }

    (token_start, &input[token_start..])
}

fn show_file_flag_candidates(token: &str) -> Vec<String> {
    ["--hash", "--author"]
        .into_iter()
        .filter(|flag| flag.starts_with(token))
        .map(str::to_string)
        .collect()
}

fn open_file_completion_prefix(prefix: &str) -> Option<(usize, &str)> {
    if let Some(path_prefix) = prefix.strip_prefix("/open_file ") {
        return Some(("/open_file ".len(), path_prefix));
    }

    for command_prefix in ["open file ", "open ", "edit file ", "edit "] {
        if let Some(path_prefix) = strip_ascii_prefix(prefix, command_prefix) {
            return Some((prefix.len() - path_prefix.len(), path_prefix));
        }
    }

    None
}

fn natural_show_file_completion_prefix(prefix: &str) -> Option<(usize, &str)> {
    if let Some(path_prefix) = strip_ascii_prefix(prefix, "show file ") {
        return Some((prefix.len() - path_prefix.len(), path_prefix));
    }

    let path_prefix = strip_ascii_prefix(prefix, "show ")?;
    let (token_start, _) = last_shell_token(path_prefix);
    if token_start != 0 {
        return None;
    }

    Some((prefix.len() - path_prefix.len(), path_prefix))
}

fn checkout_completion_candidates(prefix: &str, workspace: &Path) -> Option<(usize, Vec<String>)> {
    let (start, token, switch_form) = if let Some(rest) = prefix.strip_prefix("/checkout ") {
        ("/checkout ".len(), rest, false)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "git checkout ") {
        (prefix.len() - rest.len(), rest, false)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "checkout ") {
        (prefix.len() - rest.len(), rest, false)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "switch to ") {
        (prefix.len() - rest.len(), rest, true)
    } else {
        return None;
    };

    let mut candidates: Vec<String> = discover_git_root(workspace)
        .map(|root| {
            let mut refs = git_branch_names(&root);
            if switch_form {
                refs.extend(git_tag_names(&root));
                refs.sort();
                refs.dedup();
            }
            refs
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.starts_with(token))
        .collect();

    if !switch_form {
        for file in file_completion_candidates(token, workspace) {
            if !candidates.contains(&file) {
                candidates.push(file);
            }
        }
    }

    Some((start, candidates))
}

fn add_file_completion_candidates(prefix: &str, workspace: &Path) -> Option<(usize, Vec<String>)> {
    let (start, token) = if let Some(rest) = prefix.strip_prefix("/add_file ") {
        ("/add_file ".len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "git add ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "add file ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "add ") {
        (prefix.len() - rest.len(), rest)
    } else {
        return None;
    };

    let candidates = discover_git_root(workspace)
        .map(|root| git_untracked_candidates(&root, token))
        .unwrap_or_default();

    Some((start, candidates))
}

fn git_untracked_candidates(repo_root: &Path, token: &str) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--others", "--exclude-standard", "--directory"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() || !line.starts_with(token) {
            continue;
        }
        if line.ends_with('/') {
            dirs.push(line.to_string());
        } else {
            files.push(line.to_string());
        }
    }
    dirs.sort();
    files.sort();
    dirs.extend(files);
    dirs
}

fn remove_file_completion_candidates(
    prefix: &str,
    workspace: &Path,
) -> Option<(usize, Vec<String>)> {
    let (start, token) = if let Some(rest) = prefix.strip_prefix("/remove_file ") {
        ("/remove_file ".len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "git rm ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "remove file ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "remove ") {
        (prefix.len() - rest.len(), rest)
    } else {
        return None;
    };

    let candidates = discover_git_root(workspace)
        .map(|root| git_tracked_candidates(&root, token))
        .unwrap_or_default();

    Some((start, candidates))
}

fn git_tracked_candidates(repo_root: &Path, token: &str) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut dirs = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() || !line.starts_with(token) {
            continue;
        }
        let rest = &line[token.len()..];
        if let Some(slash) = rest.find('/') {
            dirs.insert(format!("{}{}/", token, &rest[..slash]));
        } else {
            files.push(line.to_string());
        }
    }
    let mut result: Vec<String> = dirs.into_iter().collect();
    files.sort();
    result.extend(files);
    result
}

fn move_file_completion_candidates(prefix: &str, workspace: &Path) -> Option<(usize, Vec<String>)> {
    let (cmd_len, args) = if let Some(rest) = prefix.strip_prefix("/move_file ") {
        ("/move_file ".len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "git mv ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "move file ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "move ") {
        (prefix.len() - rest.len(), rest)
    } else {
        return None;
    };

    let (token_start, token) = last_shell_token(args);
    let previous = args[..token_start].trim_end();
    let previous_count = if previous.is_empty() {
        0
    } else {
        shell_words(previous).unwrap_or_default().len()
    };

    let absolute_start = cmd_len + token_start;
    let candidates = if previous_count == 0 {
        discover_git_root(workspace)
            .map(|root| git_tracked_candidates(&root, token))
            .unwrap_or_default()
    } else {
        file_completion_candidates(token, workspace)
    };

    Some((absolute_start, candidates))
}

fn merge_completion_prefix(prefix: &str) -> Option<(usize, &str)> {
    if let Some(branch) = prefix.strip_prefix("/merge ") {
        return Some(("/merge ".len(), branch));
    }
    for command_prefix in ["git merge ", "merge "] {
        if let Some(branch) = strip_ascii_prefix(prefix, command_prefix) {
            return Some((prefix.len() - branch.len(), branch));
        }
    }
    None
}

fn delete_branch_completion_prefix(prefix: &str) -> Option<(usize, &str)> {
    if let Some(branch) = prefix.strip_prefix("/delete ") {
        return Some(("/delete ".len(), branch));
    }
    for command_prefix in ["git branch -D ", "delete branch ", "delete "] {
        if let Some(branch) = strip_ascii_prefix(prefix, command_prefix) {
            return Some((prefix.len() - branch.len(), branch));
        }
    }
    None
}

fn git_branch_names(repo_root: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["branch", "--all", "--format=%(refname:short)"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut branches: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "HEAD" && !l.ends_with("/HEAD"))
        .map(str::to_string)
        .collect();
    branches.sort();
    branches.dedup();
    branches
}

fn git_tag_names(repo_root: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["tag"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut tags: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

fn git_local_branch_names(repo_root: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["branch", "--format=%(refname:short)"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut branches: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    branches.sort();
    branches.dedup();
    branches
}

fn cherry_pick_completion_candidates(
    prefix: &str,
    workspace: &Path,
) -> Option<(usize, Vec<String>)> {
    let (cmd_len, token) = if let Some(rest) = prefix.strip_prefix("/cherry_pick ") {
        ("/cherry_pick ".len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "git cherry-pick ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "cherry-pick ") {
        (prefix.len() - rest.len(), rest)
    } else if let Some(rest) = strip_ascii_prefix(prefix, "cherry pick ") {
        (prefix.len() - rest.len(), rest)
    } else {
        return None;
    };
    let token = token.trim_start();
    let candidates = discover_git_root(workspace)
        .map(|root| git_commit_hashes(&root, token))
        .unwrap_or_default();
    Some((cmd_len, candidates))
}

fn git_commit_hashes(repo_root: &Path, token: &str) -> Vec<String> {
    for branch in ["origin/main", "origin/master", "main", "master"] {
        let check = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-parse", "--verify", branch])
            .output();
        if !matches!(check, Ok(ref o) if o.status.success()) {
            continue;
        }
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["log", "--abbrev-commit", "--format=%h", branch])
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let hashes: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|h| !h.is_empty() && h.starts_with(token))
            .take(50)
            .map(str::to_string)
            .collect();
        if !hashes.is_empty() || token.is_empty() {
            return hashes;
        }
    }
    Vec::new()
}

fn open_file_completion_candidates(token: &str, workspace: &Path) -> Vec<String> {
    let (quoted, token) = match token.chars().next() {
        Some(quote @ '"') | Some(quote @ '\'') => (Some(quote), &token[quote.len_utf8()..]),
        _ => (None, token),
    };
    let gitignore = workspace_gitignore(workspace);

    let mut matches = WalkDir::new(workspace)
        .into_iter()
        .filter_entry(|entry| {
            should_include_completion_path(
                workspace,
                entry.path(),
                entry.file_type().is_dir(),
                gitignore.as_ref(),
            )
        })
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(workspace).ok()?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            let file_name = entry.file_name().to_string_lossy();
            if !open_file_completion_matches(&relative, &file_name, token) {
                return None;
            }

            Some(match quoted {
                Some(quote) => format!("{quote}{relative}"),
                None => relative,
            })
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
}

fn open_file_completion_matches(relative: &str, file_name: &str, token: &str) -> bool {
    token.is_empty()
        || relative.starts_with(token)
        || (!token.contains('/') && file_name.starts_with(token))
}

fn workspace_gitignore(workspace: &Path) -> Option<Gitignore> {
    let ignore_root = discover_git_root(workspace).unwrap_or_else(|| workspace.to_path_buf());
    let mut builder = GitignoreBuilder::new(&ignore_root);
    let root_gitignore_path = ignore_root.join(".gitignore");
    if root_gitignore_path.is_file() {
        builder.add(root_gitignore_path);
    }
    let workspace_gitignore_path = workspace.join(".gitignore");
    if workspace != ignore_root && workspace_gitignore_path.is_file() {
        builder.add(workspace_gitignore_path);
    }
    builder.build().ok()
}

fn should_include_completion_path(
    workspace: &Path,
    path: &Path,
    is_dir: bool,
    gitignore: Option<&Gitignore>,
) -> bool {
    let Ok(relative) = path.strip_prefix(workspace) else {
        return false;
    };

    if gitignore.is_some_and(|matcher| {
        matcher
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
    }) {
        return false;
    }

    if relative.as_os_str().is_empty() {
        return true;
    }

    let relative = relative.to_string_lossy().replace('\\', "/");
    !(relative == ".git"
        || relative.starts_with(".git/")
        || relative == "build"
        || relative.starts_with("build/")
        || relative == "target"
        || relative.starts_with("target/"))
}

fn previous_boundary(input: &str, cursor: usize) -> Option<usize> {
    input[..cursor.min(input.len())]
        .char_indices()
        .last()
        .map(|(index, _)| index)
}

fn next_boundary(input: &str, cursor: usize) -> Option<usize> {
    let cursor = cursor.min(input.len());
    input[cursor..]
        .char_indices()
        .nth(1)
        .map(|(index, _)| cursor + index)
        .or_else(|| (cursor < input.len()).then_some(input.len()))
}

// Underscore is not a word char: Alt+Backspace/Alt+D stop at `_` boundaries in identifiers and paths.
fn is_readline_word_char(ch: char) -> bool {
    ch.is_alphanumeric()
}

fn readline_word_start(buffer: &str, cursor: usize) -> usize {
    let mut pos = cursor;
    while let Some(prev) = previous_boundary(buffer, pos) {
        if buffer[prev..pos]
            .chars()
            .all(|ch| !is_readline_word_char(ch))
        {
            pos = prev;
        } else {
            break;
        }
    }
    while let Some(prev) = previous_boundary(buffer, pos) {
        if buffer[prev..pos].chars().all(is_readline_word_char) {
            pos = prev;
        } else {
            break;
        }
    }
    pos
}

fn readline_word_end(buffer: &str, cursor: usize) -> usize {
    let mut pos = cursor;
    while let Some(next) = next_boundary(buffer, pos) {
        if buffer[pos..next]
            .chars()
            .all(|ch| !is_readline_word_char(ch))
        {
            pos = next;
        } else {
            break;
        }
    }
    while let Some(next) = next_boundary(buffer, pos) {
        if buffer[pos..next].chars().all(is_readline_word_char) {
            pos = next;
        } else {
            break;
        }
    }
    pos
}

enum CommandOutcome {
    Unhandled,
    Quiet,
    Output(String),
    Cleared,
    Quit,
}

enum LocalCommand<'a> {
    Help,
    ConnectDefault,
    ConnectTo(&'a str),
    Disconnect,
    Reload,
    ListModels,
    ListFiles,
    ShowFile(Cow<'a, str>),
    Tools,
    ModelInfo,
    SetModel(&'a str),
    Diff,
    Status,
    Log,
    Pull(Option<u64>),
    Rebase,
    Merge(Option<Cow<'a, str>>),
    Checkout(Option<Cow<'a, str>>),
    AddFile(Option<Cow<'a, str>>),
    RemoveFile(Option<Cow<'a, str>>),
    MoveFile(Option<(Cow<'a, str>, Cow<'a, str>)>),
    CherryPick(Option<Cow<'a, str>>),
    Commit(Option<Cow<'a, str>>),
    Push(bool),
    InitRepo,
    DeleteBranch(Option<Cow<'a, str>>),
    OpenFile(&'a str),
    Clear,
    Quit,
}

struct CommandContext<'a> {
    startup_model: &'a str,
    startup_endpoint: &'a str,
    llms: &'a HashMap<String, LlmConfiguration>,
    tools: &'a ToolExecutor,
    workspace: &'a Path,
}

struct CommandState<'a> {
    active_model: &'a mut String,
    current_endpoint: &'a mut Option<String>,
    session: &'a mut ChatSession,
}

fn handle_command(
    input: &str,
    state: CommandState<'_>,
    context: CommandContext<'_>,
) -> Result<CommandOutcome> {
    let Some(command) = parse_local_command(input) else {
        if input.trim_start().starts_with('/') {
            return Ok(CommandOutcome::Output(format!(
                "Unknown command '{}'. Use /help to see available commands.",
                input.trim()
            )));
        }
        return Ok(CommandOutcome::Unhandled);
    };

    let CommandState {
        active_model,
        current_endpoint,
        session,
    } = state;
    let CommandContext {
        startup_model,
        startup_endpoint,
        llms,
        tools,
        workspace,
    } = context;

    match command {
        LocalCommand::Help => Ok(CommandOutcome::Output(help_text().to_string())),
        LocalCommand::ConnectDefault => {
            let endpoint = llms
                .get(active_model)
                .ok_or_else(|| anyhow!("unknown model profile '{active_model}'"))?
                .endpoint
                .clone();
            *current_endpoint = Some(endpoint);
            Ok(CommandOutcome::Quiet)
        }
        LocalCommand::ConnectTo(endpoint) => {
            if endpoint.is_empty() {
                return Ok(CommandOutcome::Output(connect_usage_message().to_string()));
            }
            *current_endpoint = Some(endpoint.to_string());
            Ok(CommandOutcome::Quiet)
        }
        LocalCommand::Disconnect => Ok({
            *current_endpoint = None;
            CommandOutcome::Quiet
        }),
        LocalCommand::Reload => {
            *active_model = startup_model.to_string();
            *current_endpoint = Some(startup_endpoint.to_string());
            let prompt = system_prompt(
                llms.get(startup_model)
                    .ok_or_else(|| anyhow!("unknown model profile '{startup_model}'"))?,
            );
            session.clear(prompt);
            Ok(CommandOutcome::Quiet)
        }
        LocalCommand::ListModels => Ok(CommandOutcome::Output(format_models(llms))),
        LocalCommand::ListFiles => match list_workspace_files_tree(workspace) {
            Ok(output) => Ok(CommandOutcome::Output(output)),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::ShowFile(args) => match show_file_output(workspace, args.as_ref()) {
            Ok(output) => Ok(CommandOutcome::Output(output)),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Tools => Ok(CommandOutcome::Output(format_tools(tools))),
        LocalCommand::ModelInfo => Ok(CommandOutcome::Output(
            "Use /list_models to see configured profiles".to_string(),
        )),
        LocalCommand::SetModel(name) => {
            if name.is_empty() {
                return Ok(CommandOutcome::Output(model_usage_message().to_string()));
            }
            if !llms.contains_key(name) {
                return Ok(CommandOutcome::Output(format!(
                    "Unknown model profile '{name}'. Available: {}",
                    sorted_model_names(llms).join(", ")
                )));
            }
            let profile = &llms[name];
            let endpoint = normalized_openai_endpoint(&profile.endpoint);
            *active_model = name.to_string();
            *current_endpoint = Some(endpoint);
            session.set_system_prompt(system_prompt(profile));
            Ok(CommandOutcome::Quiet)
        }
        LocalCommand::Diff => match git_workspace_diff(workspace) {
            Ok(output) => Ok(CommandOutcome::Output(output)),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Status => match status_output(workspace) {
            Ok(output) => Ok(CommandOutcome::Output(output)),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Log => match log_output(workspace) {
            Ok(output) => Ok(CommandOutcome::Output(output)),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Pull(None) => Ok(CommandOutcome::Output(pull_usage_message().to_string())),
        LocalCommand::Pull(Some(pr_number)) => match pull_request_output(workspace, pr_number) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Rebase => match rebase_output(workspace) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Merge(None) => Ok(CommandOutcome::Output(merge_usage_message().to_string())),
        LocalCommand::Merge(Some(branch)) => match merge_output(workspace, &branch) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Checkout(None) => {
            Ok(CommandOutcome::Output(checkout_usage_message().to_string()))
        }
        LocalCommand::Checkout(Some(target)) => match checkout_output(workspace, &target) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::AddFile(None) => {
            Ok(CommandOutcome::Output(add_file_usage_message().to_string()))
        }
        LocalCommand::AddFile(Some(path)) => match add_file_output(workspace, &path) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::RemoveFile(None) => Ok(CommandOutcome::Output(
            remove_file_usage_message().to_string(),
        )),
        LocalCommand::RemoveFile(Some(path)) => match remove_file_output(workspace, &path) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::MoveFile(None) => Ok(CommandOutcome::Output(
            move_file_usage_message().to_string(),
        )),
        LocalCommand::MoveFile(Some((src, dst))) => match move_file_output(workspace, &src, &dst) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::CherryPick(None) => Ok(CommandOutcome::Output(
            cherry_pick_usage_message().to_string(),
        )),
        LocalCommand::CherryPick(Some(commit)) => match cherry_pick_output(workspace, &commit) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Commit(None) => {
            Ok(CommandOutcome::Output(commit_usage_message().to_string()))
        }
        LocalCommand::Commit(Some(message)) => match commit_output(workspace, &message) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::Push(force) => match push_output(workspace, force) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::InitRepo => match init_repo_output(workspace) {
            Ok(_) => Ok(CommandOutcome::Quiet),
            Err(err) => Ok(local_command_error(err)),
        },
        LocalCommand::DeleteBranch(None) => Ok(CommandOutcome::Output(
            delete_branch_usage_message().to_string(),
        )),
        LocalCommand::DeleteBranch(Some(branch)) => {
            match delete_branch_output(workspace, &branch) {
                Ok(_) => Ok(CommandOutcome::Quiet),
                Err(err) => Ok(local_command_error(err)),
            }
        }
        LocalCommand::OpenFile(path) => {
            if path.is_empty() {
                return Ok(CommandOutcome::Output(
                    open_file_usage_message().to_string(),
                ));
            }
            match open_in_editor(workspace, path) {
                Ok(()) => Ok(CommandOutcome::Quiet),
                Err(err) => Ok(CommandOutcome::Output(format!("Error: {err:#}"))),
            }
        }
        LocalCommand::Clear => {
            let prompt = system_prompt(
                llms.get(active_model)
                    .ok_or_else(|| anyhow!("unknown model profile '{active_model}'"))?,
            );
            session.clear(prompt);
            Ok(CommandOutcome::Cleared)
        }
        LocalCommand::Quit => Ok(CommandOutcome::Quit),
    }
}

fn parse_local_command(input: &str) -> Option<LocalCommand<'_>> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    parse_slash_command(input).or_else(|| parse_natural_language_command(input))
}

fn parse_slash_command(input: &str) -> Option<LocalCommand<'_>> {
    match input {
        "/help" => Some(LocalCommand::Help),
        "/connect" => Some(LocalCommand::ConnectDefault),
        "/disconnect" => Some(LocalCommand::Disconnect),
        "/reload" => Some(LocalCommand::Reload),
        "/list_models" => Some(LocalCommand::ListModels),
        "/list_files" => Some(LocalCommand::ListFiles),
        "/show_file" => Some(LocalCommand::ShowFile(Cow::Borrowed(""))),
        "/open_file" => Some(LocalCommand::OpenFile("")),
        "/tools" => Some(LocalCommand::Tools),
        "/model" => Some(LocalCommand::ModelInfo),
        "/diff" => Some(LocalCommand::Diff),
        "/status" => Some(LocalCommand::Status),
        "/log" => Some(LocalCommand::Log),
        "/pull" => Some(LocalCommand::Pull(None)),
        "/rebase" => Some(LocalCommand::Rebase),
        "/merge" => Some(LocalCommand::Merge(None)),
        "/checkout" => Some(LocalCommand::Checkout(None)),
        "/add_file" => Some(LocalCommand::AddFile(None)),
        "/remove_file" => Some(LocalCommand::RemoveFile(None)),
        "/move_file" => Some(LocalCommand::MoveFile(None)),
        "/cherry_pick" => Some(LocalCommand::CherryPick(None)),
        "/commit" => Some(LocalCommand::Commit(None)),
        "/push" => Some(LocalCommand::Push(false)),
        "/init_repo" => Some(LocalCommand::InitRepo),
        "/delete" => Some(LocalCommand::DeleteBranch(None)),
        "/clear" => Some(LocalCommand::Clear),
        "/quit" => Some(LocalCommand::Quit),
        _ => {
            if let Some(endpoint) = input.strip_prefix("/connect ") {
                return Some(LocalCommand::ConnectTo(endpoint.trim()));
            }
            if let Some(name) = input.strip_prefix("/model ") {
                return Some(LocalCommand::SetModel(name.trim()));
            }
            if let Some(args) = input.strip_prefix("/show_file ") {
                return Some(LocalCommand::ShowFile(Cow::Borrowed(args.trim())));
            }
            if let Some(args) = input.strip_prefix("/pull ") {
                return Some(LocalCommand::Pull(args.trim().parse::<u64>().ok()));
            }
            if let Some(args) = input.strip_prefix("/merge ") {
                let branch = args.trim();
                return Some(LocalCommand::Merge(if branch.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(branch))
                }));
            }
            if let Some(args) = input.strip_prefix("/checkout ") {
                let target = args.trim();
                return Some(LocalCommand::Checkout(if target.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(target))
                }));
            }
            if let Some(args) = input.strip_prefix("/add_file ") {
                let path = args.trim();
                return Some(LocalCommand::AddFile(if path.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(path))
                }));
            }
            if let Some(args) = input.strip_prefix("/remove_file ") {
                let path = args.trim();
                return Some(LocalCommand::RemoveFile(if path.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(path))
                }));
            }
            if let Some(args) = input.strip_prefix("/move_file ") {
                let args = args.trim();
                return Some(match shell_words(args) {
                    Ok(words) if words.len() >= 2 => LocalCommand::MoveFile(Some((
                        Cow::Owned(words[0].clone()),
                        Cow::Owned(words[1].clone()),
                    ))),
                    _ => LocalCommand::MoveFile(None),
                });
            }
            if let Some(args) = input.strip_prefix("/cherry_pick ") {
                let commit = args.trim();
                return Some(LocalCommand::CherryPick(if commit.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(commit))
                }));
            }
            if let Some(args) = input.strip_prefix("/commit ") {
                let message = strip_matching_quotes(args.trim());
                return Some(LocalCommand::Commit(if message.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(message))
                }));
            }
            if let Some(flag) = input.strip_prefix("/push ") {
                let flag = flag.trim();
                if flag == "--force" || flag == "-f" || flag.eq_ignore_ascii_case("force") {
                    return Some(LocalCommand::Push(true));
                }
            }
            if let Some(args) = input.strip_prefix("/delete ") {
                let branch = args.trim();
                return Some(LocalCommand::DeleteBranch(if branch.is_empty() {
                    None
                } else {
                    Some(Cow::Borrowed(branch))
                }));
            }
            if let Some(args) = input.strip_prefix("/open_file ")
                && args.trim().is_empty()
            {
                return Some(LocalCommand::OpenFile(""));
            }
            parse_open_file_target(input, "/open_file ").map(LocalCommand::OpenFile)
        }
    }
}

fn parse_natural_language_command(input: &str) -> Option<LocalCommand<'_>> {
    if matches_ci(
        input,
        &[
            "help",
            "show help",
            "show commands",
            "show available commands",
        ],
    ) {
        return Some(LocalCommand::Help);
    }
    if matches_ci(input, &["connect", "reconnect"]) {
        return Some(LocalCommand::ConnectDefault);
    }
    if let Some(endpoint) = strip_ascii_prefix(input, "connect to ") {
        return Some(LocalCommand::ConnectTo(endpoint.trim()));
    }
    if matches_ci(input, &["disconnect"]) {
        return Some(LocalCommand::Disconnect);
    }
    if matches_ci(input, &["reload", "reload configuration", "reset session"]) {
        return Some(LocalCommand::Reload);
    }
    if matches_ci(
        input,
        &[
            "list models",
            "show models",
            "show available models",
            "models",
        ],
    ) {
        return Some(LocalCommand::ListModels);
    }
    if matches_ci(
        input,
        &[
            "list files",
            "show files",
            "show workspace files",
            "list workspace files",
        ],
    ) {
        return Some(LocalCommand::ListFiles);
    }
    if matches_ci(
        input,
        &["show tools", "list tools", "show local tools", "tools"],
    ) {
        return Some(LocalCommand::Tools);
    }
    if matches_ci(
        input,
        &[
            "show model",
            "current model",
            "what model am i using",
            "model",
        ],
    ) {
        return Some(LocalCommand::ModelInfo);
    }
    if matches_ci(input, &["diff", "show diff", "git diff"]) {
        return Some(LocalCommand::Diff);
    }
    if matches_ci(input, &["status", "show status", "git status"]) {
        return Some(LocalCommand::Status);
    }
    if matches_ci(input, &["log", "show log", "git log", "git lg"]) {
        return Some(LocalCommand::Log);
    }
    for prefix in [
        "use model ",
        "switch model to ",
        "set model to ",
        "select model ",
    ] {
        if let Some(name) = strip_ascii_prefix(input, prefix) {
            return Some(LocalCommand::SetModel(name.trim()));
        }
    }
    if let Some(path) = parse_open_file_target(input, "/open_file ") {
        return Some(LocalCommand::OpenFile(path));
    }
    for prefix in ["open file ", "open ", "edit file ", "edit "] {
        if let Some(path) = parse_open_file_target(input, prefix) {
            return Some(LocalCommand::OpenFile(path));
        }
    }
    if let Some(args) = parse_show_file_natural_language_args(input) {
        return Some(LocalCommand::ShowFile(args));
    }
    if let Some(pr_number) = parse_pull_pr_number(input) {
        return Some(LocalCommand::Pull(Some(pr_number)));
    }
    if matches_ci(input, &["rebase", "git rebase"]) {
        return Some(LocalCommand::Rebase);
    }
    for prefix in ["git merge ", "merge "] {
        if let Some(branch) = strip_ascii_prefix(input, prefix) {
            let branch = branch.trim();
            if !branch.is_empty() {
                return Some(LocalCommand::Merge(Some(Cow::Borrowed(branch))));
            }
        }
    }
    if matches_ci(input, &["merge"]) {
        return Some(LocalCommand::Merge(None));
    }
    for prefix in ["git checkout ", "checkout "] {
        if let Some(target) = strip_ascii_prefix(input, prefix) {
            let target = target.trim();
            if !target.is_empty() {
                return Some(LocalCommand::Checkout(Some(Cow::Borrowed(target))));
            }
        }
    }
    if let Some(target) = strip_ascii_prefix(input, "switch to ") {
        let target = strip_ascii_suffix(target.trim(), " branch")
            .map(str::trim)
            .unwrap_or(target.trim());
        if !target.is_empty() {
            return Some(LocalCommand::Checkout(Some(Cow::Borrowed(target))));
        }
    }
    if matches_ci(input, &["checkout", "switch to"]) {
        return Some(LocalCommand::Checkout(None));
    }
    for prefix in ["git add ", "add file ", "add "] {
        if let Some(path) = strip_ascii_prefix(input, prefix) {
            let path = path.trim();
            if !path.is_empty() {
                return Some(LocalCommand::AddFile(Some(Cow::Borrowed(path))));
            }
        }
    }
    if matches_ci(input, &["add"]) {
        return Some(LocalCommand::AddFile(None));
    }
    for prefix in ["git rm ", "remove file ", "remove "] {
        if let Some(path) = strip_ascii_prefix(input, prefix) {
            let path = path.trim();
            if !path.is_empty() {
                return Some(LocalCommand::RemoveFile(Some(Cow::Borrowed(path))));
            }
        }
    }
    if matches_ci(input, &["remove"]) {
        return Some(LocalCommand::RemoveFile(None));
    }
    for prefix in ["git mv ", "move file ", "move "] {
        if let Some(rest) = strip_ascii_prefix(input, prefix) {
            let rest = rest.trim();
            if let Ok(words) = shell_words(rest)
                && words.len() >= 2
            {
                return Some(LocalCommand::MoveFile(Some((
                    Cow::Owned(words[0].clone()),
                    Cow::Owned(words[1].clone()),
                ))));
            }
        }
    }
    if matches_ci(input, &["move"]) {
        return Some(LocalCommand::MoveFile(None));
    }
    for prefix in ["git cherry-pick ", "cherry-pick ", "cherry pick "] {
        if let Some(commit) = strip_ascii_prefix(input, prefix) {
            let commit = commit.trim();
            if !commit.is_empty() {
                return Some(LocalCommand::CherryPick(Some(Cow::Borrowed(commit))));
            }
        }
    }
    if matches_ci(input, &["cherry pick", "cherry-pick"]) {
        return Some(LocalCommand::CherryPick(None));
    }
    for prefix in ["git commit -a -m ", "git commit -m ", "commit "] {
        if let Some(msg) = strip_ascii_prefix(input, prefix) {
            let msg = strip_matching_quotes(msg.trim());
            if !msg.is_empty() {
                return Some(LocalCommand::Commit(Some(Cow::Borrowed(msg))));
            }
        }
    }
    if matches_ci(input, &["commit"]) {
        return Some(LocalCommand::Commit(None));
    }
    if matches_ci(
        input,
        &[
            "force push",
            "push force",
            "push --force",
            "push -f",
            "git push --force",
            "git push -f",
            "git push origin --force",
            "git push origin -f",
        ],
    ) {
        return Some(LocalCommand::Push(true));
    }
    if matches_ci(input, &["push", "git push", "git push origin"]) {
        return Some(LocalCommand::Push(false));
    }
    if matches_ci(input, &["init", "init repo", "git init"]) {
        return Some(LocalCommand::InitRepo);
    }
    if matches_ci(input, &["delete", "delete branch"]) {
        return Some(LocalCommand::DeleteBranch(None));
    }
    for prefix in ["git branch -D ", "delete branch ", "delete "] {
        if let Some(branch) = strip_ascii_prefix(input, prefix) {
            let branch = branch.trim();
            if !branch.is_empty() {
                return Some(LocalCommand::DeleteBranch(Some(Cow::Borrowed(branch))));
            }
        }
    }
    if matches_ci(
        input,
        &["clear", "clear conversation", "reset conversation"],
    ) {
        return Some(LocalCommand::Clear);
    }
    if matches_ci(input, &["quit", "exit"]) {
        return Some(LocalCommand::Quit);
    }

    None
}

fn parse_open_file_target<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let path = strip_ascii_prefix(input, prefix)?.trim();
    if path.is_empty() {
        return None;
    }
    Some(strip_matching_quotes(path))
}

fn parse_show_file_natural_language_args(input: &str) -> Option<Cow<'_, str>> {
    parse_show_file_natural_language_args_with_prefix(input, "show file ", false)
        .or_else(|| parse_show_file_natural_language_args_with_prefix(input, "show ", true))
}

fn parse_show_file_natural_language_args_with_prefix<'a>(
    input: &'a str,
    prefix: &str,
    single_token_only: bool,
) -> Option<Cow<'a, str>> {
    let raw = strip_ascii_prefix(input, prefix)?.trim();
    let (path, options) = parse_show_file_natural_language_target(raw, single_token_only)?;
    if !options.show_hash && !options.show_author {
        return Some(Cow::Borrowed(path));
    }

    let mut args = String::new();
    if options.show_hash {
        args.push_str("--hash ");
    }
    if options.show_author {
        args.push_str("--author ");
    }
    args.push_str(&quote_shell_argument(path));
    Some(Cow::Owned(args))
}

fn parse_show_file_natural_language_target(
    raw: &str,
    single_token_only: bool,
) -> Option<(&str, ShowFileOptions)> {
    for (suffix, options) in [
        (
            " with hash and author",
            ShowFileOptions {
                show_hash: true,
                show_author: true,
            },
        ),
        (
            " with author and hash",
            ShowFileOptions {
                show_hash: true,
                show_author: true,
            },
        ),
        (
            " with hash",
            ShowFileOptions {
                show_hash: true,
                show_author: false,
            },
        ),
        (
            " with author",
            ShowFileOptions {
                show_hash: false,
                show_author: true,
            },
        ),
    ] {
        if let Some(path) = strip_ascii_suffix(raw, suffix) {
            let path = parse_show_file_target(path.trim(), single_token_only)?;
            return Some((path, options));
        }
    }

    parse_show_file_target(raw, single_token_only).map(|path| (path, ShowFileOptions::default()))
}

fn parse_show_file_target(path: &str, single_token_only: bool) -> Option<&str> {
    if path.is_empty() {
        return None;
    }
    let quoted = matches!(path.chars().next(), Some('"') | Some('\''));
    if single_token_only && !quoted && path.chars().any(char::is_whitespace) {
        return None;
    }
    Some(strip_matching_quotes(path))
}

fn strip_ascii_suffix<'a>(input: &'a str, suffix: &str) -> Option<&'a str> {
    if input.len() >= suffix.len()
        && input[input.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    {
        Some(&input[..input.len() - suffix.len()])
    } else {
        None
    }
}

fn quote_shell_argument(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\' | '$' | '`'))
    {
        return argument.to_string();
    }

    let mut quoted = String::from("\"");
    for ch in argument.chars() {
        match ch {
            '"' | '\\' | '$' | '`' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn strip_ascii_prefix<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    if input.len() >= prefix.len() && input[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&input[prefix.len()..])
    } else {
        None
    }
}

fn list_workspace_files_tree(workspace: &Path) -> Result<String> {
    let mut lines = vec![workspace.display().to_string()];
    append_workspace_tree(workspace, "", &mut lines)?;
    Ok(lines.join("\n"))
}

fn append_workspace_tree(directory: &Path, prefix: &str, lines: &mut Vec<String>) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", directory.display()))?;
    entries.retain(|entry| should_include_listed_path(&entry.file_name(), &entry.path()));
    entries.sort_by(|left, right| {
        compare_tree_entries(
            &left.file_name(),
            &left.path(),
            &right.file_name(),
            &right.path(),
        )
    });
    let total_entries = entries.len();

    for (index, entry) in entries.into_iter().enumerate() {
        let path = entry.path();
        let is_dir = path.is_dir();
        let name = entry.file_name().to_string_lossy().to_string();
        let branch = if index + 1 == total_entries {
            "└── "
        } else {
            "├── "
        };
        lines.push(format!("{prefix}{branch}{name}"));
        if is_dir {
            let next_prefix = if index + 1 == total_entries {
                format!("{prefix}    ")
            } else {
                format!("{prefix}│   ")
            };
            append_workspace_tree(&path, &next_prefix, lines)?;
        }
    }

    Ok(())
}

fn should_include_listed_path(file_name: &std::ffi::OsStr, path: &Path) -> bool {
    !(path.is_dir() && matches!(file_name.to_str(), Some(".git" | "build" | "target")))
}

fn compare_tree_entries(
    left_name: &std::ffi::OsStr,
    left_path: &Path,
    right_name: &std::ffi::OsStr,
    right_path: &Path,
) -> std::cmp::Ordering {
    left_path
        .is_file()
        .cmp(&right_path.is_file())
        .then_with(|| {
            left_name
                .to_string_lossy()
                .to_lowercase()
                .cmp(&right_name.to_string_lossy().to_lowercase())
        })
}

fn matches_ci(input: &str, options: &[&str]) -> bool {
    options
        .iter()
        .any(|option| input.eq_ignore_ascii_case(option))
}

fn strip_matching_quotes(input: &str) -> &str {
    if input.len() >= 2 {
        let bytes = input.as_bytes();
        let first = bytes[0];
        let last = bytes[input.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &input[1..input.len() - 1];
        }
    }
    input
}

fn open_in_editor(workspace: &Path, raw_path: &str) -> Result<()> {
    let editor = std::env::var("EDITOR").context("EDITOR is not set")?;
    let editor_parts = shell_words(&editor)?;
    let path = resolve_workspace_path(workspace, raw_path)?;
    let (program, args) = editor_parts
        .split_first()
        .ok_or_else(|| anyhow!("EDITOR is empty"))?;

    let _raw_mode_pause_guard = RawModePauseGuard::new()?;
    let _child = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch editor '{}'", editor))?;

    Ok(())
}

fn git_workspace_diff(workspace: &Path) -> Result<String> {
    let Some(repo_root) = discover_git_root(workspace) else {
        return workspace_is_not_git(workspace);
    };
    let terminal_width = current_terminal_width();
    let workspace_pathspec = workspace
        .strip_prefix(&repo_root)
        .ok()
        .filter(|path| !path.as_os_str().is_empty());

    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(&repo_root)
        .arg("diff")
        .arg("--color=always");
    command.env("COLUMNS", terminal_width.to_string());
    if let Some(pathspec) = workspace_pathspec {
        command.arg("--").arg(pathspec);
    }

    let output = command.output().context("failed to run git diff")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git diff failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }

    let diff = if let Some(pager_command) = configured_git_diff_pager(&repo_root)? {
        run_git_diff_pager(&repo_root, &pager_command, &output.stdout, terminal_width)?
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    if diff.trim().is_empty() {
        Ok("No changes against the current branch.".to_string())
    } else {
        Ok(diff)
    }
}

fn status_output(workspace: &Path) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("status is only available inside a Git repository"))?;
    if let Some(output) = try_gh_status(&repo_root)? {
        return Ok(output);
    }
    git_status(&repo_root)
}

fn try_gh_status(_repo_root: &Path) -> Result<Option<String>> {
    Ok(None)
}

fn git_status(repo_root: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--branch", "--short"])
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git status failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let colored = colorize_git_status(&raw);
    if colored.trim().is_empty() {
        Ok("Nothing to commit, working tree clean.".to_string())
    } else {
        Ok(colored)
    }
}

fn colorize_git_status(raw: &str) -> String {
    let mut result = String::new();
    for line in raw.lines() {
        if line.starts_with("## ") {
            result.push_str(ANSI_FG_SUBTLE);
            result.push_str(line);
            result.push_str(ANSI_FG_RESET);
        } else if line.len() >= 2 {
            let x = line.as_bytes()[0] as char;
            let y = line.as_bytes()[1] as char;
            let color = status_entry_color(x, y);
            result.push_str(color);
            result.push_str(line);
            if !color.is_empty() {
                result.push_str(ANSI_FG_RESET);
            }
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    result.trim_end_matches('\n').to_string()
}

fn status_entry_color(x: char, y: char) -> &'static str {
    if x == 'D' || y == 'D' {
        return ANSI_FG_LIGHT_RED;
    }
    if x == 'A' || x == '?' {
        return ANSI_FG_LIGHT_GREEN;
    }
    ""
}

fn log_output(workspace: &Path) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("log is only available inside a Git repository"))?;
    if let Some(output) = try_gh_log(&repo_root)? {
        return Ok(output);
    }
    git_log(&repo_root)
}

fn try_gh_log(_repo_root: &Path) -> Result<Option<String>> {
    Ok(None)
}

fn git_log(repo_root: &Path) -> Result<String> {
    let has_lg = std::process::Command::new("git")
        .args(["config", "--global", "--get", "alias.lg"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo_root);
    command.args(["-c", "color.ui=always"]);
    if has_lg {
        command.arg("lg");
    } else {
        command.args([
            "log",
            "--color=always",
            "--graph",
            "--oneline",
            "--decorate",
        ]);
    }

    let output = command.output().context("failed to run git log")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git log failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let log = String::from_utf8_lossy(&output.stdout).to_string();
    if log.trim().is_empty() {
        Ok("No commits yet.".to_string())
    } else {
        Ok(log)
    }
}

fn configured_git_diff_pager(repo_root: &Path) -> Result<Option<String>> {
    for key in ["pager.diff", "core.pager"] {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", "--get", key])
            .output()
            .with_context(|| format!("failed to read git config key {key}"))?;
        if !output.status.success() {
            continue;
        }
        let value = String::from_utf8(output.stdout)
            .with_context(|| format!("git config key {key} was not valid UTF-8"))?;
        let value = value.trim();
        if value.is_empty() || looks_like_interactive_pager(value) {
            continue;
        }
        return Ok(Some(value.to_string()));
    }

    Ok(None)
}

fn looks_like_interactive_pager(command: &str) -> bool {
    let first = shell_words(command)
        .ok()
        .and_then(|parts| parts.into_iter().next())
        .unwrap_or_else(|| command.trim().to_string());
    let first = Path::new(&first)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(first.as_str());
    matches!(first, "less" | "more" | "most" | "lv")
}

fn with_explicit_pager_width(command: &str, terminal_width: usize) -> String {
    let Ok(parts) = shell_words(command) else {
        return command.to_string();
    };
    let Some(first) = parts.first() else {
        return command.to_string();
    };
    let executable = Path::new(first)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(first.as_str());
    if executable != "delta"
        || parts
            .iter()
            .any(|part| part == "--width" || part.starts_with("--width="))
    {
        return command.to_string();
    }

    format!("{command} --width={terminal_width}")
}

fn run_git_diff_pager(
    repo_root: &Path,
    pager_command: &str,
    diff: &[u8],
    terminal_width: usize,
) -> Result<String> {
    let pager_command = with_explicit_pager_width(pager_command, terminal_width);
    let mut pager = std::process::Command::new("sh")
        .arg("-lc")
        .arg(&pager_command)
        .current_dir(repo_root)
        .env("COLUMNS", terminal_width.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to launch configured git pager '{pager_command}'"))?;

    if let Some(mut stdin) = pager.stdin.take() {
        stdin
            .write_all(diff)
            .with_context(|| format!("failed to write diff to git pager '{pager_command}'"))?;
    }

    let output = pager
        .wait_with_output()
        .with_context(|| format!("failed to read output from git pager '{pager_command}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git pager failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }

    String::from_utf8(output.stdout).context("git pager output was not UTF-8")
}

fn workspace_is_not_git(_workspace: &Path) -> Result<String> {
    Err(anyhow!("diff is only available inside a Git repository"))
}

fn parse_pull_pr_number(input: &str) -> Option<u64> {
    for prefix in [
        "pull pull request ",
        "pull request ",
        "pull pr ",
        "pull #",
        "pull ",
    ] {
        if let Some(rest) = strip_ascii_prefix(input, prefix)
            && let Ok(num) = rest.trim().parse::<u64>()
        {
            return Some(num);
        }
    }
    None
}

fn pull_request_output(workspace: &Path, pr_number: u64) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("pull is only available inside a Git repository"))?;
    if let Some(output) = try_gh_pr_checkout(&repo_root, pr_number)? {
        return Ok(output);
    }
    git_pr_checkout(&repo_root, pr_number)
}

fn try_gh_pr_checkout(repo_root: &Path, pr_number: u64) -> Result<Option<String>> {
    let output = match std::process::Command::new("gh")
        .args(["pr", "checkout", &pr_number.to_string()])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("failed to run gh"),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "gh pr checkout failed{}",
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

fn git_pr_checkout(repo_root: &Path, pr_number: u64) -> Result<String> {
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

fn rebase_output(workspace: &Path) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("rebase is only available inside a Git repository"))?;
    if let Some(output) = try_gh_rebase(&repo_root)? {
        return Ok(output);
    }
    git_rebase_main(&repo_root)
}

fn try_gh_rebase(repo_root: &Path) -> Result<Option<String>> {
    let branch_output = match std::process::Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("failed to run gh"),
    };
    if !branch_output.status.success() {
        return Ok(None);
    }
    let default_branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    if default_branch.is_empty() {
        return Ok(None);
    }
    git_rebase_onto(repo_root, &default_branch).map(Some)
}

fn git_rebase_main(repo_root: &Path) -> Result<String> {
    for branch in ["main", "master"] {
        let check = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["ls-remote", "--heads", "origin", branch])
            .output()
            .context("failed to run git ls-remote")?;
        if check.status.success() && !check.stdout.is_empty() {
            return git_rebase_onto(repo_root, branch);
        }
    }
    Err(anyhow!(
        "could not determine the default branch (tried main and master)"
    ))
}

fn git_rebase_onto(repo_root: &Path, branch: &str) -> Result<String> {
    let fetch = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["fetch", "origin", branch])
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
    let rebase = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rebase", &format!("origin/{branch}")])
        .output()
        .context("failed to run git rebase")?;
    if !rebase.status.success() {
        let stderr = String::from_utf8_lossy(&rebase.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&rebase.stdout).trim().to_string();
        let detail = [stdout, stderr]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git rebase failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&rebase.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Rebased onto origin/{branch}")
    } else {
        stdout
    })
}

fn merge_output(workspace: &Path, branch: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("merge is only available inside a Git repository"))?;
    if let Some(output) = try_gh_merge(&repo_root, branch)? {
        return Ok(output);
    }
    git_merge(&repo_root, branch)
}

fn try_gh_merge(repo_root: &Path, branch: &str) -> Result<Option<String>> {
    let output = match std::process::Command::new("gh")
        .args(["pr", "merge", "--merge", branch])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("failed to run gh"),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "gh pr merge failed{}",
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
        format!("Merged branch '{branch}'")
    } else {
        combined
    }))
}

fn git_merge(repo_root: &Path, branch: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge", branch])
        .output()
        .context("failed to run git merge")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = [stdout, stderr]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git merge failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Merged '{branch}'")
    } else {
        stdout
    })
}

fn checkout_output(workspace: &Path, target: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("checkout is only available inside a Git repository"))?;
    if let Some(output) = try_gh_checkout(&repo_root, target)? {
        return Ok(output);
    }
    git_checkout(&repo_root, target)
}

fn try_gh_checkout(_repo_root: &Path, _target: &str) -> Result<Option<String>> {
    Ok(None)
}

fn git_checkout(repo_root: &Path, target: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["checkout", target])
        .output()
        .context("failed to run git checkout")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = [stdout, stderr]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git checkout failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(format!("Switched to '{target}'"))
}

fn add_file_output(workspace: &Path, path: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("add_file is only available inside a Git repository"))?;
    if let Some(output) = try_gh_add_file(&repo_root, path)? {
        return Ok(output);
    }
    git_add_file(&repo_root, path)
}

fn try_gh_add_file(_repo_root: &Path, _path: &str) -> Result<Option<String>> {
    Ok(None)
}

fn git_add_file(repo_root: &Path, path: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["add", path])
        .output()
        .context("failed to run git add")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git add failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(format!("Staged '{path}'"))
}

fn remove_file_output(workspace: &Path, path: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("remove_file is only available inside a Git repository"))?;
    if let Some(output) = try_gh_remove_file(&repo_root, path)? {
        return Ok(output);
    }
    git_remove_file(&repo_root, path)
}

fn try_gh_remove_file(_repo_root: &Path, _path: &str) -> Result<Option<String>> {
    Ok(None)
}

fn git_remove_file(repo_root: &Path, path: &str) -> Result<String> {
    let mut args = vec!["rm"];
    if repo_root.join(path.trim_end_matches('/')).is_dir() {
        args.push("-r");
    }
    args.push(path);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(&args)
        .output()
        .context("failed to run git rm")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git rm failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(format!("Removed '{path}'"))
}

fn move_file_output(workspace: &Path, source: &str, destination: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("move_file is only available inside a Git repository"))?;
    if let Some(output) = try_gh_move_file(&repo_root, source, destination)? {
        return Ok(output);
    }
    git_move_file(&repo_root, source, destination)
}

fn try_gh_move_file(
    _repo_root: &Path,
    _source: &str,
    _destination: &str,
) -> Result<Option<String>> {
    Ok(None)
}

fn git_move_file(repo_root: &Path, source: &str, destination: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["mv", source, destination])
        .output()
        .context("failed to run git mv")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git mv failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(format!("Moved '{source}' to '{destination}'"))
}

fn cherry_pick_output(workspace: &Path, commit: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("cherry_pick is only available inside a Git repository"))?;
    if let Some(output) = try_gh_cherry_pick(&repo_root, commit)? {
        return Ok(output);
    }
    git_cherry_pick(&repo_root, commit)
}

fn try_gh_cherry_pick(_repo_root: &Path, _commit: &str) -> Result<Option<String>> {
    Ok(None)
}

fn git_cherry_pick(repo_root: &Path, commit: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cherry-pick", commit])
        .output()
        .context("failed to run git cherry-pick")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = [stdout, stderr]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git cherry-pick failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Cherry-picked {commit}")
    } else {
        stdout
    })
}

fn commit_output(workspace: &Path, message: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("commit is only available inside a Git repository"))?;
    if let Some(output) = try_gh_commit(&repo_root, message)? {
        return Ok(output);
    }
    git_commit(&repo_root, message)
}

fn try_gh_commit(_repo_root: &Path, _message: &str) -> Result<Option<String>> {
    Ok(None)
}

fn git_commit(repo_root: &Path, message: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["commit", "-a", "-m", message])
        .output()
        .context("failed to run git commit")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = [stdout, stderr]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git commit failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Committed: {message}")
    } else {
        stdout
    })
}

fn push_output(workspace: &Path, force: bool) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("push is only available inside a Git repository"))?;
    if let Some(output) = try_gh_push(&repo_root, force)? {
        return Ok(output);
    }
    git_push(&repo_root, force)
}

fn try_gh_push(_repo_root: &Path, _force: bool) -> Result<Option<String>> {
    Ok(None)
}

fn git_push(repo_root: &Path, force: bool) -> Result<String> {
    let branch = git_current_branch(repo_root)?;
    if force && is_protected_branch(&branch) {
        return Err(anyhow!(
            "force push is not allowed on the '{}' branch",
            branch
        ));
    }
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo_root).arg("push");
    if force {
        command.arg("-f");
    }
    command.args(["origin", &branch]);
    let output = command.output().context("failed to run git push")?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        let detail = [&stdout, &stderr]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git push failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let combined = [stdout, stderr]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(if combined.is_empty() {
        format!("Pushed '{branch}' to origin")
    } else {
        combined
    })
}

fn git_current_branch(repo_root: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["branch", "--show-current"])
        .output()
        .context("failed to run git branch")?;
    if !output.status.success() {
        return Err(anyhow!("failed to determine current branch"));
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        return Err(anyhow!(
            "could not determine current branch (detached HEAD?)"
        ));
    }
    Ok(branch)
}

fn is_protected_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master")
}

fn init_repo_output(workspace: &Path) -> Result<String> {
    if let Some(output) = try_gh_init_repo(workspace)? {
        return Ok(output);
    }
    git_init(workspace)
}

fn try_gh_init_repo(_workspace: &Path) -> Result<Option<String>> {
    Ok(None)
}

fn git_init(workspace: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("init")
        .current_dir(workspace)
        .output()
        .context("failed to run git init")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "git init failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        format!("Initialized Git repository in {}", workspace.display())
    } else {
        stdout
    })
}

fn delete_branch_usage_message() -> &'static str {
    "Usage: /delete <branch>"
}

fn delete_branch_output(workspace: &Path, branch: &str) -> Result<String> {
    let repo_root = discover_git_root(workspace)
        .ok_or_else(|| anyhow!("delete is only available inside a Git repository"))?;
    if is_protected_branch(branch) {
        return Err(anyhow!("deleting the '{}' branch is not allowed", branch));
    }
    if let Some(output) = try_gh_delete_branch(&repo_root, branch)? {
        return Ok(output);
    }
    git_delete_branch(&repo_root, branch)
}

fn try_gh_delete_branch(_repo_root: &Path, _branch: &str) -> Result<Option<String>> {
    Ok(None)
}

fn git_delete_branch(repo_root: &Path, branch: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["branch", "-D", branch])
        .output()
        .context("failed to run git branch -D")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = [stdout, stderr]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!(
            "git branch -D failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(format!("Deleted branch '{branch}'"))
}

fn current_terminal_width() -> usize {
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

fn shell_words(input: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some(active_quote) => {
                if ch == active_quote {
                    quote = None;
                } else if ch == '\\' && active_quote == '"' {
                    if let Some(escaped) = chars.next() {
                        current.push(escaped);
                    }
                } else {
                    current.push(ch);
                }
            }
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
            }
            None if ch == '\\' => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            None => current.push(ch),
        }
    }

    if quote.is_some() {
        return Err(anyhow!("EDITOR contains unterminated quotes"));
    }
    if !current.is_empty() {
        words.push(current);
    }
    if words.is_empty() {
        return Err(anyhow!("EDITOR is empty"));
    }

    Ok(words)
}

fn prepare_submitted_input(
    input: &str,
    history: &mut Vec<String>,
    history_path: &Path,
    output_state: &mut OutputState,
    pending_commands: Option<&mut VecDeque<String>>,
) -> Result<Option<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.starts_with('\\') {
        return Ok(None);
    }

    history.push(trimmed.to_string());
    append_history_entry(history_path, trimmed)?;

    if trimmed.starts_with('#') {
        output_state.push_input(&format!("> {trimmed}"));
        output_state.reset_scroll();
        return Ok(None);
    }

    if let Some(pending_commands) = pending_commands {
        pending_commands.push_back(trimmed.to_string());
        return Ok(None);
    }

    Ok(Some(trimmed.to_string()))
}

fn workspace_branch_name(workspace: &Path) -> Option<String> {
    let git_dir = discover_git_dir(workspace)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let reference = head.trim().strip_prefix("ref: ")?;
    reference.strip_prefix("refs/heads/").map(ToOwned::to_owned)
}

fn discover_git_root(workspace: &Path) -> Option<PathBuf> {
    discover_git_repository(workspace).map(|(root, _)| root)
}

fn discover_git_dir(workspace: &Path) -> Option<PathBuf> {
    discover_git_repository(workspace).map(|(_, git_dir)| git_dir)
}

fn discover_git_repository(workspace: &Path) -> Option<(PathBuf, PathBuf)> {
    for ancestor in workspace.ancestors() {
        let git_entry = ancestor.join(".git");
        if git_entry.is_dir() {
            return Some((ancestor.to_path_buf(), git_entry));
        }
        if git_entry.is_file() {
            let gitdir = fs::read_to_string(&git_entry).ok()?;
            let relative = gitdir.trim().strip_prefix("gitdir: ")?.trim();
            let path = Path::new(relative);
            let git_dir = if path.is_absolute() {
                path.to_path_buf()
            } else {
                ancestor.join(path)
            };
            return Some((ancestor.to_path_buf(), git_dir));
        }
    }
    None
}

fn system_prompt(profile: &LlmConfiguration) -> &str {
    if profile.system_prompt.is_empty() {
        "You are Orangu, a coding environment assistant connected to a local workspace. Use the available local tools to inspect files, edit files on disk, fetch external URLs for knowledge, and run shell commands when needed. Be precise, explain what you changed, and surface tool failures explicitly."
    } else {
        &profile.system_prompt
    }
}

fn sorted_model_names(llms: &HashMap<String, LlmConfiguration>) -> Vec<String> {
    let mut names = llms.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
}

fn print_screen(render: RenderContext<'_>, screen: ScreenState<'_>) {
    print!("{CLEAR_TERMINAL_SEQUENCE}");
    print!(
        "{}",
        render_screen(ScreenRenderArgs {
            version: VERSION,
            current_model: render.current_model,
            endpoint: render.endpoint,
            workspace: render.workspace,
            prompt_branch: render.prompt_branch,
            status: render.header_status,
            transcript: screen.transcript,
            scroll_offset: screen.scroll_offset,
            left_status: screen.left_status,
            pending_count: screen.pending_count,
            pending_line: screen.pending_line,
            input: screen.input,
            cursor: screen.cursor,
        })
    );
}

async fn wait_for_response(
    session: &mut ChatSession,
    user_input: &str,
    profile: &LlmConfiguration,
    tools: &ToolExecutor,
    wait_context: WaitContext<'_>,
) -> Result<WaitResult> {
    let WaitContext {
        render,
        history,
        history_path,
        model_names,
        interrupt_state,
        output_state,
        input_state,
        pending_commands,
    } = wait_context;
    let streamed_state = Arc::new(Mutex::new(StreamRenderState::default()));
    let prompt_output = Arc::clone(&streamed_state);
    let prompt_metrics = Arc::clone(&streamed_state);
    let tokenizer = cl100k_base().ok();
    let mut prompt_future = Box::pin(session.prompt(
        user_input,
        profile,
        tools,
        move |delta| {
            if let Ok(mut state) = prompt_output.lock() {
                state.output.push_str(delta);
            }
        },
        move |metrics| {
            if let Ok(mut state) = prompt_metrics.lock() {
                state.metrics.merge(metrics);
            }
        },
    ));
    let mut interval = tokio::time::interval(WAIT_LOOP_POLL_INTERVAL);
    let mut thinking_frame = 0usize;
    let thinking_started = Instant::now();
    let mut last_rendered_output = String::new();
    let mut last_rendered_metrics = StreamMetrics::default();
    let mut escape_cancel_state = EscapeCancelState::default();
    let initial_status = render_thinking_status(thinking_frame, thinking_started.elapsed());

    print_screen(
        render,
        ScreenState {
            transcript: output_state.lines(),
            scroll_offset: output_state.scroll_offset(),
            left_status: Some(initial_status),
            pending_count: pending_commands.len(),
            pending_line: None,
            input: input_state.as_str(),
            cursor: input_state.cursor(),
        },
    );
    std::io::stdout().flush()?;

    loop {
        tokio::select! {
            result = &mut prompt_future => {
                let response = result?;
                let final_state = streamed_state
                    .lock()
                    .map(|state| state.clone())
                    .unwrap_or_default();
                if let Some(pending_line) = final_pending_line(&final_state.output, &response)
                    .map(|line| render_markdown_for_console(&line))
                {
                    print_screen(
                        render,
                        ScreenState {
                            transcript: output_state.lines(),
                            scroll_offset: output_state.scroll_offset(),
                            left_status: None,
                            pending_count: pending_commands.len(),
                            pending_line: Some(pending_line.as_str()),
                            input: input_state.as_str(),
                            cursor: input_state.cursor(),
                        },
                    );
                    std::io::stdout().flush()?;
                }
                return Ok(WaitResult::Response(response));
            }
            _ = interval.tick() => {
                let elapsed = thinking_started.elapsed();
                let next_frame = (elapsed.as_millis() / THINKING_FRAME_INTERVAL.as_millis()) as usize;
                let mut redraw = next_frame != thinking_frame;
                thinking_frame = next_frame;
                let current_state = streamed_state
                    .lock()
                    .map(|state| state.clone())
                    .unwrap_or_default();
                let current_streamed_output = current_state.output;
                let current_stream_metrics = current_state.metrics;
                redraw |= current_streamed_output != last_rendered_output;
                redraw |= current_stream_metrics != last_rendered_metrics;

                while event::poll(Duration::ZERO)? {
                    let event = event::read()?;
                    if is_wait_cancel_escape(&event) {
                        if escape_cancel_state.handle_escape(Instant::now()) {
                            let partial_output = streamed_state
                                .lock()
                                .map(|state| state.output.clone())
                                .unwrap_or_default();
                            drop(prompt_future);
                            return Ok(WaitResult::Cancelled(partial_output));
                        }
                        continue;
                    }
                    escape_cancel_state.reset();
                    let result = handle_input_event(
                        event,
                        input_state,
                        interrupt_state,
                        output_state,
                        InputContext {
                            history,
                            workspace: render.workspace,
                            model_names,
                            render,
                        },
                    );

                    if let Some(outcome) = result.outcome {
                        match outcome {
                            InputResult::Submitted(line) => {
                                let had_pending = pending_commands.len();
                                let _ = prepare_submitted_input(
                                    &line,
                                    history,
                                    history_path,
                                    output_state,
                                    Some(pending_commands),
                                )?;
                                redraw = redraw || pending_commands.len() != had_pending || !line.trim().is_empty();
                            }
                            InputResult::Refresh => {}
                            InputResult::Quit => return Ok(WaitResult::Quit),
                        }
                    }
                    redraw |= result.redraw;
                }

                if redraw {
                    last_rendered_output = current_streamed_output;
                    last_rendered_metrics = current_stream_metrics;
                    let left_status = render_left_status(
                        profile,
                        &last_rendered_output,
                        &last_rendered_metrics,
                        elapsed,
                        thinking_frame,
                        tokenizer.as_ref(),
                    );
                    let pending_line = if last_rendered_output.is_empty() {
                        String::new()
                    } else {
                        render_markdown_for_console(&last_rendered_output)
                    };
                    print_screen(
                        render,
                        ScreenState {
                            transcript: output_state.lines(),
                            scroll_offset: output_state.scroll_offset(),
                            left_status,
                            pending_count: pending_commands.len(),
                            pending_line: Some(pending_line.as_str()),
                            input: input_state.as_str(),
                            cursor: input_state.cursor(),
                        },
                    );
                    std::io::stdout().flush()?;
                }
            }
        }
    }
}

fn render_left_status(
    profile: &LlmConfiguration,
    rendered_output: &str,
    metrics: &StreamMetrics,
    elapsed: Duration,
    frame: usize,
    tokenizer: Option<&tiktoken_rs::CoreBPE>,
) -> Option<StatusFragment> {
    if rendered_output.is_empty() {
        return Some(render_thinking_status(frame, elapsed));
    }

    if profile.provider.eq_ignore_ascii_case("llama.cpp")
        && let Some(rate) = metrics
            .predicted_per_second
            .filter(|rate| *rate > 0.0 && !rendered_output.is_empty())
    {
        return Some(render_working_status(frame, rate, elapsed));
    }

    tokenizer.and_then(|tokenizer| {
        let token_count = tokenizer.encode_with_special_tokens(rendered_output).len();
        let elapsed_secs = elapsed.as_secs_f64();
        (token_count > 0 && elapsed_secs > 0.0)
            .then(|| StatusFragment::plain(format!("{:.1}t/s", token_count as f64 / elapsed_secs)))
    })
}

fn is_wait_cancel_escape(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press,
            ..
        })
    )
}

fn final_pending_line(streamed_output: &str, response: &str) -> Option<String> {
    if !streamed_output.is_empty() {
        Some(streamed_output.to_string())
    } else if !response.is_empty() {
        Some(response.to_string())
    } else {
        None
    }
}

fn request_cancelled_message() -> String {
    format!("{ANSI_FG_LIGHT_RED}Request cancelled.{ANSI_RESET}")
}

fn preserve_cancelled_output(output_state: &mut OutputState, partial_output: &str) {
    if !partial_output.is_empty() {
        output_state.push_markdown(partial_output);
    }
    output_state.push_text(&request_cancelled_message());
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    name: String,
}

async fn probe_header_status(
    http_client: &reqwest::Client,
    workspace: &Path,
    active_model: &str,
    profile: &LlmConfiguration,
    endpoint: Option<&str>,
) -> HeaderStatus {
    let workspace_ok = workspace.exists();
    let mut server_ok = false;
    let mut model_ok = false;

    if let Some(endpoint) = endpoint {
        let models_url = format!("{}/v1/models", normalized_openai_endpoint(endpoint));
        if let Ok(response) = http_client.get(&models_url).send().await
            && response.status().is_success()
        {
            server_ok = true;
            if let Ok(models) = response.json::<ModelsResponse>().await {
                model_ok = models.data.iter().chain(models.models.iter()).any(|entry| {
                    entry.id == profile.model
                        || entry.model == profile.model
                        || entry.name == profile.model
                        || entry.id == active_model
                        || entry.model == active_model
                        || entry.name == active_model
                });
            }
        }
    }

    HeaderStatus {
        workspace_ok,
        server_ok,
        model_ok,
    }
}

fn history_file_path() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(HISTORY_DIRECTORY).join(HISTORY_FILE))
}

fn llm_prompt_block_reason(
    endpoint: Option<&str>,
    _header_status: HeaderStatus,
) -> Option<&'static str> {
    if endpoint.is_none() {
        return Some("Error: Not connected to an LLM server");
    }
    None
}

fn resolve_workspace_root(workspace: Option<PathBuf>) -> Result<PathBuf> {
    let current_dir = std::env::current_dir().context("failed to resolve current directory")?;
    let workspace = workspace.unwrap_or_else(|| current_dir.clone());
    let absolute = if workspace.is_absolute() {
        workspace
    } else {
        current_dir.join(workspace)
    };
    Ok(normalize_path(&absolute))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir => result.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(part) => result.push(part),
        }
    }
    result
}

fn load_history(path: &Path) -> Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => {
            Err(err).with_context(|| format!("failed to read history file {}", path.display()))
        }
    }
}

fn append_history_entry(path: &Path, entry: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create history directory {}", parent.display()))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open history file {}", path.display()))?;
    writeln!(file, "{entry}")
        .with_context(|| format!("failed to write history file {}", path.display()))
}

fn format_models(llms: &HashMap<String, LlmConfiguration>) -> String {
    let mut names = sorted_model_names(llms);
    let mut lines = Vec::with_capacity(names.len());
    for name in names.drain(..) {
        if let Some(llm) = llms.get(&name) {
            lines.push(format!("- {}: {} ({})", name, llm.model, llm.provider));
        }
    }
    lines.join("\n")
}

fn format_tools(tools: &ToolExecutor) -> String {
    tools
        .definitions()
        .into_iter()
        .map(|tool| format!("- {}: {}", tool.function.name, tool.function.description))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        ANSI_FG_LIGHT_GREEN, ANSI_FG_LIGHT_RED, ANSI_RESET, CommandContext, CommandOutcome,
        CommandState, EscapeCancelState, GitLineMetadata, InputContext, InputState, InterruptState,
        LocalCommand, OutputState, RenderContext, ShowFileOptions, colorize_git_status,
        completion_candidates, discover_git_dir, discover_git_root, final_pending_line,
        format_show_file_line, git_workspace_diff, handle_command, handle_input_event,
        delete_branch_output, idle_status_refresh_timeout, init_repo_output, is_protected_branch,
        is_wait_cancel_escape,
        list_workspace_files_tree, llm_prompt_block_reason, parse_local_command,
        parse_show_file_arguments, preserve_cancelled_output, render_left_status,
        render_markdown_for_console, request_cancelled_message, resolve_workspace_root,
        shell_words, show_file_output, system_prompt, with_explicit_pager_width,
        workspace_branch_name,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use orangu::{
        config::LlmConfiguration,
        llm::{StreamMetrics, StreamPromptProgress, normalized_openai_endpoint},
        session::ChatSession,
        tools::ToolExecutor,
        tui::{HeaderStatus, TranscriptLine},
    };
    use std::collections::HashMap;
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    fn process_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_process_env() -> std::sync::MutexGuard<'static, ()> {
        process_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: tests serialize process-wide environment changes with process_env_lock().
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn set_value(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: tests serialize process-wide environment changes with process_env_lock().
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: tests serialize process-wide environment changes with process_env_lock().
            unsafe {
                match &self.original {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn init_test_git_repo(workspace: &std::path::Path) {
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(workspace)
                .status()
                .expect("git init")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["config", "user.name", "Orangu Tests"])
                .current_dir(workspace)
                .status()
                .expect("git config name")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["config", "user.email", "tests@example.com"])
                .current_dir(workspace)
                .status()
                .expect("git config email")
                .success()
        );
    }

    fn test_profile(provider: &str, endpoint: &str, model: &str) -> LlmConfiguration {
        LlmConfiguration {
            provider: provider.to_string(),
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            api_key: None,
            request_timeout_seconds: 1800,
            max_tool_rounds: 10,
            system_prompt: String::new(),
        }
    }

    fn test_input_context<'a>(workspace: &'a std::path::Path) -> InputContext<'a> {
        InputContext {
            history: &[],
            workspace,
            model_names: &[],
            render: RenderContext {
                current_model: "default",
                endpoint: "http://localhost:11434/v1",
                workspace,
                prompt_branch: None,
                header_status: HeaderStatus {
                    workspace_ok: true,
                    server_ok: true,
                    model_ok: true,
                },
            },
        }
    }

    #[test]
    fn resolve_workspace_root_makes_relative_paths_absolute() {
        let current_dir = std::env::current_dir().expect("current directory");
        let resolved = resolve_workspace_root(Some(PathBuf::from("."))).expect("workspace");

        assert_eq!(resolved, current_dir);
        assert!(resolved.is_absolute());
    }

    #[test]
    fn resolve_workspace_root_normalizes_parent_segments() {
        let current_dir = std::env::current_dir().expect("current directory");
        let resolved =
            resolve_workspace_root(Some(PathBuf::from("src/../tests"))).expect("workspace");

        assert_eq!(resolved, current_dir.join("tests"));
    }

    #[test]
    fn output_state_keeps_last_ten_thousand_lines() {
        let mut output_state = OutputState::default();
        for index in 0..10_005 {
            output_state.push_text(&format!("line {index}"));
        }

        assert_eq!(output_state.lines().len(), 10_000);
        assert_eq!(
            output_state.lines().first().map(TranscriptLine::as_str),
            Some("line 5")
        );
        assert_eq!(
            output_state.lines().last().map(TranscriptLine::as_str),
            Some("line 10004")
        );
    }

    #[test]
    fn output_state_styles_echoed_user_input() {
        let mut output_state = OutputState::default();

        output_state.push_input("> show README.md");
        output_state.push_text("plain output");

        assert!(
            matches!(output_state.lines().first(), Some(TranscriptLine::UserInput(s)) if s == "> show README.md")
        );
        assert!(
            matches!(output_state.lines().get(1), Some(TranscriptLine::Plain(s)) if s == "plain output")
        );
    }

    #[test]
    fn renders_markdown_emphasis_for_console() {
        let rendered = render_markdown_for_console("Hello **bold** and *italic*.");

        assert!(rendered.contains("\x1b[1mbold\x1b[22m"));
        assert!(rendered.contains("\x1b[3mitalic\x1b[23m"));
    }

    #[test]
    fn renders_markdown_blocks_for_console() {
        let rendered = render_markdown_for_console(
            "# Title\n\n- one\n- two\n\n`code`\n\n[docs](https://example.com)",
        );

        assert!(rendered.contains("\x1b[1m# Title\x1b[22m"));
        assert!(rendered.contains("- one"));
        assert!(rendered.contains("- two"));
        assert!(rendered.contains("\x1b[38;2;255;215;120m`code\x1b[39m`"));
        assert!(rendered.contains("docs"));
        assert!(rendered.contains("https://example.com"));
    }

    #[test]
    fn renders_fenced_code_blocks_with_syntax_highlighting() {
        let rendered = render_markdown_for_console("```c\nprintf(\"Hello World !\\\\n\");\n```");

        assert!(rendered.contains("```c"));
        assert!(rendered.contains("printf"));
        assert!(rendered.contains("\x1b["));
    }

    #[test]
    fn renders_unknown_fenced_code_blocks_with_plain_code_color() {
        let rendered = render_markdown_for_console("```unknownlang\nplain text\n```");

        assert!(rendered.contains("```unknownlang"));
        assert!(rendered.contains("\x1b[38;2;255;215;120mplain text\x1b[39m"));
    }

    #[test]
    fn open_file_failure_returns_output_instead_of_error() {
        let llms = HashMap::from([(
            "llama".to_string(),
            test_profile("llama.cpp", "http://localhost:8100/v1", "gemma"),
        )]);
        let workspace = tempdir().expect("workspace");
        let tools = ToolExecutor::new(workspace.path());
        let mut active_model = "llama".to_string();
        let mut current_endpoint = Some(normalized_openai_endpoint("http://localhost:8100/v1"));
        let mut session = ChatSession::new("system");

        let outcome = handle_command(
            "/open_file /etc/hosts",
            CommandState {
                active_model: &mut active_model,
                current_endpoint: &mut current_endpoint,
                session: &mut session,
            },
            CommandContext {
                startup_model: "llama",
                startup_endpoint: "http://localhost:8100/v1",
                llms: &llms,
                tools: &tools,
                workspace: workspace.path(),
            },
        )
        .expect("handle command");

        assert!(matches!(
            outcome,
            CommandOutcome::Output(message) if message.starts_with("Error: ")
        ));
    }

    #[test]
    fn alt_backspace_deletes_previous_bash_word() {
        let workspace = tempdir().expect("workspace");
        let mut input_state = InputState::default();
        input_state.set_buffer("src/tui.rs".to_string());
        let mut interrupt_state = InterruptState::default();
        let mut output_state = OutputState::default();

        let result = handle_input_event(
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::ALT,
                KeyEventKind::Press,
            )),
            &mut input_state,
            &mut interrupt_state,
            &mut output_state,
            test_input_context(workspace.path()),
        );

        assert!(result.redraw);
        assert!(result.outcome.is_none());
        assert_eq!(input_state.as_str(), "src/tui.");
        assert_eq!(input_state.cursor(), "src/tui.".len());
    }

    #[test]
    fn alt_d_deletes_next_bash_word() {
        let workspace = tempdir().expect("workspace");
        let mut input_state = InputState::default();
        input_state.set_buffer("src/tui.rs".to_string());
        input_state.move_home();
        let mut interrupt_state = InterruptState::default();
        let mut output_state = OutputState::default();

        let result = handle_input_event(
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('d'),
                KeyModifiers::ALT,
                KeyEventKind::Press,
            )),
            &mut input_state,
            &mut interrupt_state,
            &mut output_state,
            test_input_context(workspace.path()),
        );

        assert!(result.redraw);
        assert!(result.outcome.is_none());
        assert_eq!(input_state.as_str(), "/tui.rs");
        assert_eq!(input_state.cursor(), 0);
    }

    #[test]
    fn ctrl_left_moves_to_previous_bash_word() {
        let workspace = tempdir().expect("workspace");
        let mut input_state = InputState::default();
        input_state.set_buffer("src/tui.rs".to_string());
        let mut interrupt_state = InterruptState::default();
        let mut output_state = OutputState::default();

        let result = handle_input_event(
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Left,
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            )),
            &mut input_state,
            &mut interrupt_state,
            &mut output_state,
            test_input_context(workspace.path()),
        );

        assert!(result.redraw);
        assert!(result.outcome.is_none());
        assert_eq!(input_state.cursor(), "src/tui.".len());
    }

    #[test]
    fn ctrl_right_moves_to_next_bash_word() {
        let workspace = tempdir().expect("workspace");
        let mut input_state = InputState::default();
        input_state.set_buffer("src/tui.rs".to_string());
        input_state.move_home();
        let mut interrupt_state = InterruptState::default();
        let mut output_state = OutputState::default();

        let result = handle_input_event(
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Right,
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            )),
            &mut input_state,
            &mut interrupt_state,
            &mut output_state,
            test_input_context(workspace.path()),
        );

        assert!(result.redraw);
        assert!(result.outcome.is_none());
        assert_eq!(input_state.cursor(), 3);
    }

    #[test]
    fn ctrl_w_keeps_whitespace_based_word_deletion() {
        let mut input_state = InputState::default();
        input_state.set_buffer("src/tui.rs".to_string());

        input_state.delete_prev_word();

        assert_eq!(input_state.as_str(), "");
        assert_eq!(input_state.cursor(), 0);
    }

    #[test]
    fn missing_required_command_arguments_return_usage_output() {
        let llms = HashMap::from([(
            "llama".to_string(),
            test_profile("llama.cpp", "http://localhost:8100/v1", "gemma"),
        )]);
        let workspace = tempdir().expect("workspace");
        let tools = ToolExecutor::new(workspace.path());

        for (input, expected) in [
            (
                "/show_file",
                "Usage: /show_file [--hash] [--author] <path>. Use /help to see available commands.",
            ),
            (
                "/show_file --hash",
                "Usage: /show_file [--hash] [--author] <path>. Use /help to see available commands.",
            ),
            (
                "/open_file",
                "Usage: /open_file <path>. Use /help to see available commands.",
            ),
        ] {
            let mut active_model = "llama".to_string();
            let mut current_endpoint = Some(normalized_openai_endpoint("http://localhost:8100/v1"));
            let mut session = ChatSession::new("system");

            let outcome = handle_command(
                input,
                CommandState {
                    active_model: &mut active_model,
                    current_endpoint: &mut current_endpoint,
                    session: &mut session,
                },
                CommandContext {
                    startup_model: "llama",
                    startup_endpoint: "http://localhost:8100/v1",
                    llms: &llms,
                    tools: &tools,
                    workspace: workspace.path(),
                },
            )
            .expect("handle command");

            assert!(
                matches!(outcome, CommandOutcome::Output(message) if message == expected),
                "unexpected outcome for {input:?}"
            );
        }
    }

    #[test]
    fn list_files_outputs_filtered_workspace_tree() {
        let llms = HashMap::from([(
            "llama".to_string(),
            test_profile("llama.cpp", "http://localhost:8100/v1", "gemma"),
        )]);
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join("README.md"), "readme").expect("root file");
        fs::create_dir(workspace.path().join("doc")).expect("doc dir");
        fs::write(workspace.path().join("doc/guide.txt"), "guide").expect("doc file");
        fs::create_dir(workspace.path().join("src")).expect("src dir");
        fs::write(workspace.path().join("src/lib.rs"), "pub fn lib() {}").expect("src file");
        fs::create_dir(workspace.path().join(".git")).expect("git dir");
        fs::write(workspace.path().join(".git/config"), "[core]").expect("git config");
        fs::create_dir(workspace.path().join("build")).expect("build dir");
        fs::write(workspace.path().join("build/output.txt"), "artifact").expect("build file");
        fs::create_dir(workspace.path().join("target")).expect("target dir");
        fs::write(workspace.path().join("target/app"), "binary").expect("target file");

        let tree = list_workspace_files_tree(workspace.path()).expect("tree");
        assert_eq!(
            tree,
            format!(
                "{}\n├── doc\n│   └── guide.txt\n├── src\n│   └── lib.rs\n└── README.md",
                workspace.path().display()
            )
        );

        let tools = ToolExecutor::new(workspace.path());
        let mut active_model = "llama".to_string();
        let mut current_endpoint = Some(normalized_openai_endpoint("http://localhost:8100/v1"));
        let mut session = ChatSession::new("system");
        let outcome = handle_command(
            "/list_files",
            CommandState {
                active_model: &mut active_model,
                current_endpoint: &mut current_endpoint,
                session: &mut session,
            },
            CommandContext {
                startup_model: "llama",
                startup_endpoint: "http://localhost:8100/v1",
                llms: &llms,
                tools: &tools,
                workspace: workspace.path(),
            },
        )
        .expect("handle command");

        assert!(matches!(outcome, CommandOutcome::Output(output) if output == tree));
    }

    #[test]
    fn parses_open_file_commands() {
        match parse_local_command("/open_file README.md") {
            Some(LocalCommand::OpenFile(path)) => assert_eq!(path, "README.md"),
            _ => panic!("expected open file slash command"),
        }
        match parse_local_command("Open README.md") {
            Some(LocalCommand::OpenFile(path)) => assert_eq!(path, "README.md"),
            _ => panic!("expected open file natural language command"),
        }
        match parse_local_command("open \"docs/user guide.md\"") {
            Some(LocalCommand::OpenFile(path)) => assert_eq!(path, "docs/user guide.md"),
            _ => panic!("expected quoted natural language open file command"),
        }
    }

    #[test]
    fn parses_show_file_natural_language_commands() {
        match parse_local_command("show README.md") {
            Some(LocalCommand::ShowFile(path)) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected natural language show file command"),
        }
        match parse_local_command("show file \"docs/user guide.md\"") {
            Some(LocalCommand::ShowFile(path)) => assert_eq!(path.as_ref(), "docs/user guide.md"),
            _ => panic!("expected quoted natural language show file command"),
        }
        match parse_local_command("show src/tui.rs with hash") {
            Some(LocalCommand::ShowFile(args)) => assert_eq!(args.as_ref(), "--hash src/tui.rs"),
            _ => panic!("expected natural language show file hash command"),
        }
        match parse_local_command("show src/tui.rs with author") {
            Some(LocalCommand::ShowFile(args)) => {
                assert_eq!(args.as_ref(), "--author src/tui.rs")
            }
            _ => panic!("expected natural language show file author command"),
        }
        match parse_local_command("show file \"docs/user guide.md\" with hash and author") {
            Some(LocalCommand::ShowFile(args)) => {
                assert_eq!(args.as_ref(), "--hash --author \"docs/user guide.md\"")
            }
            _ => panic!("expected natural language show file metadata command"),
        }
    }

    #[test]
    fn parses_show_file_commands() {
        match parse_local_command("/show_file README.md") {
            Some(LocalCommand::ShowFile(args)) => assert_eq!(args.as_ref(), "README.md"),
            _ => panic!("expected show file slash command"),
        }

        let (path, options) = parse_show_file_arguments("--hash --author \"docs/user guide.md\"")
            .expect("show file args");
        assert_eq!(path, "docs/user guide.md");
        assert!(options.show_hash);
        assert!(options.show_author);
    }

    #[test]
    fn parses_list_files_commands() {
        assert!(matches!(
            parse_local_command("/list_files"),
            Some(LocalCommand::ListFiles)
        ));
        assert!(matches!(
            parse_local_command("list files"),
            Some(LocalCommand::ListFiles)
        ));
        assert!(matches!(
            parse_local_command("show workspace files"),
            Some(LocalCommand::ListFiles)
        ));
    }

    #[test]
    fn parses_natural_language_command_aliases() {
        assert!(matches!(
            parse_local_command("show commands"),
            Some(LocalCommand::Help)
        ));
        assert!(matches!(
            parse_local_command("diff"),
            Some(LocalCommand::Diff)
        ));
        assert!(matches!(
            parse_local_command("list models"),
            Some(LocalCommand::ListModels)
        ));
        assert!(matches!(
            parse_local_command("show tools"),
            Some(LocalCommand::Tools)
        ));
        assert!(matches!(
            parse_local_command("disconnect"),
            Some(LocalCommand::Disconnect)
        ));
        assert!(matches!(
            parse_local_command("reset conversation"),
            Some(LocalCommand::Clear)
        ));
        assert!(matches!(
            parse_local_command("exit"),
            Some(LocalCommand::Quit)
        ));
    }

    #[test]
    fn parses_natural_language_commands_with_arguments() {
        match parse_local_command("connect to http://localhost:8080/v1") {
            Some(LocalCommand::ConnectTo(endpoint)) => {
                assert_eq!(endpoint, "http://localhost:8080/v1")
            }
            _ => panic!("expected connect command"),
        }
        match parse_local_command("switch model to local") {
            Some(LocalCommand::SetModel(name)) => assert_eq!(name, "local"),
            _ => panic!("expected set model command"),
        }
    }

    #[test]
    fn parses_pull_request_commands() {
        assert!(matches!(
            parse_local_command("/pull 58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
        assert!(matches!(
            parse_local_command("/pull"),
            Some(LocalCommand::Pull(None))
        ));
        assert!(matches!(
            parse_local_command("/pull notanumber"),
            Some(LocalCommand::Pull(None))
        ));
        assert!(matches!(
            parse_local_command("pull 58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
        assert!(matches!(
            parse_local_command("Pull 58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
        assert!(matches!(
            parse_local_command("pull pr 58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
        assert!(matches!(
            parse_local_command("pull request 58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
        assert!(matches!(
            parse_local_command("pull pull request 58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
        assert!(matches!(
            parse_local_command("pull #58"),
            Some(LocalCommand::Pull(Some(58)))
        ));
    }

    #[test]
    fn parses_status_commands() {
        assert!(matches!(
            parse_local_command("/status"),
            Some(LocalCommand::Status)
        ));
        assert!(matches!(
            parse_local_command("status"),
            Some(LocalCommand::Status)
        ));
        assert!(matches!(
            parse_local_command("Status"),
            Some(LocalCommand::Status)
        ));
        assert!(matches!(
            parse_local_command("show status"),
            Some(LocalCommand::Status)
        ));
        assert!(matches!(
            parse_local_command("git status"),
            Some(LocalCommand::Status)
        ));
    }

    #[test]
    fn parses_log_commands() {
        assert!(matches!(
            parse_local_command("/log"),
            Some(LocalCommand::Log)
        ));
        assert!(matches!(
            parse_local_command("log"),
            Some(LocalCommand::Log)
        ));
        assert!(matches!(
            parse_local_command("Log"),
            Some(LocalCommand::Log)
        ));
        assert!(matches!(
            parse_local_command("show log"),
            Some(LocalCommand::Log)
        ));
        assert!(matches!(
            parse_local_command("git log"),
            Some(LocalCommand::Log)
        ));
        assert!(matches!(
            parse_local_command("git lg"),
            Some(LocalCommand::Log)
        ));
    }

    #[test]
    fn colorizes_git_status_output() {
        let raw = "## main...origin/main\nA  new_file.rs\n M modified.rs\nD  deleted.rs\n?? untracked.txt\n";
        let colored = colorize_git_status(raw);
        // Branch line uses subtle color
        assert!(colored.contains("## main...origin/main"));
        // Added line (A) uses green
        let green_start = colored
            .find(ANSI_FG_LIGHT_GREEN)
            .expect("green color present");
        assert!(colored[green_start..].contains("new_file.rs"));
        // Deleted line (D) uses red
        let red_start = colored.find(ANSI_FG_LIGHT_RED).expect("red color present");
        assert!(colored[red_start..].contains("deleted.rs"));
        // Modified line (M) has no special color prefix
        let mod_idx = colored.find("modified.rs").expect("modified.rs present");
        let before_mod = &colored[..mod_idx];
        assert!(!before_mod.ends_with(ANSI_FG_LIGHT_RED));
        assert!(!before_mod.ends_with(ANSI_FG_LIGHT_GREEN));
        // Untracked (??) uses green
        assert!(colored.contains("untracked.txt"));
        let green_positions: Vec<_> = colored.match_indices(ANSI_FG_LIGHT_GREEN).collect();
        assert!(green_positions.len() >= 2);
    }

    #[test]
    fn parses_rebase_commands() {
        assert!(matches!(
            parse_local_command("/rebase"),
            Some(LocalCommand::Rebase)
        ));
        assert!(matches!(
            parse_local_command("rebase"),
            Some(LocalCommand::Rebase)
        ));
        assert!(matches!(
            parse_local_command("Rebase"),
            Some(LocalCommand::Rebase)
        ));
        assert!(matches!(
            parse_local_command("git rebase"),
            Some(LocalCommand::Rebase)
        ));
    }

    #[test]
    fn parses_merge_commands() {
        assert!(matches!(
            parse_local_command("/merge"),
            Some(LocalCommand::Merge(None))
        ));
        assert!(matches!(
            parse_local_command("/merge "),
            Some(LocalCommand::Merge(None))
        ));
        assert!(matches!(
            parse_local_command("merge"),
            Some(LocalCommand::Merge(None))
        ));
        assert!(matches!(
            parse_local_command("Merge"),
            Some(LocalCommand::Merge(None))
        ));
        match parse_local_command("/merge feature/foo") {
            Some(LocalCommand::Merge(Some(branch))) => assert_eq!(branch.as_ref(), "feature/foo"),
            _ => panic!("expected merge with branch"),
        }
        match parse_local_command("merge feature/foo") {
            Some(LocalCommand::Merge(Some(branch))) => assert_eq!(branch.as_ref(), "feature/foo"),
            _ => panic!("expected natural merge with branch"),
        }
        match parse_local_command("Merge feature/foo") {
            Some(LocalCommand::Merge(Some(branch))) => assert_eq!(branch.as_ref(), "feature/foo"),
            _ => panic!("expected case-insensitive merge with branch"),
        }
        match parse_local_command("git merge feature/foo") {
            Some(LocalCommand::Merge(Some(branch))) => assert_eq!(branch.as_ref(), "feature/foo"),
            _ => panic!("expected git merge natural language with branch"),
        }
    }

    #[test]
    fn completes_merge_branch_names() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "").expect("readme");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["branch", "feature/test"])
                .current_dir(workspace.path())
                .status()
                .expect("git branch")
                .success()
        );

        let (start, _, candidates) = completion_candidates(
            "/merge feature/",
            "/merge feature/".len(),
            workspace.path(),
            &[],
        )
        .expect("merge completion");
        assert_eq!(start, "/merge ".len());
        assert_eq!(candidates, vec!["feature/test".to_string()]);

        let (start, _, natural_candidates) = completion_candidates(
            "merge feature/",
            "merge feature/".len(),
            workspace.path(),
            &[],
        )
        .expect("natural merge completion");
        assert_eq!(start, "merge ".len());
        assert_eq!(natural_candidates, vec!["feature/test".to_string()]);
    }

    #[test]
    fn parses_checkout_commands() {
        assert!(matches!(
            parse_local_command("/checkout"),
            Some(LocalCommand::Checkout(None))
        ));
        assert!(matches!(
            parse_local_command("/checkout "),
            Some(LocalCommand::Checkout(None))
        ));
        assert!(matches!(
            parse_local_command("checkout"),
            Some(LocalCommand::Checkout(None))
        ));
        assert!(matches!(
            parse_local_command("Checkout"),
            Some(LocalCommand::Checkout(None))
        ));
        match parse_local_command("/checkout feature/foo") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "feature/foo")
            }
            _ => panic!("expected checkout with branch"),
        }
        match parse_local_command("checkout feature/foo") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "feature/foo")
            }
            _ => panic!("expected natural checkout with branch"),
        }
        match parse_local_command("Checkout README.md") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "README.md")
            }
            _ => panic!("expected case-insensitive checkout with file"),
        }
        match parse_local_command("git checkout feature/foo") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "feature/foo")
            }
            _ => panic!("expected git checkout natural language"),
        }
        match parse_local_command("switch to main") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "main")
            }
            _ => panic!("expected switch to main"),
        }
        match parse_local_command("Switch to main") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "main")
            }
            _ => panic!("expected case-insensitive switch to main"),
        }
        match parse_local_command("switch to feature/foo") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "feature/foo")
            }
            _ => panic!("expected switch to feature/foo"),
        }
        match parse_local_command("switch to main branch") {
            Some(LocalCommand::Checkout(Some(target))) => {
                assert_eq!(target.as_ref(), "main")
            }
            _ => panic!("expected switch to main branch -> main"),
        }
        assert!(matches!(
            parse_local_command("switch to"),
            Some(LocalCommand::Checkout(None))
        ));
    }

    #[test]
    fn completes_checkout_targets() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "").expect("readme");
        fs::write(workspace.path().join("todo.txt"), "").expect("todo");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md", "todo.txt"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["branch", "topic/fix"])
                .current_dir(workspace.path())
                .status()
                .expect("git branch")
                .success()
        );

        // Branch match: "t" matches "topic/fix" before "todo.txt"
        let (start, _, candidates) =
            completion_candidates("/checkout t", "/checkout t".len(), workspace.path(), &[])
                .expect("checkout completion");
        assert_eq!(start, "/checkout ".len());
        assert_eq!(candidates[0], "topic/fix");
        assert!(candidates.contains(&"todo.txt".to_string()));

        // Natural language form
        let (start, _, natural_candidates) =
            completion_candidates("checkout t", "checkout t".len(), workspace.path(), &[])
                .expect("natural checkout completion");
        assert_eq!(start, "checkout ".len());
        assert_eq!(natural_candidates[0], "topic/fix");
        assert!(natural_candidates.contains(&"todo.txt".to_string()));
    }

    #[test]
    fn switch_to_completes_branches_and_tags() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("main.rs"), "").expect("main.rs");
        assert!(
            std::process::Command::new("git")
                .args(["add", "main.rs"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );
        // Create a branch and a tag both starting with "m"
        assert!(
            std::process::Command::new("git")
                .args(["branch", "mybranch"])
                .current_dir(workspace.path())
                .status()
                .expect("git branch")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["tag", "mytag"])
                .current_dir(workspace.path())
                .status()
                .expect("git tag")
                .success()
        );

        let (start, _, candidates) =
            completion_candidates("switch to m", "switch to m".len(), workspace.path(), &[])
                .expect("switch to completion");
        assert_eq!(start, "switch to ".len());
        assert!(candidates.contains(&"mybranch".to_string()), "branch missing");
        assert!(candidates.contains(&"mytag".to_string()), "tag missing");
        // workspace files should NOT appear
        assert!(!candidates.contains(&"main.rs".to_string()), "file should not appear");
    }

    #[test]
    fn parses_add_file_commands() {
        assert!(matches!(
            parse_local_command("/add_file"),
            Some(LocalCommand::AddFile(None))
        ));
        assert!(matches!(
            parse_local_command("/add_file "),
            Some(LocalCommand::AddFile(None))
        ));
        assert!(matches!(
            parse_local_command("add"),
            Some(LocalCommand::AddFile(None))
        ));
        assert!(matches!(
            parse_local_command("Add"),
            Some(LocalCommand::AddFile(None))
        ));
        match parse_local_command("/add_file README.md") {
            Some(LocalCommand::AddFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected add_file with path"),
        }
        match parse_local_command("add README.md") {
            Some(LocalCommand::AddFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected natural add with path"),
        }
        match parse_local_command("Add src/") {
            Some(LocalCommand::AddFile(Some(path))) => assert_eq!(path.as_ref(), "src/"),
            _ => panic!("expected case-insensitive add with directory"),
        }
        match parse_local_command("add file README.md") {
            Some(LocalCommand::AddFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected add file prefix"),
        }
        match parse_local_command("git add README.md") {
            Some(LocalCommand::AddFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected git add natural language"),
        }
    }

    #[test]
    fn completes_add_file_untracked() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("tracked.rs"), "").expect("tracked file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "tracked.rs"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );
        fs::create_dir(workspace.path().join("newdir")).expect("new dir");
        fs::write(workspace.path().join("newdir/file.rs"), "").expect("dir file");
        fs::write(workspace.path().join("newfile.txt"), "").expect("new file");

        // "n" matches directory "newdir/" before file "newfile.txt"
        let (start, _, candidates) =
            completion_candidates("/add_file n", "/add_file n".len(), workspace.path(), &[])
                .expect("add_file completion");
        assert_eq!(start, "/add_file ".len());
        assert_eq!(candidates[0], "newdir/");
        assert!(candidates.contains(&"newfile.txt".to_string()));
        // tracked file not included
        assert!(!candidates.contains(&"tracked.rs".to_string()));

        // Natural-language form
        let (start, _, nat_candidates) =
            completion_candidates("add n", "add n".len(), workspace.path(), &[])
                .expect("natural add_file completion");
        assert_eq!(start, "add ".len());
        assert_eq!(nat_candidates[0], "newdir/");
    }

    #[test]
    fn parses_remove_file_commands() {
        assert!(matches!(
            parse_local_command("/remove_file"),
            Some(LocalCommand::RemoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("/remove_file "),
            Some(LocalCommand::RemoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("remove"),
            Some(LocalCommand::RemoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("Remove"),
            Some(LocalCommand::RemoveFile(None))
        ));
        match parse_local_command("/remove_file README.md") {
            Some(LocalCommand::RemoveFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected remove_file with path"),
        }
        match parse_local_command("remove README.md") {
            Some(LocalCommand::RemoveFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected natural remove with path"),
        }
        match parse_local_command("Remove src/") {
            Some(LocalCommand::RemoveFile(Some(path))) => assert_eq!(path.as_ref(), "src/"),
            _ => panic!("expected case-insensitive remove with directory"),
        }
        match parse_local_command("remove file README.md") {
            Some(LocalCommand::RemoveFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected remove file prefix"),
        }
        match parse_local_command("git rm README.md") {
            Some(LocalCommand::RemoveFile(Some(path))) => assert_eq!(path.as_ref(), "README.md"),
            _ => panic!("expected git rm natural language"),
        }
    }

    #[test]
    fn completes_remove_file_tracked() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::create_dir(workspace.path().join("src")).expect("src dir");
        fs::write(workspace.path().join("src/main.rs"), "").expect("main.rs");
        fs::write(workspace.path().join("schema.sql"), "").expect("schema.sql");
        fs::write(workspace.path().join("untracked.txt"), "").expect("untracked");
        assert!(
            std::process::Command::new("git")
                .args(["add", "src/main.rs", "schema.sql"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        // "s" matches directory "src/" before file "schema.sql"
        let (start, _, candidates) = completion_candidates(
            "/remove_file s",
            "/remove_file s".len(),
            workspace.path(),
            &[],
        )
        .expect("remove_file completion");
        assert_eq!(start, "/remove_file ".len());
        assert_eq!(candidates[0], "src/");
        assert!(candidates.contains(&"schema.sql".to_string()));
        // untracked file not included
        assert!(!candidates.contains(&"untracked.txt".to_string()));

        // Natural-language form
        let (start, _, nat_candidates) =
            completion_candidates("remove s", "remove s".len(), workspace.path(), &[])
                .expect("natural remove_file completion");
        assert_eq!(start, "remove ".len());
        assert_eq!(nat_candidates[0], "src/");
    }

    #[test]
    fn parses_move_file_commands() {
        assert!(matches!(
            parse_local_command("/move_file"),
            Some(LocalCommand::MoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("/move_file "),
            Some(LocalCommand::MoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("/move_file onlyone"),
            Some(LocalCommand::MoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("move"),
            Some(LocalCommand::MoveFile(None))
        ));
        assert!(matches!(
            parse_local_command("Move"),
            Some(LocalCommand::MoveFile(None))
        ));
        match parse_local_command("/move_file old.rs new.rs") {
            Some(LocalCommand::MoveFile(Some((src, dst)))) => {
                assert_eq!(src.as_ref(), "old.rs");
                assert_eq!(dst.as_ref(), "new.rs");
            }
            _ => panic!("expected move_file with source and destination"),
        }
        match parse_local_command("move old.rs new.rs") {
            Some(LocalCommand::MoveFile(Some((src, dst)))) => {
                assert_eq!(src.as_ref(), "old.rs");
                assert_eq!(dst.as_ref(), "new.rs");
            }
            _ => panic!("expected natural move with source and destination"),
        }
        match parse_local_command("move file old.rs new.rs") {
            Some(LocalCommand::MoveFile(Some((src, dst)))) => {
                assert_eq!(src.as_ref(), "old.rs");
                assert_eq!(dst.as_ref(), "new.rs");
            }
            _ => panic!("expected move file prefix"),
        }
        match parse_local_command("git mv old.rs new.rs") {
            Some(LocalCommand::MoveFile(Some((src, dst)))) => {
                assert_eq!(src.as_ref(), "old.rs");
                assert_eq!(dst.as_ref(), "new.rs");
            }
            _ => panic!("expected git mv natural language"),
        }
    }

    #[test]
    fn completes_move_file_targets() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::create_dir(workspace.path().join("src")).expect("src dir");
        fs::write(workspace.path().join("src/main.rs"), "").expect("main.rs");
        fs::write(workspace.path().join("readme.md"), "").expect("readme");
        fs::write(workspace.path().join("untracked.txt"), "").expect("untracked");
        assert!(
            std::process::Command::new("git")
                .args(["add", "src/main.rs", "readme.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        // First arg: "s" matches tracked "src/" (directory) — untracked file absent
        let (start, _, src_candidates) =
            completion_candidates("/move_file s", "/move_file s".len(), workspace.path(), &[])
                .expect("move_file source completion");
        assert_eq!(start, "/move_file ".len());
        assert_eq!(src_candidates[0], "src/");
        assert!(!src_candidates.contains(&"untracked.txt".to_string()));

        // Second arg: completes workspace files (not filtered by tracked status)
        let (start, _, dst_candidates) = completion_candidates(
            "/move_file src/main.rs u",
            "/move_file src/main.rs u".len(),
            workspace.path(),
            &[],
        )
        .expect("move_file destination completion");
        assert_eq!(start, "/move_file src/main.rs ".len());
        assert!(dst_candidates.contains(&"untracked.txt".to_string()));

        // Natural-language form — first arg
        let (start, _, nat_candidates) =
            completion_candidates("move s", "move s".len(), workspace.path(), &[])
                .expect("natural move_file completion");
        assert_eq!(start, "move ".len());
        assert_eq!(nat_candidates[0], "src/");
    }

    #[test]
    fn parses_cherry_pick_commands() {
        assert!(matches!(
            parse_local_command("/cherry_pick"),
            Some(LocalCommand::CherryPick(None))
        ));
        match parse_local_command("/cherry_pick abc1234") {
            Some(LocalCommand::CherryPick(Some(commit))) => {
                assert_eq!(commit.as_ref(), "abc1234");
            }
            _ => panic!("expected cherry_pick with commit"),
        }
        match parse_local_command("cherry pick abc1234") {
            Some(LocalCommand::CherryPick(Some(commit))) => {
                assert_eq!(commit.as_ref(), "abc1234");
            }
            _ => panic!("expected natural cherry pick with commit"),
        }
        match parse_local_command("cherry-pick abc1234") {
            Some(LocalCommand::CherryPick(Some(commit))) => {
                assert_eq!(commit.as_ref(), "abc1234");
            }
            _ => panic!("expected cherry-pick with commit"),
        }
        match parse_local_command("git cherry-pick abc1234") {
            Some(LocalCommand::CherryPick(Some(commit))) => {
                assert_eq!(commit.as_ref(), "abc1234");
            }
            _ => panic!("expected git cherry-pick with commit"),
        }
        assert!(matches!(
            parse_local_command("cherry pick"),
            Some(LocalCommand::CherryPick(None))
        ));
        assert!(matches!(
            parse_local_command("cherry-pick"),
            Some(LocalCommand::CherryPick(None))
        ));
    }

    #[test]
    fn completes_cherry_pick_commits() {
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("readme.md"), "initial").expect("readme");
        assert!(
            std::process::Command::new("git")
                .args(["add", "readme.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--quiet", "-m", "first commit"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        // Completion with no token returns recent commit hashes from main
        let result = completion_candidates(
            "/cherry_pick ",
            "/cherry_pick ".len(),
            workspace.path(),
            &[],
        );
        if let Some((start, _, candidates)) = result {
            assert_eq!(start, "/cherry_pick ".len());
            // Abbreviated hashes are 7 chars
            assert!(candidates.iter().all(|h| h.len() >= 4));
        }

        // Natural-language form triggers completion
        let nl_result =
            completion_candidates("cherry pick ", "cherry pick ".len(), workspace.path(), &[]);
        if let Some((start, _, _)) = nl_result {
            assert_eq!(start, "cherry pick ".len());
        }
    }

    #[test]
    fn parses_commit_commands() {
        assert!(matches!(
            parse_local_command("/commit"),
            Some(LocalCommand::Commit(None))
        ));
        assert!(matches!(
            parse_local_command("commit"),
            Some(LocalCommand::Commit(None))
        ));
        match parse_local_command("/commit [#42] My feature") {
            Some(LocalCommand::Commit(Some(msg))) => {
                assert_eq!(msg.as_ref(), "[#42] My feature");
            }
            _ => panic!("expected commit with plain message"),
        }
        match parse_local_command("/commit \"[#42] My feature\"") {
            Some(LocalCommand::Commit(Some(msg))) => {
                assert_eq!(msg.as_ref(), "[#42] My feature");
            }
            _ => panic!("expected commit with double-quoted message"),
        }
        match parse_local_command("Commit \"[#42] My feature\"") {
            Some(LocalCommand::Commit(Some(msg))) => {
                assert_eq!(msg.as_ref(), "[#42] My feature");
            }
            _ => panic!("expected natural commit with quoted message"),
        }
        match parse_local_command("commit [#42] My feature") {
            Some(LocalCommand::Commit(Some(msg))) => {
                assert_eq!(msg.as_ref(), "[#42] My feature");
            }
            _ => panic!("expected natural commit without quotes"),
        }
        match parse_local_command("git commit -a -m \"[#42] My feature\"") {
            Some(LocalCommand::Commit(Some(msg))) => {
                assert_eq!(msg.as_ref(), "[#42] My feature");
            }
            _ => panic!("expected git commit -a -m with quoted message"),
        }
        match parse_local_command("git commit -m fixed") {
            Some(LocalCommand::Commit(Some(msg))) => {
                assert_eq!(msg.as_ref(), "fixed");
            }
            _ => panic!("expected git commit -m form"),
        }
    }

    #[test]
    fn parses_push_commands() {
        assert!(matches!(
            parse_local_command("/push"),
            Some(LocalCommand::Push(false))
        ));
        assert!(matches!(
            parse_local_command("/push --force"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("/push -f"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("/push force"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("push"),
            Some(LocalCommand::Push(false))
        ));
        assert!(matches!(
            parse_local_command("Push"),
            Some(LocalCommand::Push(false))
        ));
        assert!(matches!(
            parse_local_command("git push"),
            Some(LocalCommand::Push(false))
        ));
        assert!(matches!(
            parse_local_command("force push"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("push force"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("push --force"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("git push --force"),
            Some(LocalCommand::Push(true))
        ));
        assert!(matches!(
            parse_local_command("git push origin --force"),
            Some(LocalCommand::Push(true))
        ));
    }

    #[test]
    fn force_push_blocked_on_protected_branches() {
        assert!(is_protected_branch("main"));
        assert!(is_protected_branch("master"));
        assert!(!is_protected_branch("feature/my-branch"));
        assert!(!is_protected_branch("develop"));
    }

    #[test]
    fn parses_init_repo_commands() {
        assert!(matches!(
            parse_local_command("/init_repo"),
            Some(LocalCommand::InitRepo)
        ));
        assert!(matches!(
            parse_local_command("init"),
            Some(LocalCommand::InitRepo)
        ));
        assert!(matches!(
            parse_local_command("Init"),
            Some(LocalCommand::InitRepo)
        ));
        assert!(matches!(
            parse_local_command("init repo"),
            Some(LocalCommand::InitRepo)
        ));
        assert!(matches!(
            parse_local_command("Init Repo"),
            Some(LocalCommand::InitRepo)
        ));
        assert!(matches!(
            parse_local_command("git init"),
            Some(LocalCommand::InitRepo)
        ));
    }

    #[test]
    fn init_repo_creates_git_repository() {
        let workspace = tempdir().expect("workspace");
        assert!(!workspace.path().join(".git").exists());
        let result = init_repo_output(workspace.path());
        assert!(result.is_ok(), "init_repo_output failed: {:?}", result);
        assert!(workspace.path().join(".git").exists());
    }

    #[test]
    fn parses_delete_branch_commands() {
        assert!(matches!(
            parse_local_command("/delete feature/foo"),
            Some(LocalCommand::DeleteBranch(Some(_)))
        ));
        assert!(matches!(
            parse_local_command("/delete"),
            Some(LocalCommand::DeleteBranch(None))
        ));
        assert!(matches!(
            parse_local_command("delete feature/foo"),
            Some(LocalCommand::DeleteBranch(Some(_)))
        ));
        assert!(matches!(
            parse_local_command("Delete feature/foo"),
            Some(LocalCommand::DeleteBranch(Some(_)))
        ));
        assert!(matches!(
            parse_local_command("delete branch feature/foo"),
            Some(LocalCommand::DeleteBranch(Some(_)))
        ));
        assert!(matches!(
            parse_local_command("Delete Branch feature/foo"),
            Some(LocalCommand::DeleteBranch(Some(_)))
        ));
        assert!(matches!(
            parse_local_command("git branch -D feature/foo"),
            Some(LocalCommand::DeleteBranch(Some(_)))
        ));
        assert!(matches!(
            parse_local_command("delete branch"),
            Some(LocalCommand::DeleteBranch(None))
        ));
        assert!(matches!(
            parse_local_command("delete"),
            Some(LocalCommand::DeleteBranch(None))
        ));
    }

    #[test]
    fn delete_branch_blocked_on_protected_branches() {
        let workspace = tempdir().expect("workspace");
        std::process::Command::new("git")
            .arg("init")
            .current_dir(workspace.path())
            .output()
            .expect("git init");
        for branch in ["main", "master"] {
            let result = super::delete_branch_output(workspace.path(), branch);
            assert!(result.is_err(), "should block deletion of '{branch}'");
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains(branch),
                "error should mention branch name: {msg}"
            );
        }
    }

    #[test]
    fn leaves_regular_prompts_unhandled() {
        assert!(parse_local_command("help me understand this code").is_none());
        assert!(parse_local_command("show me the files in the workspace").is_none());
    }

    #[test]
    fn set_model_switches_active_endpoint() {
        const GEMMA: &str = "gemma-4-E4B-it-GGUF";
        const OPENAI: &str = "gpt-4.1";

        let llms = HashMap::from([
            (
                GEMMA.to_string(),
                test_profile(
                    "llama.cpp",
                    "http://localhost:8100/v1",
                    "ggml-org/gemma-4-E4B-it-GGUF",
                ),
            ),
            (
                OPENAI.to_string(),
                test_profile("openai", "https://api.openai.com/v1", "gpt-4.1"),
            ),
        ]);
        let workspace = tempdir().expect("workspace");
        let tools = ToolExecutor::new(workspace.path());
        let mut active_model = GEMMA.to_string();
        let mut current_endpoint = Some(normalized_openai_endpoint("http://localhost:8100/v1"));
        let mut session = ChatSession::new("system");

        let outcome = handle_command(
            "/model gpt-4.1",
            CommandState {
                active_model: &mut active_model,
                current_endpoint: &mut current_endpoint,
                session: &mut session,
            },
            CommandContext {
                startup_model: GEMMA,
                startup_endpoint: "http://localhost:8100/v1",
                llms: &llms,
                tools: &tools,
                workspace: workspace.path(),
            },
        )
        .expect("handle command");

        assert!(matches!(outcome, CommandOutcome::Quiet));
        assert_eq!(active_model, OPENAI);
        assert_eq!(
            current_endpoint,
            Some(normalized_openai_endpoint("https://api.openai.com/v1"))
        );
    }

    #[test]
    fn discovers_git_branch_name_from_workspace() {
        let workspace = tempdir().expect("workspace");
        fs::create_dir(workspace.path().join(".git")).expect("git dir");
        fs::write(workspace.path().join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");

        assert_eq!(
            workspace_branch_name(workspace.path()).as_deref(),
            Some("main")
        );
        assert_eq!(
            discover_git_root(workspace.path()).as_deref(),
            Some(workspace.path())
        );
        assert_eq!(
            discover_git_dir(workspace.path()).as_deref(),
            Some(workspace.path().join(".git").as_path())
        );
    }

    #[test]
    fn git_workspace_diff_is_colorized_and_unified() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        let home = tempdir().expect("home");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "one\ntwo\nthree\n").expect("write file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        fs::write(
            workspace.path().join("README.md"),
            "one\nchanged\nthree\nfour\n",
        )
        .expect("update file");

        let diff = git_workspace_diff(workspace.path()).expect("git diff");
        assert!(diff.contains("\u{1b}["));
        assert!(diff.contains("@@"));
        assert!(diff.contains("diff --git"));
        assert!(diff.contains("changed"));
        assert!(diff.contains("four"));
    }

    #[test]
    fn git_workspace_diff_honors_global_gitconfig() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "one\ntwo\nthree\n").expect("write file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        fs::write(
            workspace.path().join("README.md"),
            "one\nchanged\nthree\nfour\n",
        )
        .expect("update file");

        let home = tempdir().expect("home");
        fs::write(
            home.path().join(".gitconfig"),
            "[diff]\n\tnoprefix = true\n",
        )
        .expect("gitconfig");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());

        let diff = git_workspace_diff(workspace.path()).expect("git diff");
        assert!(diff.contains("diff --git README.md README.md"));
        assert!(diff.contains("--- README.md"));
        assert!(diff.contains("+++ README.md"));
        assert!(!diff.contains("diff --git a/README.md b/README.md"));
    }

    #[test]
    fn git_workspace_diff_uses_configured_noninteractive_pager() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "one\ntwo\nthree\n").expect("write file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        fs::write(
            workspace.path().join("README.md"),
            "one\nchanged\nthree\nfour\n",
        )
        .expect("update file");

        let home = tempdir().expect("home");
        let pager = home.path().join("pager.sh");
        fs::write(
            &pager,
            "#!/bin/sh\nprintf 'PAGER-START WIDTH=%s\\n' \"$COLUMNS\"\ncat\nprintf 'PAGER-END\\n'\n",
        )
        .expect("pager script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&pager).expect("pager metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&pager, permissions).expect("pager permissions");
        }
        fs::write(
            home.path().join(".gitconfig"),
            format!("[core]\n\tpager = {}\n", pager.display()),
        )
        .expect("gitconfig");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        let _columns_guard = EnvVarGuard::set_value("COLUMNS", "123");

        let diff = git_workspace_diff(workspace.path()).expect("git diff");
        assert!(diff.contains("PAGER-START WIDTH="));
        assert!(diff.contains("diff --git"));
        assert!(diff.ends_with("PAGER-END\n"));
    }

    #[test]
    fn adds_explicit_width_to_delta_pager_command() {
        assert_eq!(
            with_explicit_pager_width("delta --side-by-side", 123),
            "delta --side-by-side --width=123"
        );
        assert_eq!(
            with_explicit_pager_width("/usr/bin/delta --width=90 --side-by-side", 123),
            "/usr/bin/delta --width=90 --side-by-side"
        );
        assert_eq!(with_explicit_pager_width("less -FRX", 123), "less -FRX");
    }

    #[test]
    fn unknown_slash_commands_error_locally() {
        let workspace = tempdir().expect("workspace");
        let tools = ToolExecutor::new(workspace.path());
        let mut llms = HashMap::new();
        llms.insert(
            "default".to_string(),
            LlmConfiguration {
                provider: "openai".to_string(),
                model: "gpt-4.1".to_string(),
                endpoint: "http://localhost:11434/v1".to_string(),
                api_key: None,
                request_timeout_seconds: 30,
                max_tool_rounds: 10,
                system_prompt: String::new(),
            },
        );
        let mut session = ChatSession::new(system_prompt(&llms["default"]));
        let mut active_model = "default".to_string();
        let mut current_endpoint = Some("http://localhost:11434/v1".to_string());

        let outcome = handle_command(
            "/unknown",
            CommandState {
                active_model: &mut active_model,
                current_endpoint: &mut current_endpoint,
                session: &mut session,
            },
            CommandContext {
                startup_model: "default",
                startup_endpoint: "http://localhost:11434/v1",
                llms: &llms,
                tools: &tools,
                workspace: workspace.path(),
            },
        )
        .expect("command outcome");

        assert!(matches!(
            outcome,
            CommandOutcome::Output(ref message)
                if message == "Unknown command '/unknown'. Use /help to see available commands."
        ));
    }

    #[test]
    fn completes_open_file_commands_across_workspace() {
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join("README.md"), "").expect("root readme");
        fs::create_dir(workspace.path().join("doc")).expect("doc dir");
        fs::write(workspace.path().join("doc/README.md"), "").expect("doc readme");
        fs::create_dir(workspace.path().join("src")).expect("src dir");
        fs::write(workspace.path().join("src/tui.rs"), "").expect("src file");
        fs::create_dir_all(workspace.path().join("target/.fingerprint/pkg")).expect("target dir");
        fs::write(
            workspace.path().join("target/.fingerprint/pkg/tui-output"),
            "",
        )
        .expect("target file");
        fs::create_dir_all(workspace.path().join("build/out")).expect("build dir");
        fs::write(workspace.path().join("build/out/tui.txt"), "").expect("build file");
        fs::write(workspace.path().join(".gitignore"), "ignored.md\n").expect("gitignore");
        fs::write(workspace.path().join("ignored.md"), "").expect("ignored file");
        fs::create_dir(workspace.path().join(".git")).expect("git dir");
        fs::write(workspace.path().join(".git/config"), "").expect("git config");

        let (_, _, slash_candidates) = completion_candidates(
            "/open_file READ",
            "/open_file READ".len(),
            workspace.path(),
            &[],
        )
        .expect("slash completion");
        assert_eq!(
            slash_candidates,
            vec!["README.md".to_string(), "doc/README.md".to_string()]
        );

        let (start, _, natural_candidates) =
            completion_candidates("Open READ", "Open READ".len(), workspace.path(), &[])
                .expect("natural completion");
        assert_eq!(start, "Open ".len());
        assert_eq!(
            natural_candidates,
            vec!["README.md".to_string(), "doc/README.md".to_string()]
        );

        let (_, _, ignored_candidates) =
            completion_candidates("Open ign", "Open ign".len(), workspace.path(), &[])
                .expect("ignored completion");
        assert!(ignored_candidates.is_empty());

        let (_, _, git_candidates) =
            completion_candidates("Open con", "Open con".len(), workspace.path(), &[])
                .expect("git completion");
        assert!(git_candidates.is_empty());

        let (_, _, target_candidates) =
            completion_candidates("/open_file t", "/open_file t".len(), workspace.path(), &[])
                .expect("target completion");
        assert_eq!(target_candidates, vec!["src/tui.rs".to_string()]);

        let (start, _, show_candidates) =
            completion_candidates("Show t", "Show t".len(), workspace.path(), &[])
                .expect("show completion");
        assert_eq!(start, "Show ".len());
        assert_eq!(show_candidates, vec!["src/tui.rs".to_string()]);

        let (start, _, show_file_candidates) = completion_candidates(
            "show file READ",
            "show file READ".len(),
            workspace.path(),
            &[],
        )
        .expect("show file completion");
        assert_eq!(start, "show file ".len());
        assert_eq!(
            show_file_candidates,
            vec!["README.md".to_string(), "doc/README.md".to_string()]
        );
    }

    #[test]
    fn completes_show_file_commands_and_flags() {
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join("README.md"), "").expect("root readme");
        fs::create_dir(workspace.path().join("doc")).expect("doc dir");
        fs::write(workspace.path().join("doc/README.md"), "").expect("doc readme");
        fs::create_dir(workspace.path().join("src")).expect("src dir");
        fs::write(workspace.path().join("src/tui.rs"), "").expect("src file");
        fs::create_dir_all(workspace.path().join("target/.fingerprint/pkg")).expect("target dir");
        fs::write(
            workspace.path().join("target/.fingerprint/pkg/tui-output"),
            "",
        )
        .expect("target file");

        let (_, _, initial_file_candidates) =
            completion_candidates("/show_file ", "/show_file ".len(), workspace.path(), &[])
                .expect("initial file completion");
        assert_eq!(
            initial_file_candidates,
            vec![
                "README.md".to_string(),
                "doc/README.md".to_string(),
                "src/tui.rs".to_string()
            ]
        );

        let (_, _, flag_candidates) = completion_candidates(
            "/show_file --",
            "/show_file --".len(),
            workspace.path(),
            &[],
        )
        .expect("flag completion");
        assert_eq!(
            flag_candidates,
            vec!["--author".to_string(), "--hash".to_string()]
        );

        let (_, _, file_candidates) = completion_candidates(
            "/show_file --hash READ",
            "/show_file --hash READ".len(),
            workspace.path(),
            &[],
        )
        .expect("file completion");
        assert_eq!(
            file_candidates,
            vec!["README.md".to_string(), "doc/README.md".to_string()]
        );

        let (_, _, quoted_candidates) = completion_candidates(
            "/show_file \"READ",
            "/show_file \"READ".len(),
            workspace.path(),
            &[],
        )
        .expect("quoted file completion");
        assert_eq!(
            quoted_candidates,
            vec!["\"README.md".to_string(), "\"doc/README.md".to_string()]
        );

        let (_, _, target_candidates) =
            completion_candidates("/show_file t", "/show_file t".len(), workspace.path(), &[])
                .expect("target completion");
        assert_eq!(target_candidates, vec!["src/tui.rs".to_string()]);
    }

    #[test]
    fn completion_respects_repo_gitignore_when_workspace_is_ignored_subdir() {
        let repo = tempdir().expect("repo");
        fs::create_dir(repo.path().join(".git")).expect("git dir");
        fs::write(repo.path().join(".git/config"), "").expect("git config");
        fs::write(repo.path().join(".gitignore"), "target/\n").expect("gitignore");
        fs::create_dir_all(repo.path().join("target/debug/.fingerprint/pkg")).expect("target dir");
        fs::write(
            repo.path().join("target/debug/.fingerprint/pkg/tui-output"),
            "",
        )
        .expect("target file");

        let workspace = repo.path().join("target/debug");

        let (_, _, open_candidates) =
            completion_candidates("/open_file ", "/open_file ".len(), &workspace, &[])
                .expect("open completion");
        assert!(open_candidates.is_empty());

        let (_, _, show_candidates) =
            completion_candidates("/show_file ", "/show_file ".len(), &workspace, &[])
                .expect("show completion");
        assert!(show_candidates.is_empty());
    }

    #[test]
    fn show_file_outputs_line_numbers_and_syntax_highlighting() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        fs::write(
            workspace.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .expect("source file");

        let _path_guard = EnvVarGuard::set_value("PATH", "");
        let output = show_file_output(workspace.path(), "main.rs").expect("show file");
        assert!(output.contains("1 "));
        assert!(output.contains("2 "));
        assert!(output.contains("\u{1b}["));
        assert!(output.contains("println!"));
    }

    #[test]
    fn show_file_formatting_bounds_ansi_to_source_column() {
        let metadata = GitLineMetadata {
            hash: "deadbeef".to_string(),
            author: "Alice".to_string(),
        };

        let rendered = format_show_file_line(
            7,
            "\x1b[38;2;1;2;3mlet x = 1;",
            Some(&metadata),
            ShowFileOptions {
                show_hash: true,
                show_author: true,
            },
            2,
        );

        assert_eq!(
            rendered,
            format!(" 7 deadbeef Alice {ANSI_RESET}\x1b[38;2;1;2;3mlet x = 1;{ANSI_RESET}")
        );
    }

    #[test]
    fn show_file_can_include_git_hash_and_author() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "alpha\nbeta\n").expect("write file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        let hash_output = std::process::Command::new("git")
            .args(["rev-parse", "--short=8", "HEAD"])
            .current_dir(workspace.path())
            .output()
            .expect("git rev-parse");
        let expected_hash = String::from_utf8(hash_output.stdout)
            .expect("hash output")
            .trim()
            .to_string();

        let output =
            show_file_output(workspace.path(), "--hash --author README.md").expect("show file");
        assert!(output.contains(&expected_hash));
        assert!(output.contains("Orangu Tests"));
        assert!(output.contains("1 "));
        assert!(output.contains("2 "));
    }

    #[test]
    fn show_file_uses_bat_when_available_without_metadata_columns() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("source file");

        let tools_dir = tempdir().expect("tools dir");
        let bat = tools_dir.path().join("bat");
        fs::write(&bat, "#!/bin/sh\nprintf 'BAT:%s\\n' \"$*\"\n").expect("bat script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&bat).expect("bat metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&bat, permissions).expect("bat permissions");
        }
        let path_value = format!(
            "{}:{}",
            tools_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path_guard = EnvVarGuard::set_value("PATH", &path_value);
        let _columns_guard = EnvVarGuard::set_value("COLUMNS", "123");

        let output = show_file_output(workspace.path(), "main.rs").expect("show file");
        assert!(output.contains("BAT:"));
        assert!(output.contains("--paging=never"));
        assert!(output.contains("--color=always"));
        assert!(output.contains("--style=numbers"));
        assert!(output.contains("--terminal-width"));
        assert!(output.contains(workspace.path().join("main.rs").to_string_lossy().as_ref()));
    }

    #[test]
    fn show_file_bypasses_bat_when_metadata_columns_are_requested() {
        let _env_lock = lock_process_env();
        let workspace = tempdir().expect("workspace");
        init_test_git_repo(workspace.path());
        fs::write(workspace.path().join("README.md"), "alpha\nbeta\n").expect("write file");
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(workspace.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "initial"])
                .current_dir(workspace.path())
                .status()
                .expect("git commit")
                .success()
        );

        let tools_dir = tempdir().expect("tools dir");
        let bat = tools_dir.path().join("bat");
        fs::write(&bat, "#!/bin/sh\nprintf 'BAT:%s\\n' \"$*\"\n").expect("bat script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&bat).expect("bat metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&bat, permissions).expect("bat permissions");
        }
        let path_value = format!(
            "{}:{}",
            tools_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path_guard = EnvVarGuard::set_value("PATH", &path_value);

        let output = show_file_output(workspace.path(), "--hash README.md").expect("show file");
        assert!(!output.contains("BAT:"));
        assert!(output.contains("alpha"));
        assert!(output.contains("beta"));
    }

    #[test]
    fn splits_editor_command_and_flags() {
        assert_eq!(
            shell_words("code --wait").expect("editor command"),
            vec!["code".to_string(), "--wait".to_string()]
        );
        assert_eq!(
            shell_words("\"/tmp/my editor\" --flag").expect("quoted editor command"),
            vec!["/tmp/my editor".to_string(), "--flag".to_string()]
        );
    }

    #[test]
    fn final_pending_line_keeps_visible_output() {
        assert_eq!(
            final_pending_line("streamed reply", "final reply").as_deref(),
            Some("streamed reply")
        );
        assert_eq!(
            final_pending_line("", "final reply").as_deref(),
            Some("final reply")
        );
        assert_eq!(final_pending_line("", ""), None);
    }

    #[test]
    fn cancelled_output_preserves_partial_reply_and_uses_light_red_notice() {
        let mut output_state = OutputState::default();

        preserve_cancelled_output(&mut output_state, "partial reply");

        assert_eq!(
            output_state.lines(),
            &[
                TranscriptLine::Plain("partial reply".to_string()),
                TranscriptLine::Plain(request_cancelled_message()),
            ]
        );
    }

    #[test]
    fn idle_refresh_timeout_hits_zero_at_deadline() {
        let start = Instant::now();

        assert_eq!(
            idle_status_refresh_timeout(start + Duration::from_secs(60), start),
            Duration::from_secs(60)
        );
        assert_eq!(
            idle_status_refresh_timeout(
                start + Duration::from_secs(60),
                start + Duration::from_secs(61)
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn llm_prompt_block_reason_requires_model_connection() {
        assert_eq!(
            llm_prompt_block_reason(
                Some("http://localhost:8100/v1"),
                HeaderStatus {
                    workspace_ok: true,
                    server_ok: true,
                    model_ok: false,
                }
            ),
            None
        );
        assert_eq!(
            llm_prompt_block_reason(
                Some("http://localhost:8100/v1"),
                HeaderStatus {
                    workspace_ok: true,
                    server_ok: true,
                    model_ok: true,
                }
            ),
            None
        );
    }

    #[test]
    fn escape_cancel_requires_two_presses_within_timeout() {
        let mut cancel_state = EscapeCancelState::default();
        let start = Instant::now();

        assert!(!cancel_state.handle_escape(start));
        assert!(cancel_state.handle_escape(start + Duration::from_millis(500)));

        assert!(!cancel_state.handle_escape(start + Duration::from_secs(5)));
        assert!(!cancel_state.handle_escape(start + Duration::from_secs(8)));
    }

    #[test]
    fn wait_cancel_escape_only_matches_escape_press() {
        assert!(is_wait_cancel_escape(&Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE
        ))));
        assert!(!is_wait_cancel_escape(&Event::Key(
            KeyEvent::new_with_kind(KeyCode::Esc, KeyModifiers::NONE, KeyEventKind::Repeat)
        )));
        assert!(!is_wait_cancel_escape(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))));
    }

    #[test]
    fn llama_cpp_left_status_prefers_native_metrics() {
        let profile = LlmConfiguration {
            provider: "llama.cpp".to_string(),
            model: "model".to_string(),
            endpoint: "http://localhost:8080/v1".to_string(),
            api_key: None,
            request_timeout_seconds: 30,
            max_tool_rounds: 10,
            system_prompt: String::new(),
        };

        let thinking = render_left_status(
            &profile,
            "",
            &StreamMetrics {
                prompt_progress: Some(StreamPromptProgress {
                    total: 100,
                    cache: 20,
                    processed: 60,
                    time_ms: 2_000,
                }),
                prompt_per_second: Some(15.0),
                predicted_per_second: None,
            },
            Duration::from_secs(2),
            0,
            None,
        )
        .expect("thinking status");
        for ch in "Thinking".chars() {
            assert!(thinking.rendered.contains(ch));
        }
        assert!(thinking.rendered.contains("(2s)"));
        assert_eq!(thinking.visible_width, "Thinking (2s)".chars().count());

        let working = render_left_status(
            &profile,
            "hello",
            &StreamMetrics {
                prompt_progress: None,
                prompt_per_second: Some(15.0),
                predicted_per_second: Some(42.5),
            },
            Duration::from_secs(2),
            1,
            None,
        )
        .expect("working status");
        for ch in "Working".chars() {
            assert!(working.rendered.contains(ch));
        }
        assert!(working.rendered.contains("42.5 t/s"));
        assert!(working.rendered.contains("(2s)"));
        assert_eq!(
            working.visible_width,
            "Working @ 42.5 t/s (2s)".chars().count()
        );
    }
}
