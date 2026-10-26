//! Human-configured package projects. Plans contain argument arrays, never shell scripts.
use crate::model::{Tree, path, valid_id};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Manager {
    Npm,
    Cargo,
    Pip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub repo: String,
    pub name: String,
    pub manager: Manager,
    #[serde(default)]
    pub path: String,
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub offline: bool,
    pub requirements: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default = "scripts")]
    pub scripts: Vec<String>,
}
fn scripts() -> Vec<String> {
    ["build", "test", "lint"].map(String::from).to_vec()
}
impl Project {
    pub fn validate(&self) -> Result<()> {
        valid_id(&self.repo)?;
        valid_id(&self.name)?;
        if !self.path.is_empty() {
            path(&self.path)?;
        }
        if let Some(requirements) = &self.requirements {
            path(requirements)?;
        }
        for input in &self.inputs {
            path(input)?;
        }
        for script in &self.scripts {
            valid_id(script)?;
            ensure!(
                !script.starts_with('-'),
                "script name cannot start with '-'"
            );
        }
        if let Some(program) = &self.program {
            ensure!(!program.is_empty(), "empty package executable");
        }
        Ok(())
    }
    pub fn actions(&self) -> Vec<String> {
        let actions = match self.manager {
            Manager::Npm => vec!["install", "ci", "update", "clean"],
            Manager::Cargo => vec![
                "install",
                "build",
                "check",
                "test",
                "clippy",
                "format_check",
                "update",
                "clean",
            ],
            Manager::Pip => vec!["install", "check", "test", "build", "freeze", "clean"],
        };
        let mut actions: BTreeSet<String> = actions.into_iter().map(String::from).collect();
        if matches!(self.manager, Manager::Npm) {
            actions.extend(self.scripts.iter().cloned());
        }
        actions.into_iter().collect()
    }
    pub fn virtual_path(&self, relative: &str) -> String {
        if self.path.is_empty() {
            relative.into()
        } else {
            format!("{}/{relative}", self.path)
        }
    }
    pub fn scoped_inputs(&self, tree: &Tree) -> BTreeSet<String> {
        let candidates: &[&str] = match self.manager {
            Manager::Npm => &[
                "package.json",
                "package-lock.json",
                "npm-shrinkwrap.json",
                ".npmrc",
            ],
            Manager::Cargo => &["Cargo.toml", "Cargo.lock", ".cargo/config.toml"],
            Manager::Pip => &[
                "requirements.txt",
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
            ],
        };
        let mut paths: BTreeSet<String> = candidates.iter().map(|p| self.virtual_path(p)).collect();
        paths.extend(self.inputs.iter().map(|p| self.virtual_path(p)));
        if let Some(requirements) = &self.requirements {
            paths.insert(self.virtual_path(requirements));
        }
        if matches!(self.manager, Manager::Cargo) {
            paths.insert("Cargo.lock".into());
        }
        let prefix = if self.path.is_empty() {
            String::new()
        } else {
            format!("{}/", self.path)
        };
        for p in tree.keys().filter(|p| {
            p.starts_with(&prefix) && !p.split('/').any(|c| c == "node_modules" || c == ".venv")
        }) {
            let name = p.rsplit('/').next().unwrap_or("");
            let include = match self.manager {
                Manager::Npm => {
                    ["package.json", "package-lock.json", "npm-shrinkwrap.json"].contains(&name)
                }
                Manager::Cargo => name == "Cargo.toml",
                Manager::Pip => {
                    (name.starts_with("requirements") || name.starts_with("constraints"))
                        && (name.ends_with(".txt") || name.ends_with(".in"))
                }
            };
            if include {
                paths.insert(p.clone());
            }
        }
        paths
    }
}

#[derive(Debug, Clone)]
pub enum Step {
    Command { program: String, args: Vec<String> },
    Remove(PathBuf),
}
#[derive(Debug)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub environment: Option<PathBuf>,
    pub artifacts: PathBuf,
    pub installs: bool,
    pub clears: bool,
}
pub fn environment(home: &Path, project: &Project) -> PathBuf {
    home.join("environments")
        .join(&project.repo)
        .join(&project.name)
}
pub fn python_executable(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts/python.exe")
    } else {
        venv.join("bin/python")
    }
}

/// External tools can reject Windows NT verbatim paths (notably pip's venv checks).
/// Keep canonical paths for our own IO, but pass standard absolute paths to tools.
pub fn process_path(path: &Path) -> String {
    crate::git::path_argument(path)
        .to_string_lossy()
        .into_owned()
}

