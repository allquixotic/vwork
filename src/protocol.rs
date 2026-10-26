use serde_json::{Value, json};

pub const INSTRUCTIONS: &str = "Work virtually in vwork. Use repo_list and reuse registrations. For user-requested setup, repo_register imports committed state from an absolute repository path; repo_clone imports a selected URL into redb. All persistent Git objects, refs and source contents are in redb; no permanent source checkout. Use files_read/list/glob/search and workspace_info/status/diff/history instead of native filesystem or shell. Get the current revision before editing and pass expected_revision; reconcile conflicts. files_apply_patch accepts full-file replacements/deletions. Use package_list/run for configured npm/Cargo/pip/uv/CDK actions and workspace_run_task for other configured tools. External tools need files: package/task runs automatically use a tracked temporary copy and return its lease, expiry, exact revision, exit status and source_changes. Only request workspace_temporary when a specific filesystem-only tool cannot do its work virtually; explain why in purpose. Import wanted edits explicitly, then call temporary_release as soon as finished. Do not retain copies just in case. Use temporary_list to find forgotten copies and cleanup errors. Renew a lease before expiry if a tool still needs it. Expiry/startup cleanup saves bounded nonignored source changes into a separate recovery workspace and deletes the copy; dependencies, environments and build outputs are disposable. Save wanted artifacts before releasing. Never use discard_changes unless intentionally discarding those edits. Cleanup failures stay visible and block unlimited accumulation. Use workspace_export only for durable files the user explicitly wants to keep. Never claim success unless the tool reports success. Use storage_info for shared storage accounting; never create probe workspaces to measure sharing. Git objects deduplicate daemon-wide by object ID; edited blobs deduplicate by content hash. Base manifests share within a registration and commit. Forks share immutable history. Tasks, package configurations and global maintenance remain human CLI operations. CDK supports local synth/list actions, not deployments.";

