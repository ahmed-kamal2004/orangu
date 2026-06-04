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
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command, Stdio},
    thread,
};
use tokio::sync::mpsc::UnboundedSender;

/// Sink for streaming build output. Each sent string is one line that the
/// caller appends to the output window as soon as it arrives.
pub type BuildSink = UnboundedSender<String>;

pub fn build_output(workspace: &Path, sink: &BuildSink) -> Result<()> {
    if workspace.join("Cargo.toml").exists() {
        rust_build(workspace, sink)
    } else if workspace.join("CMakeLists.txt").exists() {
        c_build(workspace, sink)
    } else if workspace.join("pom.xml").exists() {
        java_build(workspace, sink)
    } else {
        Err(anyhow!(
            "no supported project found (expected Cargo.toml, CMakeLists.txt, or pom.xml)"
        ))
    }
}

fn make_cmd(program: &str, args: &[&str], cwd: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd
}

/// Forward every line from a child pipe to the sink as it is produced.
fn stream_pipe<R: Read>(pipe: R, sink: &BuildSink) {
    let reader = BufReader::new(pipe);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if sink.send(line).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

struct BuildSteps<'a> {
    sink: &'a BuildSink,
    first: bool,
}

impl<'a> BuildSteps<'a> {
    fn new(sink: &'a BuildSink) -> Self {
        Self { sink, first: true }
    }

    fn emit(&self, line: impl Into<String>) {
        let _ = self.sink.send(line.into());
    }

    fn run(&mut self, label: &str, mut command: Command) -> Result<()> {
        if !self.first {
            self.emit("");
        }
        self.first = false;
        self.emit(format!("{label}:"));

        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to run {label}"))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_handle = stdout.map(|pipe| {
            let sink = self.sink.clone();
            thread::spawn(move || stream_pipe(pipe, &sink))
        });
        let err_handle = stderr.map(|pipe| {
            let sink = self.sink.clone();
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
            .with_context(|| format!("failed to wait for {label}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("{label} failed"))
        }
    }
}

fn rust_build(workspace: &Path, sink: &BuildSink) -> Result<()> {
    let mut steps = BuildSteps::new(sink);
    steps.run("cargo fmt", make_cmd("cargo", &["fmt"], workspace))?;
    steps.run("cargo clippy", make_cmd("cargo", &["clippy"], workspace))?;
    steps.run("cargo build", make_cmd("cargo", &["build"], workspace))?;
    steps.run("cargo test", make_cmd("cargo", &["test"], workspace))?;
    Ok(())
}

fn c_build(workspace: &Path, sink: &BuildSink) -> Result<()> {
    let mut steps = BuildSteps::new(sink);

    if workspace.join("clang-format.sh").exists() {
        steps.run(
            "clang-format.sh",
            make_cmd("bash", &["clang-format.sh"], workspace),
        )?;
    }

    let build_dir = workspace.join("build");
    if !build_dir.exists() {
        std::fs::create_dir(&build_dir)
            .with_context(|| format!("failed to create {}", build_dir.display()))?;
    }

    if !build_dir.join("CMakeCache.txt").exists() {
        steps.run("cmake", make_cmd("cmake", &[".."], &build_dir))?;
    }

    steps.run("make", make_cmd("make", &[], &build_dir))?;

    Ok(())
}

fn java_build(workspace: &Path, sink: &BuildSink) -> Result<()> {
    let mut steps = BuildSteps::new(sink);

    let frontend_dir = workspace.join("src").join("frontend");
    if frontend_dir.exists() {
        let needs_install = !frontend_dir
            .join("node_modules")
            .join(".package-lock.json")
            .exists()
            || is_newer(
                &frontend_dir.join("package.json"),
                &frontend_dir.join("node_modules").join(".package-lock.json"),
            )
            || is_newer(
                &frontend_dir.join("package-lock.json"),
                &frontend_dir.join("node_modules").join(".package-lock.json"),
            );

        if needs_install {
            steps.run(
                "npm ci",
                make_cmd("npm", &["--prefix", "src/frontend", "ci"], workspace),
            )?;
        }

        steps.run(
            "npm run fix",
            make_cmd(
                "npm",
                &["--prefix", "src/frontend", "run", "fix"],
                workspace,
            ),
        )?;

        steps.run(
            "npm run check",
            make_cmd(
                "npm",
                &["--prefix", "src/frontend", "run", "check"],
                workspace,
            ),
        )?;
    }

    steps.run("mvn package", make_cmd("mvn", &["package"], workspace))?;

    Ok(())
}

fn is_newer(a: &Path, b: &Path) -> bool {
    let Ok(a_meta) = a.metadata() else {
        return false;
    };
    let Ok(b_meta) = b.metadata() else {
        return true;
    };
    let Ok(a_time) = a_meta.modified() else {
        return false;
    };
    let Ok(b_time) = b_meta.modified() else {
        return true;
    };
    a_time > b_time
}
