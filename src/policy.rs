//! A small misuse detector, not a shell sandbox. Unrecognized syntax is allowed:
//! restricting reads or general scripting would defeat the host-tool workflow.
use serde_json::{Value, json};

const SHELL_TOOLS: &str = "shell|execute|run_command";

pub fn matcher() -> String {
    format!("(?i)^(?:(?:developer|terminal)__)?(?:{SHELL_TOOLS})$")
}

pub fn hook(payload: &Value) -> Value {
    let name = payload["tool_name"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = name
        .strip_prefix("developer__")
        .or_else(|| name.strip_prefix("terminal__"))
        .unwrap_or(&name);
    if !SHELL_TOOLS.split('|').any(|tool| tool == name) {
        return json!({"decision":"allow"});
    }
    let input = &payload["tool_input"];
    let command = input["command"].as_str().or_else(|| input["cmd"].as_str());
    if command.is_some_and(|command| creates_repository(command, 0)) {
        json!({"decision":"block","reason":"Direct Git repository creation leaves unmanaged files on disk. Use vwork_repo_clone/repo_register and virtual workspaces. For filesystem tools use workspace_temporary and release it when finished; workspace_export is available for an intentional indefinite export. Reads, host-file edits and general scripts remain allowed."})
    } else {
        json!({"decision":"allow"})
    }
}

fn creates_repository(command: &str, depth: usize) -> bool {
    if depth > 3 {
        return false;
    }
    let Some(statements) = simple_statements(command) else {
        return false;
    };
    statements.into_iter().any(|statement| {
        shlex::split(statement).is_some_and(|words| creates_from_words(&words, depth))
    })
}

fn creates_from_words(words: &[String], depth: usize) -> bool {
    if depth > 3 {
        return false;
    }
    let Some(program) = words.first() else {
        return false;
    };
    let program = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let program = program.strip_suffix(".exe").unwrap_or(&program);
    let args = &words[1..];
    if matches!(program, "sh" | "bash" | "zsh" | "pwsh" | "powershell") {
        return args.windows(2).any(|pair| {
            matches!(
                pair[0].to_ascii_lowercase().as_str(),
                "-c" | "-lc" | "-command"
            ) && creates_repository(&pair[1], depth + 1)
        });
    }
    if program == "cmd" {
        return args
            .iter()
            .position(|arg| arg.eq_ignore_ascii_case("/c"))
            .is_some_and(|i| {
                let rest = &args[i + 1..];
                if rest.len() == 1 {
                    creates_repository(&rest[0], depth + 1)
                } else {
                    creates_from_words(rest, depth + 1)
                }
            });
    }
    if program != "git"
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return false;
    }
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-v" | "--exec-path" | "--html-path" | "--man-path" | "--info-path" => {
                return false;
            }
            arg if arg.starts_with("--list-cmds=") => return false,
            // Global options whose next word is data, not a subcommand.
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                args.next();
            }
            arg if arg.starts_with('-') => {}
            "clone" | "init" => return true,
            "worktree" => return args.next().is_some_and(|arg| arg == "add"),
            _ => return false,
        }
    }
    false
}

/// Split ordinary shell command chains without treating quoted examples as
/// commands. This deliberately does not interpret scripts, substitutions,
/// here-documents or shell grammar; unknown constructs are left to prompting.
fn simple_statements(command: &str) -> Option<Vec<&str>> {
    let mut statements = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    for (i, c) in command.char_indices() {
        if comment {
            if c == '\n' {
                start = i + 1;
                comment = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else {
            match c {
                '\'' | '"' => quote = Some(c),
                // Ambiguous syntax: don't turn data in a script into a denial.
                '`' | '$' | '(' | ')' | '{' | '}' | '<' => return None,
                '#' if i == 0 || command[..i].ends_with(char::is_whitespace) => {
                    statements.push(&command[start..i]);
                    comment = true;
                }
                ';' | '&' | '|' | '\n' => {
                    statements.push(&command[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            }
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if !comment {
        statements.push(&command[start..]);
    }
    Some(statements)
}
