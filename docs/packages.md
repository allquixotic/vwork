# npm, Cargo and pip

vwork provides native Rust orchestration for installed package tools. Package
managers and compilers still need physical source, dependency caches and build
outputs. These actions use the **one shared build checkout per repo**, not a
checkout/environment per virtual workspace. They take the same lock as custom
tasks and prepare its borrowed-object Git view, so Git-aware build scripts work.

The human registers named projects once. An agent gets `vwork_package_list` and
`vwork_package_run`; it cannot choose a new executable, arbitrary arguments or
package project configuration through MCP. Existing build scripts and npm
lifecycle scripts still execute with your account's normal permissions. This is
orchestration and tool routing, not a security sandbox.

## Register projects

Use the same `$Vwork` and `$State` as the README's PowerShell quick start. Set
`path` to the repository-relative project directory, or `''` for its root:

```powershell
@{repo='mono'; name='web'; manager='npm'; path='apps/web'; offline=$false;
  scripts=@('build','test','lint')} | ConvertTo-Json -Compress |
  & $Vwork --home $State call package_configure --json -

@{repo='mono'; name='rust'; manager='cargo'; path=''; offline=$false} |
  ConvertTo-Json -Compress |
  & $Vwork --home $State call package_configure --json -

@{repo='mono'; name='python'; manager='pip'; path='tools/service';
  requirements='requirements-dev.txt'; offline=$false} | ConvertTo-Json -Compress |
  & $Vwork --home $State call package_configure --json -
```

Or save one object in a JSON file and pipe it into the CLI:

```sh
vwork --home /path/to/state call package_configure --json - < examples/package-npm.json
vwork --home /path/to/state call package_list --json '{"repo":"mono"}'
```

`package_configure` replaces the named project's configuration. The configuration
is stored in redb and immediately available; no daemon restart is needed.

| Field | Meaning |
|---|---|
| `repo`, `name`, `manager` | Required IDs and `npm`, `cargo`, or `pip` |
| `path` | Relative project root; defaults to repo root |
| `program` | Optional executable name/absolute path; defaults to `npm.cmd` on Windows / `npm` elsewhere, `cargo`, or `python` on Windows / `python3` elsewhere |
| `args` | Arguments before generated arguments, e.g. `["+stable"]` for Cargo or `["-3.12"]` for the Windows `py` launcher |
| `env` | Extra environment variables; do not store secrets here |
| `offline` | Default false. Sets npm offline / Cargo `--offline` / pip `PIP_NO_INDEX` |
| `requirements` | Optional pip requirements file relative to project root |
| `inputs` | Extra dependency-input paths relative to project root |
| `scripts` | npm script names available as actions; default `build`, `test`, `lint` |

Project paths and inputs must use `/`, stay inside the repo, and satisfy vwork's
portable-path rules. Executables are resolved in the daemon's inherited PATH;
restart it after changing PATH, or set `program` explicitly. A configured Python
must support `venv --copies` and provide pip through ensurepip. Some standalone
macOS Python distributions have copied-executable library-path problems; select
a working installed Python with `program`. vwork does not download a runtime.

## Agent workflow

Read the workspace revision, list projects, then call an action:

```json
{"workspace":"task-one","expected_revision":"CURRENT_REVISION","project":"web","action":"install"}
```

Use that object with `vwork_package_run` in MCP or `call package_run --json -` in
the CLI. The returned `success`, exit code, stdout/stderr and tested `revision`
have the same semantics as custom tasks. The global timeout covers all steps,
including Python environment creation, and stream capture is bounded.

| Manager | Actions | Behavior |
|---|---|---|
| npm | `install`, `ci`, `update`, `clean`, configured scripts | `install` chooses `npm ci` when a lockfile exists, otherwise `npm install`; install operations disable audit/fund requests. Script action runs `npm run <name>`. `clean` removes project `node_modules`. |
| Cargo | `install`, `build`, `check`, `test`, `clippy`, `format_check`, `update`, `clean` | `install` means `cargo fetch --locked`, not global binary installation. Build/check/test/clippy use `--locked`; `format_check` runs `cargo fmt --all -- --check`; `update` may change Cargo.lock. `clean` removes this project's managed target directory. |
| pip | `install`, `check`, `test`, `build`, `freeze`, `clean` | `install` prepares a copied venv and installs configured requirements, `requirements.txt`, or editable `.` from pyproject/setup.py. Other actions run `pip check`, `pytest`, `pip wheel --no-deps`, or `pip freeze` inside that venv. `clean` removes it. |

