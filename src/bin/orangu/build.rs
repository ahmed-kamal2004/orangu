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
    fmt::Write as _,
    path::Path,
    process::{Command, Output},
};

pub fn build_output(workspace: &Path) -> Result<String> {
    if workspace.join("Cargo.toml").exists() {
        rust_build(workspace)
    } else if workspace.join("CMakeLists.txt").exists() {
        c_build(workspace)
    } else if workspace.join("pom.xml").exists() {
        java_build(workspace)
    } else {
        Err(anyhow!(
            "no supported project found (expected Cargo.toml, CMakeLists.txt, or pom.xml)"
        ))
    }
}

fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    [stdout, stderr]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_cmd(program: &str, args: &[&str], cwd: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd
}

struct BuildSteps {
    buf: String,
}

impl BuildSteps {
    fn new() -> Self {
        Self { buf: String::new() }
    }

    fn run(&mut self, label: &str, mut command: Command) -> Result<()> {
        let output = command
            .output()
            .with_context(|| format!("failed to run {label}"))?;
        let detail = combined_output(&output);
        if !self.buf.is_empty() {
            self.buf.push('\n');
        }
        if output.status.success() {
            if detail.is_empty() {
                let _ = write!(self.buf, "{label}: ok");
            } else {
                let _ = write!(self.buf, "{label}:\n{detail}");
            }
            Ok(())
        } else {
            if detail.is_empty() {
                let _ = write!(self.buf, "{label}: failed");
            } else {
                let _ = write!(self.buf, "{label}:\n{detail}");
            }
            Err(anyhow!("{}", self.buf))
        }
    }

    fn finish(self) -> String {
        self.buf
    }
}

fn rust_build(workspace: &Path) -> Result<String> {
    let mut steps = BuildSteps::new();
    steps.run("cargo fmt", make_cmd("cargo", &["fmt"], workspace))?;
    steps.run("cargo clippy", make_cmd("cargo", &["clippy"], workspace))?;
    steps.run("cargo build", make_cmd("cargo", &["build"], workspace))?;
    steps.run("cargo test", make_cmd("cargo", &["test"], workspace))?;
    Ok(steps.finish())
}

fn c_build(workspace: &Path) -> Result<String> {
    let mut steps = BuildSteps::new();

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

    Ok(steps.finish())
}

fn java_build(workspace: &Path) -> Result<String> {
    let mut steps = BuildSteps::new();

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

    Ok(steps.finish())
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
