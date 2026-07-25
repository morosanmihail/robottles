use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use super::AgentRunner;
use crate::config::LmstudioConfig;
use crate::git::print_git_status;

const SYSTEM_PROMPT: &str = "You are a coding agent working inside a git checkout of a software \
project. You have tools to read files, write files, list directories and run shell commands, all \
rooted at the project directory (use paths relative to it, never absolute paths or '..'). Use them \
to inspect the existing code, make the changes needed to complete the user's task, and add or \
update tests where sensible. When you are done, reply with a plain text message and no further \
tool calls summarizing what you did.";

/// Runs a task's prompt through a local LM Studio server, using its
/// OpenAI-compatible `/chat/completions` endpoint with tool-calling to read
/// and write files and run shell commands in the project checkout.
pub struct LmstudioRunner {
    config: LmstudioConfig,
}

impl LmstudioRunner {
    pub fn new(config: LmstudioConfig) -> Self {
        Self { config }
    }
}

impl AgentRunner for LmstudioRunner {
    fn run(&self, project_dir: &Path, prompt: &str) -> anyhow::Result<()> {
        println!(
            "Starting LM Studio session against {} in {}",
            self.config.base_url,
            project_dir.display()
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(self.config.timeout_secs))
            .build()
            .context("building HTTP client for LM Studio")?;
        let mut messages = vec![
            json!({"role": "system", "content": SYSTEM_PROMPT}),
            json!({"role": "user", "content": prompt}),
        ];

        for _ in 0..self.config.max_iterations {
            let response = chat_completion(&client, &self.config, &messages)?;
            let message = response
                .get("choices")
                .and_then(|choices| choices.get(0))
                .and_then(|choice| choice.get("message"))
                .cloned()
                .context("LM Studio response missing choices[0].message")?;

            let tool_calls: Vec<Value> = message
                .get("tool_calls")
                .and_then(|tool_calls| tool_calls.as_array())
                .cloned()
                .unwrap_or_default();
            messages.push(message.clone());

            if tool_calls.is_empty() {
                if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                    println!("LM Studio finished: {content}");
                }
                print_git_status(project_dir);
                return Ok(());
            }

            for call in tool_calls {
                let id = call.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let name = call
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let arguments_str = call
                    .pointer("/function/arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let arguments: Value = serde_json::from_str(arguments_str).unwrap_or(json!({}));
                let result = execute_tool(project_dir, name, &arguments);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": result,
                }));
            }
        }

        bail!(
            "LM Studio did not finish within {} tool-calling iterations",
            self.config.max_iterations
        )
    }
}

fn chat_completion(
    client: &reqwest::blocking::Client,
    config: &LmstudioConfig,
    messages: &[Value],
) -> anyhow::Result<Value> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));

    let mut body = json!({
        "messages": messages,
        "tools": tool_definitions(),
    });
    if let Some(model) = &config.model {
        body["model"] = json!(model);
    }

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .with_context(|| format!("calling LM Studio at {url} — is the LM Studio server running?"))?;

    let status = response.status();
    let text = response.text().context("reading LM Studio response body")?;
    if !status.is_success() {
        bail!("LM Studio API returned {status} for {url}: {text}");
    }

    serde_json::from_str(&text).context("parsing LM Studio response as JSON")
}

/// OpenAI-style tool definitions describing the filesystem/shell tools the
/// model can call.
fn tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a file in the project directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path relative to the project root."}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create or overwrite a file in the project directory with the given contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path relative to the project root."},
                        "contents": {"type": "string", "description": "Full contents to write to the file."}
                    },
                    "required": ["path", "contents"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "List files and directories under a path in the project directory (non-recursive).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path relative to the project root; defaults to \".\"."}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a shell command in the project directory and return its exit code, stdout and stderr.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to run."}
                    },
                    "required": ["command"]
                }
            }
        }
    ])
}