/// Dependency inputs include absent lockfiles so creation/deletion changes the fingerprint.
pub fn fingerprint(
    project: &Project,
    contents: &BTreeMap<String, Option<Vec<u8>>>,
) -> Result<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(&serde_json::to_vec(project)?);
    for (p, bytes) in contents {
        hash.update(&(p.len() as u64).to_le_bytes());
        hash.update(p.as_bytes());
        match bytes {
            Some(bytes) => {
                hash.update(&[1]);
                hash.update(&(bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
            None => {
                hash.update(&[0]);
            }
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub fn plan(
    project: &Project,
    action: &str,
    home: &Path,
    checkout: &Path,
    tree: &Tree,
    installed: bool,
) -> Result<Plan> {
    project.validate()?;
    ensure!(
        project.actions().iter().any(|a| a == action),
        "unknown package action: {action}; allowed: {:?}",
        project.actions()
    );
    let cwd = checkout.join(&project.path);
    let artifacts = home
        .join("artifacts")
        .join(&project.repo)
        .join(&project.name);
    let mut plan = Plan {
        steps: Vec::new(),
        env: project.env.clone(),
        cwd,
        environment: None,
        artifacts,
        installs: action == "install" || action == "ci" || action == "update",
        clears: action == "clean",
    };
    let default_program = match project.manager {
        Manager::Npm => {
            if cfg!(windows) {
                "npm.cmd"
            } else {
                "npm"
            }
        }
        Manager::Cargo => "cargo",
        Manager::Pip => {
            if cfg!(windows) {
                "python"
            } else {
                "python3"
            }
        }
    };
    let program = project
        .program
        .as_deref()
        .unwrap_or(default_program)
        .to_string();
    let mut args = project.args.clone();
    match project.manager {
        Manager::Npm => {
            if action == "clean" {
                plan.steps.push(Step::Remove(plan.cwd.join("node_modules")));
                return Ok(plan);
            }
            ensure!(
                plan.installs || installed,
                "npm dependencies are not prepared for these inputs; run package install/ci, then import any reported lockfile changes"
            );
            let cmd = if action == "install"
                && (tree.contains_key(&project.virtual_path("package-lock.json"))
                    || tree.contains_key(&project.virtual_path("npm-shrinkwrap.json")))
            {
                "ci"
            } else {
                action
            };
            if plan.installs {
                args.extend([cmd.to_string(), "--no-audit".into(), "--no-fund".into()]);
            } else {
                args.extend(["run".into(), action.into()]);
            }
            if project.offline {
                plan.env.insert("npm_config_offline".into(), "true".into());
            }
            plan.steps.push(Step::Command { program, args });
        }
        Manager::Cargo => {
            let target = home.join("targets").join(&project.repo).join(&project.name);
            plan.env
                .insert("CARGO_TARGET_DIR".into(), process_path(&target));
            if action == "clean" {
                plan.steps.push(Step::Remove(target));
                return Ok(plan);
            }
            args.push(
                match action {
                    "install" => "fetch",
                    "format_check" => "fmt",
                    other => other,
                }
                .into(),
            );
            if action == "format_check" {
                args.extend(["--all".into(), "--".into(), "--check".into()]);
            } else {
                if action != "update" {
                    args.push("--locked".into());
                }
                if project.offline {
                    args.push("--offline".into());
                }
            }
            plan.steps.push(Step::Command { program, args });
        }
        Manager::Pip => {
            let venv = environment(home, project);
            let python = python_executable(&venv);
            plan.environment = Some(venv.clone());
            if action == "clean" {
                plan.steps.push(Step::Remove(venv));
                return Ok(plan);
            }
            if action == "install" {
                if !installed && venv.exists() {
                    plan.steps.push(Step::Remove(venv.clone()));
                }
                if !installed || !python.is_file() {
                    args.extend([
                        "-m".into(),
                        "venv".into(),
                        "--copies".into(),
                        process_path(&venv),
                    ]);
                    plan.steps.push(Step::Command { program, args });
                }
            } else {
                ensure!(
                    installed && python.is_file(),
                    "Python environment is not prepared for these inputs; run package install first"
                );
            }
            plan.env.insert("VIRTUAL_ENV".into(), process_path(&venv));
            plan.env.insert("PYTHONNOUSERSITE".into(), "1".into());
            plan.env
                .insert("PIP_REQUIRE_VIRTUALENV".into(), "true".into());
            plan.env
                .insert("PIP_DISABLE_PIP_VERSION_CHECK".into(), "1".into());
            if project.offline {
                plan.env.insert("PIP_NO_INDEX".into(), "true".into());
            }
            let scripts = python.parent().context("invalid environment path")?;
            let original = plan
                .env
                .get("PATH")
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let paths = std::iter::once(PathBuf::from(process_path(scripts)))
                .chain(std::env::split_paths(&original));
            plan.env.insert(
                "PATH".into(),
                std::env::join_paths(paths)?.to_string_lossy().into(),
            );
            let mut pip = vec!["-m".into()];
            match action {
                "install" => {
                    pip.extend(["pip".into(), "install".into()]);
                    let requirements = project.requirements.clone().or_else(|| {
                        tree.contains_key(&project.virtual_path("requirements.txt"))
                            .then(|| "requirements.txt".into())
                    });
                    if let Some(file) = requirements {
                        pip.extend(["-r".into(), file]);
                    } else {
                        ensure!(
                            tree.contains_key(&project.virtual_path("pyproject.toml"))
                                || tree.contains_key(&project.virtual_path("setup.py")),
                            "pip project needs requirements.txt, configured requirements, pyproject.toml, or setup.py"
                        );
                        pip.extend(["-e".into(), ".".into()]);
                    }
                }
                "check" => pip.extend(["pip".into(), "check".into()]),
                "freeze" => pip.extend(["pip".into(), "freeze".into()]),
                "test" => pip.push("pytest".into()),
                "build" => pip.extend([
                    "pip".into(),
                    "wheel".into(),
                    "--no-deps".into(),
                    "--wheel-dir".into(),
                    process_path(&plan.artifacts),
                    ".".into(),
                ]),
                _ => unreachable!(),
            }
            plan.steps.push(Step::Command {
                program: process_path(&python),
                args: pip,
            });
        }
    }
    Ok(plan)
}