pub fn tools(bound: bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut add = |op: &str, desc: &str, fields: Vec<(&str, &str, bool)>, read: bool| {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, kind, req) in fields {
            let schema = match kind {
                "edits" => {
                    json!({"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"base64":{"type":"string"},"mode":{"type":"string","enum":["100644","100755","120000"]},"delete":{"type":"boolean"}},"required":["path"],"additionalProperties":false}})
                }
                "strings" => json!({"type":"array","items":{"type":"string"}}),
                "integer" => json!({"type":"integer","minimum":0}),
                _ => json!({"type":kind}),
            };
            props.insert(name.into(), schema);
            if req && !(bound && name == "workspace") {
                required.push(name);
            }
        }
        out.push(json!({"name":format!("vwork_{op}"),"description":desc,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":read,"destructiveHint":!read,"idempotentHint":read,"openWorldHint":op == "repo_clone"}}));
    };
    let w = ("workspace", "string", true);
    let e = ("expected_revision", "string", true);
    let p = ("path", "string", true);
    let lim = ("limit", "integer", false);
    let off = ("offset", "integer", false);
    add(
        "storage_info",
        "Read daemon-wide shared storage counters and deduplication scopes, including base manifests, compressed blobs and workspace/revision metadata. No probe workspace or native CLI needed; includes retained/uncollected records, not per-workspace disk allocation.",
        vec![],
        true,
    );
    add(
        "repo_list",
        "List registered Git repositories. Reuse an existing registration for additional workspaces before cloning another copy.",
        vec![],
        true,
    );
    add(
        "repo_register",
        "Import all Git objects, refs and committed source from an absolute existing repository path into redb under a new ID. Original checkout/index/refs are preserved; later source changes require a new import. Reuse this registration across virtual workspaces.",
        vec![("repo", "string", true), ("path", "string", true)],
        false,
    );
    add(
        "repo_clone",
        "Clone a user-selected Git source into redb under a new repository ID. All persistent objects and refs stay in redb; temporary transfer packs are cleaned after import/restart. Accepts HTTPS/SSH/Git/file URLs, SSH host:path or an absolute local repository path. No URL passwords/query tokens or arbitrary Git arguments. Then create virtual workspaces from HEAD or registered branch names.",
        vec![("repo", "string", true), ("source", "string", true)],
        false,
    );
    add(
        "repo_log",
        "Read actual Git commit history without a checkout. Virtual edit history is workspace_history.",
        vec![
            ("repo", "string", true),
            ("reference", "string", false),
            lim,
        ],
        true,
    );
    add(
        "workspace_list",
        "List virtual workspaces and their revision ids.",
        vec![],
        true,
    );
    add(
        "workspace_create",
        "Create a virtual workspace from a registered repository and Git reference, without checking out files.",
        vec![w, ("repo", "string", true), ("reference", "string", false)],
        false,
    );
    add(
        "workspace_fork",
        "Create an independent workspace sharing this workspace's immutable snapshot.",
        vec![w, ("name", "string", true)],
        false,
    );
    add(
        "workspace_info",
        "Get current revision, base commit and file count. Read this before mutations.",
        vec![w],
        true,
    );
    add(
        "workspace_size",
        "Rank virtual directories by logical byte size without extracting contents. For shared physical storage and deduplication scopes, use vwork_storage_info.",
        vec![w, lim, off],
        true,
    );
    add(
        "files_list",
        "List virtual files and sizes under an optional relative directory. Pagination uses offset/limit.",
        vec![
            w,
            ("path", "string", false),
            ("glob", "string", false),
            lim,
            off,
        ],
        true,
    );
    add(
        "files_glob",
        "Find virtual paths by glob (for example **/*.rs). Replaces find and rg --files.",
        vec![w, ("glob", "string", true), lim, off],
        true,
    );
    add(
        "files_stat",
        "Inspect virtual file metadata without materializing files.",
        vec![w, p, lim, off],
        true,
    );
    add(
        "files_read",
        "Read UTF-8 lines from virtual source, or base64 bytes with binary=true. Returns revision and content hash.",
        vec![
            w,
            p,
            ("start_line", "integer", false),
            ("line_count", "integer", false),
            ("binary", "boolean", false),
        ],
        true,
    );
    add(
        "files_search",
        "Regex search virtual contents, respecting edits and deletions. Replaces grep/rg. Returns line numbers and context; no filesystem export.",
        vec![
            w,
            ("pattern", "string", true),
            ("glob", "string", false),
            ("ignore_case", "boolean", false),
            ("context", "integer", false),
            lim,
            off,
        ],
        true,
    );
    add(
        "files_write",
        "Write UTF-8 content OR base64 bytes into a virtual file; expected_revision prevents lost updates.",
        vec![
            w,
            e,
            p,
            ("content", "string", false),
            ("base64", "string", false),
            ("mode", "string", false),
        ],
        false,
    );
    add(
        "files_replace",
        "Replace exact text in a virtual UTF-8 file. occurrences defaults to 1 and must match exactly.",
        vec![
            w,
            e,
            p,
            ("before", "string", true),
            ("after", "string", true),
            ("occurrences", "integer", false),
        ],
        false,
    );
    add(
        "files_apply_patch",
        "Atomically replace/delete multiple virtual files. edits contains {path,content} or {path,base64} or {path,delete:true}; not unified diff syntax.",
        vec![w, e, ("edits", "edits", true), ("message", "string", false)],
        false,
    );
    add(
        "files_delete",
        "Delete a virtual file, or subtree with recursive=true. Does not delete original Git files.",
        vec![w, e, p, ("recursive", "boolean", false)],
        false,
    );
    for op in ["files_copy", "files_rename"] {
        add(
            op,
            "Copy or rename a virtual file without copying its content blob. Destination must not exist.",
            vec![w, e, p, ("destination", "string", true)],
            false,
        );
    }
    for op in ["workspace_status", "workspace_diff"] {
        add(
            op,
            "Compare virtual content to its base Git commit, or against_revision. Diff output is bounded.",
            vec![w, ("against_revision", "string", false), lim, off],
            true,
        );
    }
    add(
        "workspace_history",
        "Read immutable virtual edit history; distinct from Git commit history.",
        vec![w, lim, off],
        true,
    );
    add(
        "workspace_restore",
        "Restore an older revision as a new edit, preserving history.",
        vec![w, e, ("revision", "string", true)],
        false,
    );
    add(
        "workspace_merge",
        "Three-way merge another virtual workspace. Conflicts leave destination unchanged.",
        vec![w, e, ("source", "string", true)],
        false,
    );
    add(
        "workspace_commit",
        "Store exact virtual snapshot in redb as a real Git commit under refs/vwork/commits. Does not change original branch/index or push.",
        vec![w, e, ("message", "string", true)],
        false,
    );
    add(
        "workspace_temporary",
        "Create a tracked temporary filesystem copy only for a tool that requires physical files. Give the reason; use virtual tools otherwise. Returns lease ID/path/expiry. Import wanted edits, save wanted artifacts, then release immediately after use. Expiry automatically reclaims it with bounded source recovery.",
        vec![
            w,
            e,
            ("purpose", "string", true),
            ("ttl_seconds", "integer", false),
        ],
        false,
    );
    add(
        "temporary_list",
        "Inspect active copies, expiry, cleanup failures and recent recovery workspaces. Use this to release forgotten copies.",
        vec![],
        true,
    );
    add(
        "temporary_renew",
        "Extend an existing ready copy while a filesystem tool still needs it. Maximum TTL is configured by the human; release promptly afterward.",
        vec![("lease", "string", true), ("ttl_seconds", "integer", false)],
        false,
    );
    add(
        "temporary_release",
        "Remove an owned temporary copy and all its dependencies/artifacts. Default saves unimported source edits in a separate recovery workspace. Import desired changes first. discard_changes=true intentionally discards remaining edits. Active vwork jobs prevent removal.",
        vec![
            ("lease", "string", true),
            ("discard_changes", "boolean", false),
        ],
        false,
    );
    add(
        "workspace_export",
        "Explicitly materialize files to an empty absolute destination or a managed export. Native filesystem access; only use when needed.",
        vec![
            w,
            ("destination", "string", true),
            ("glob", "string", false),
        ],
        false,
    );
    add(
        "workspace_import",
        "Import explicitly named edited/deleted files from this workspace's managed export. Unknown files are never imported automatically.",
        vec![w, e, ("source", "string", true), ("paths", "strings", true)],
        false,
    );
    add(
        "package_list",
        "List human-configured npm, Cargo, pip, uv and CDK projects and their allowed actions.",
        vec![("repo", "string", true)],
        true,
    );
    add(
        "package_run",
        "Run a configured package action in the serialized build checkout. Returns manifest/lockfile changes for explicit workspace_import; never mutates virtual source automatically. Pass lease to use an existing temporary copy for this workspace; import wanted physical edits before it is prepared again.",
        vec![
            w,
            e,
            ("project", "string", true),
            ("action", "string", true),
            ("lease", "string", false),
        ],
        false,
    );
    add(
        "task_list",
        "List human-configured native build/test tasks for a repository.",
        vec![("repo", "string", true)],
        true,
    );
    add(
        "workspace_run_task",
        "Materialize a pinned virtual revision into the repository's serialized build checkout, run a configured task and return bounded output and tested revision. Optionally pass an existing lease for this workspace to avoid another copy; import wanted edits first because preparation restores the requested virtual snapshot.",
        vec![w, e, ("task", "string", true), ("lease", "string", false)],
        false,
    );
    add(
        "workspace_drop",
        "Drop this workspace reference. Other forks and Git commits remain intact; physical exports are not removed.",
        vec![w, e],
        false,
    );
    out
}

const NATIVE_TOOLS: &[&str] = &[
    "shell",
    "write",
    "edit",
    "tree",
    "text_editor",
    "read_file",
    "write_file",
    "edit_file",
    "list_directory",
    "search_files",
    "execute",
    "run_command",
];
const NATIVE_PREFIXES: &[&str] = &["developer__", "filesystem__", "terminal__"];

/// Goose applies this regex before starting a command hook. Share the blocked
/// families with the hook so virtual tool calls don't pay for a shell process.
pub fn hook_matcher() -> String {
    let exact = NATIVE_TOOLS
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    let prefixes = NATIVE_PREFIXES
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    format!("(?i)^(?:(?:{exact})$|(?:{prefixes}))")
}

pub fn hook(payload: &Value) -> Value {
    let name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let lower = name.to_lowercase();
    let blocked = !lower.starts_with("vwork_")
        && (NATIVE_TOOLS.contains(&lower.as_str())
            || NATIVE_PREFIXES
                .iter()
                .any(|prefix| lower.starts_with(prefix)));
    if blocked {
        json!({"decision":"block","reason":"This session uses virtual source. Use vwork_files_* and vwork_workspace_* tools; native tools see a different filesystem. Use vwork_package_list/run for configured npm/Cargo/pip actions, or vwork_workspace_run_task for other builds. Import needed source_changes explicitly before the next action."})
    } else {
        json!({"decision":"allow"})
    }
}

pub fn mcp_response(
    request: &Value,
    bound: Option<&str>,
    mut invoke: impl FnMut(&str, &Value) -> anyhow::Result<Value>,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":match params["protocolVersion"].as_str(){Some("2024-11-05")=>"2024-11-05",Some("2025-03-26")=>"2025-03-26",_=>"2025-06-18"},"capabilities":{"tools":{"listChanged":false},"prompts":{}},"serverInfo":{"name":"vwork","version":env!("CARGO_PKG_VERSION")},"instructions":INSTRUCTIONS})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":tools(bound.is_some())}),
        "prompts/list" => {
            json!({"prompts":[{"name":"virtual-workspace","description":"How to work in a vwork workspace"}]})
        }
        "prompts/get" if params["name"] == "virtual-workspace" => {
            json!({"messages":[{"role":"user","content":{"type":"text","text":INSTRUCTIONS}}]})
        }
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            if !tools(bound.is_some()).iter().any(|t| t["name"] == name) {
                return Some(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"unknown vwork tool"}}),
                );
            }
            let op = name.strip_prefix("vwork_").unwrap();
            let mut args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !args.is_object() {
                return Some(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"arguments must be object"}}),
                );
            }
            let schema = tools(bound.is_some())
                .into_iter()
                .find(|t| t["name"] == name)
                .unwrap();
            if schema["inputSchema"]["properties"]
                .get("workspace")
                .is_some()
                && args.get("workspace").is_none()
                && let Some(ws) = bound
            {
                args["workspace"] = json!(ws);
            }
            match invoke(op, &args) {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":value.to_string()}],"isError":false})
                }
                Err(err) => {
                    json!({"content":[{"type":"text","text":format!("{err:#}")}],"isError":true})
                }
            }
        }
        _ => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not supported"}}),
            );
        }
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