Cargo projects need a committed/virtual lockfile for locked operations. To create
one, use `update` and import the result. npm scripts must exist in package.json.
Python `test` needs pytest installed by your requirements; wheel builds need the
project's build dependencies available. Offline mode does not populate caches;
install from your approved registry/mirror/cache first. Nothing publishes packages
or installs Cargo binaries globally.

The defaults cover common workflows. Use a human-defined custom task for extra
flags, npm workspace selection, custom test runners, pip index options, or other
package managers. For npm workspaces, register the root containing the shared
lockfile and use configured root scripts to target individual packages.

## Reuse, disk use and invalidation

- npm uses `node_modules` inside the shared project and inherits npm's normal
  cache/config. Multiple projects in a repo should have distinct roots or be one
  npm workspace project.
- Cargo inherits Cargo home/registry/Git caches and sets `CARGO_TARGET_DIR` to
  `<home>/targets/<repo>/<project>`, outside the source checkout.
- pip inherits pip's cache/config. It creates a reusable environment at
  `<home>/environments/<repo>/<project>` using `venv --copies`, sets VIRTUAL_ENV
  and prepends its executable directory to PATH. Wheels go to
  `<home>/artifacts/<repo>/<project>`.

npm scripts and pip check/test/build/freeze require a successful install matching
the current dependency inputs. vwork fingerprints project configuration,
manifests, lockfiles, requirements, selected nested manifests and extra `inputs`.
Changing them invalidates prepared state. A pip reinstall for different inputs
recreates the environment so obsolete packages cannot silently survive. An npm
locked install uses `ci` to replace dependencies. A failed install invalidates
the previous success record. These records are shared per project, so switching
between workspaces with different inputs may require reinstalling.

Custom includes outside those defaults, local path dependencies in other project
directories, build configuration, registry state, and manual dependency-directory
changes are not fully inferred. Add relevant files to `inputs`; reinstall after
changing external state. The same inputs can still resolve differently without
lockfiles/pins. vwork supplies no reproducible dependency solver of its own.

Dependency caches/outputs are ordinary disk files and can exceed source size.
Use `clean` to reclaim the managed environment/target/node_modules, then install
again as needed. It does not clear your global npm/Cargo/pip caches or wheel
artifacts. Generated symlinks or platform requirements of third-party packages
are determined by those packages; vwork itself needs no symlink capability.

## Import lockfiles and generated source explicitly

Actions never edit the virtual workspace automatically. Results include:

- `checkout`, `revision`, and `base_commit`;
- `source_changes` for tracked physical edits/deletions plus known input changes;
- `package.input_changes` including newly created known lockfiles;
- package manager, project, action, environment and artifacts paths.

If npm creates `apps/web/package-lock.json`, import it **before another action**:

```json
{"workspace":"task-one","expected_revision":"REVISION_FROM_RESULT","source":"CHECKOUT_FROM_RESULT","paths":["apps/web/package-lock.json"]}
```

Call `vwork_workspace_import` with that object. Only then read the new revision
and run the next package action. Until that lockfile is in the virtual snapshot,
the installed-input fingerprint deliberately does not match. A manifest-changing
formatter or package update uses the same explicit import flow. Name any desired
new generated source files as well; arbitrary untracked files are not enumerated.
Do not import node_modules, venvs, targets or caches.

The checkout lock lasts for each action, not the entire edit/install/import
conversation. Another agent can reuse the checkout between calls. Coordinate
physical-edit import promptly; if the marker or revision changed, preserve/review
the output and reconcile through virtual edits. A failed revision check never
silently imports into a different virtual snapshot.