/// Dispatch a single tool call by name, returning the text to send back to
/// the model as the tool's result (including on error — the model gets to
/// see and react to failures rather than the run aborting).
fn execute_tool(project_dir: &Path, name: &str, arguments: &Value) -> String {
    match name {
        "read_file" => {
            let Some(path) = arguments.get("path").and_then(|v| v.as_str()) else {
                return "error: missing 'path' argument".to_string();
            };
            match resolve_path(project_dir, path) {
                Ok(resolved) => std::fs::read_to_string(&resolved)
                    .unwrap_or_else(|e| format!("error reading {path}: {e}")),
                Err(e) => e,
            }
        }
        "write_file" => {
            let (Some(path), Some(contents)) = (
                arguments.get("path").and_then(|v| v.as_str()),
                arguments.get("contents").and_then(|v| v.as_str()),
            ) else {
                return "error: missing 'path' or 'contents' argument".to_string();
            };
            match resolve_path(project_dir, path) {
                Ok(resolved) => write_file(&resolved, contents)
                    .unwrap_or_else(|e| format!("error writing {path}: {e}")),
                Err(e) => e,
            }
        }
        "list_files" => {
            let path = arguments.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match resolve_path(project_dir, path) {
                Ok(resolved) => list_files(&resolved)
                    .unwrap_or_else(|e| format!("error listing {path}: {e}")),
                Err(e) => e,
            }
        }
        "run_command" => {
            let Some(command) = arguments.get("command").and_then(|v| v.as_str()) else {
                return "error: missing 'command' argument".to_string();
            };
            run_command(project_dir, command)
        }
        other => format!("error: unknown tool '{other}'"),
    }
}

fn write_file(resolved: &Path, contents: &str) -> std::io::Result<String> {
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(resolved, contents)?;
    Ok(format!("wrote {} bytes to {}", contents.len(), resolved.display()))
}

fn list_files(resolved: &Path) -> std::io::Result<String> {
    let mut names: Vec<String> = std::fs::read_dir(resolved)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    Ok(names.join("\n"))
}

fn run_command(project_dir: &Path, command: &str) -> String {
    match Command::new("sh").arg("-c").arg(command).current_dir(project_dir).output() {
        Ok(output) => format!(
            "exit code: {}\nstdout:\n{}\nstderr:\n{}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(e) => format!("error running command: {e}"),
    }
}

/// Resolve `path` (as given by the model) against `project_dir`, rejecting
/// absolute paths and `..` components so the model can't read or write
/// outside the project checkout.
fn resolve_path(project_dir: &Path, path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err(format!("error: path '{path}' must be relative to the project root"));
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("error: path '{path}' may not contain '..'"));
    }
    Ok(project_dir.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_joins_relative_paths() {
        let resolved = resolve_path(Path::new("/tmp/project"), "src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/project/src/main.rs"));
    }

    #[test]
    fn resolve_path_rejects_absolute_paths() {
        let err = resolve_path(Path::new("/tmp/project"), "/etc/passwd").unwrap_err();
        assert!(err.contains("must be relative"));
    }

    #[test]
    fn resolve_path_rejects_parent_dir_components() {
        let err = resolve_path(Path::new("/tmp/project"), "../outside.txt").unwrap_err();
        assert!(err.contains("may not contain '..'"));
    }

    #[test]
    fn execute_tool_write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let write_result = execute_tool(
            dir.path(),
            "write_file",
            &json!({"path": "notes/todo.txt", "contents": "hello world"}),
        );
        assert!(write_result.starts_with("wrote"));

        let read_result = execute_tool(dir.path(), "read_file", &json!({"path": "notes/todo.txt"}));
        assert_eq!(read_result, "hello world");
    }

    #[test]
    fn execute_tool_list_files_lists_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();

        let result = execute_tool(dir.path(), "list_files", &json!({"path": "."}));
        assert_eq!(result, "a.txt\nb.txt");
    }

    #[test]
    fn execute_tool_run_command_captures_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute_tool(dir.path(), "run_command", &json!({"command": "echo hi"}));
        assert!(result.contains("exit code: 0"));
        assert!(result.contains("stdout:\nhi"));
    }

    #[test]
    fn execute_tool_rejects_escaping_paths() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute_tool(dir.path(), "read_file", &json!({"path": "../secret.txt"}));
        assert!(result.contains("may not contain '..'"));
    }

    #[test]
    fn execute_tool_unknown_tool_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute_tool(dir.path(), "delete_everything", &json!({}));
        assert_eq!(result, "error: unknown tool 'delete_everything'");
    }
}
