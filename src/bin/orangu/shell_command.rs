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
use std::{path::Path, process::Stdio, thread};

use crate::build::{BuildSink, stream_pipe};

/// Runs `command_line` through `bash -lc` in `workspace`, streaming stdout and
/// stderr lines to `sink` as they arrive (see `/build`'s `build_output`, which
/// shares the same streaming plumbing). A login shell resolves the command
/// against the user's full `$PATH`, so `/shell` can run anything the user
/// could type at their own terminal, not just a fixed allow-list.
pub fn shell_output(workspace: &Path, command_line: &str, sink: &BuildSink) -> Result<()> {
    let mut command = std::process::Command::new("bash");
    command
        .arg("-lc")
        .arg(command_line)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run: {command_line}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_handle = stdout.map(|pipe| {
        let sink = sink.clone();
        thread::spawn(move || stream_pipe(pipe, &sink))
    });
    let err_handle = stderr.map(|pipe| {
        let sink = sink.clone();
        thread::spawn(move || stream_pipe(pipe, &sink))
    });
    if let Some(handle) = out_handle {
        let _ = handle.join();
    }
    if let Some(handle) = err_handle {
        let _ = handle.join();
    }

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for: {command_line}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("command exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::shell_output;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn shell_output_streams_stdout_lines() {
        let (tx, mut rx) = unbounded_channel::<String>();
        let workspace = std::env::temp_dir();
        shell_output(&workspace, "echo hello", &tx).expect("echo succeeds");
        drop(tx);

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        assert_eq!(lines, vec!["hello".to_string()]);
    }

    #[test]
    fn shell_output_streams_stderr_lines_too() {
        let (tx, mut rx) = unbounded_channel::<String>();
        let workspace = std::env::temp_dir();
        shell_output(&workspace, "echo oops 1>&2", &tx).expect("echo succeeds");
        drop(tx);

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        assert_eq!(lines, vec!["oops".to_string()]);
    }

    #[test]
    fn shell_output_runs_in_the_given_workspace() {
        let (tx, mut rx) = unbounded_channel::<String>();
        let workspace = std::env::temp_dir();
        shell_output(&workspace, "pwd", &tx).expect("pwd succeeds");
        drop(tx);

        let line = rx.try_recv().expect("pwd printed a line");
        // Canonicalize both sides: on macOS `/tmp` is itself a symlink to
        // `/private/tmp`, which `pwd` resolves but `temp_dir()` may not.
        let printed = std::fs::canonicalize(line.trim()).unwrap_or_else(|_| line.trim().into());
        let expected = std::fs::canonicalize(&workspace).unwrap_or(workspace);
        assert_eq!(printed, expected);
    }

    #[test]
    fn shell_output_errors_on_nonzero_exit() {
        let (tx, _rx) = unbounded_channel::<String>();
        let workspace = std::env::temp_dir();
        let err = shell_output(&workspace, "exit 3", &tx).expect_err("nonzero exit is an error");
        assert!(err.to_string().contains("exit"), "{err}");
    }
}
