# Architecture

Current additions: Phase 3C push and Phase 3D change review use separate command policies in `push.rs` and `changes.rs`. `workspace.rs` shares descriptor identity and advisory-lock admission. They reuse the operation queue, cancellation flags, safe errors, SQLite, and existing UI details without new dependencies or Tauri capabilities.

`start_push` prepares a durable exact-HEAD/destination preview and waits up to five minutes for one-shot `confirm_push`. Fresh constrained fetches before and after approval use the existing advertised-ref result, excluding deleted cached destinations. An isolated fixed source ref publishes one explicit non-force refspec; independent `ls-remote` verifies the exact target. Schema 6 adds push journals. Successful publication, verified no-op, remote rejection, preflight authentication/network failures, cancellation, and uncertainty remain distinct. Startup reconciles unfinished pushes to uncertain without any network request or retry.

`review_changes` retains one bounded in-memory snapshot; its opaque token, project ID, and literal selected paths authorize `start_index_change` or `start_commit`. NUL-delimited status/numstat output preserves unusual filenames. `change_diff` returns bounded transient text separately from journals. Staging and index-only restore run on a private index. Under a native `index.lock`, the backend compares original index bytes/identity, branch/config/HEAD, and working-file snapshots, then durably writes and renames its own lock to install the verified index. Other Git tools' locks are never adopted or removed.

Commit preparation computes the staged tree from the private index, resolves direct global/local author identity, and presents it with the exact message and staged-file summary through `change_status`. `confirm_commit` consumes its backend job ID once. Execution reserves the original index and HEAD, revalidates approval, writes an unsigned commit object with the existing HEAD as its single parent, and compare-and-swaps only the approved local branch. A detached private Git metadata view with a controlled `commondir` points at the original ref/object store; this avoids Git's duplicate-HEAD transaction restriction while native `HEAD.lock` prevents cooperating tools from switching HEAD. Branch reflog is updated; the original HEAD reflog is preserved. Failed prepublication steps can leave unreferenced objects without changing the branch.

Schema 7 adds stage/unstage/commit journals containing phases, path summaries, parent/tree/result SHA, and safe errors, but no file contents or messages. Both migrations are transactional. Local critical writes defer cancellation/timeouts; stdout is bounded/drained and stderr suppressed. Post-write verification refreshes registry/sync before durable completion. Startup marks unfinished local operations as requiring inspection. A fresh review is required after any consumed token, including uncertain outcomes. SQLite and filesystem/ref changes are not one atomic transaction. Private stages and journal records accumulate; no automatic cleanup is provided.

Stow is a modular monolith. A single Rust crate exposes a reusable core library; the optional `desktop` feature adds Tauri and the native dialog plugin. Core tests run without compiling the webview. React renders backend snapshots and never performs Git or filesystem business logic.

## Modules

| Module | Responsibility |
| --- | --- |
| `src-tauri/src/core.rs` | Domain models, timestamps, structured errors, path encoding validation |
| `database.rs` + `migrations/` | SQLite migration, project registration, last-success snapshots, unregister |
| `git.rs` | Fixed read-only command vocabulary, subprocess policy, config checks, status parsing, remote redaction |
| `github.rs` | Fixed GitHub CLI policy, structured auth, paginated provider adapter, validation, controlled Terminal/browser handoff |
| `library.rs` | Independent remote cache models and conservative local/remote relationships |
| `projects.rs` | Bounded discovery, exclusions, duplicate roots, refresh/error persistence |
| `operations.rs` | Operation IDs, state transitions, cancellation tokens, bounded session history |
| `platform.rs` | Tauri application-data directory and Unix directory permissions |
| `ipc.rs` | Narrow typed commands, background work, admission control, progress notifications |
| `frontend/api.ts` | TypeScript IPC contracts and typed wrappers |
| `frontend/App.tsx` | Project list/details, scan controls, errors, operation progress |

The core has no dependency on React, Tauri state, or a specific UI. A future CLI can call the same registry, Git adapter, and discovery service. No speculative plugin/workflow framework is present.

## Data flow

1. Native directory dialog returns explicit user-selected roots to the renderer.
2. `start_scan` validates request size and admits one operation. A blocking worker performs filesystem traversal, leaving the webview responsive.
3. Each candidate passes Git policy checks and inspection. A successful snapshot is committed immediately to SQLite; errors remain attached to the scan without stopping unrelated repositories.
4. Progress is stored in the operation record. Scan events are throttled to four per second; completion emits another event. The UI also polls once per second during work to recover missed events.
5. The UI reads projects and operations and renders actual persisted/backend data. No fixtures are used in production.

Individual and batch inspection use the same worker infrastructure. A single active operation limits process/IO overhead and simplifies consistency. Operation admission and unregister share a mutex, preventing in-flight scans from immediately restoring a removed registration. SQLite access has its own mutex and three-second busy timeout. No database lock spans a Git command.

## Persistence

Schema version 1 uses `PRAGMA user_version` and a transactional migration. `projects.id` is an integer primary key; canonical `path` is unique; `name` has a case-insensitive search/order index. Registration and successful-inspection timestamps use Unix seconds. Availability is constrained to `available`, `missing`, `unsupported`, or `error`. Domain snapshots and structured errors are stored as JSON in separate columns; list queries reject corrupted JSON instead of inventing defaults. No credentials are intentionally persisted.

Registration uses an upsert retaining the original ID and registration time. Failed refreshes retain the previous successful snapshot and timestamp while updating availability/error. Removal executes only a parameterized `DELETE` against the registry. WAL supports recovery of completed transactions; it is not a backup. A future schema version is rejected explicitly.

## IPC and presentation

The API exposes `system_info`, `list_projects`, `list_operations`, `start_scan`, `start_inspection`, `cancel_operation`, and `remove_project`. IDs are used for inspection/removal; there is no arbitrary shell, SQL, file-write, or Git-command endpoint. Renderer and Rust structs are manually matched; TypeScript compilation checks caller usage, not automatic Rust-to-TS schema generation. Changes to wire fields must update both sides.

Database initialization failure is kept in managed state, allowing the window to display a recoverable error. A Git detection failure does not prevent viewing existing registrations. Tauri capabilities grant the local main window core APIs and native open-dialog access. No remote origins, shell plugin, filesystem plugin, or SQL plugin are enabled. A restrictive CSP protects the embedded frontend.

## Platform and performance

macOS is the initial platform. Persistence uses Tauri's OS-specific app-data resolver; on macOS this is Application Support. Git paths and `/dev/null` assumptions are isolated in the adapter but need porting for Windows. Filesystem traversal uses standard APIs, never follows ordinary directory symlinks, and stops at repository markers. Git may access linked-worktree metadata outside the chosen directory, as required by Git's layout.

Git subprocess output is drained concurrently with bounded buffers; the supervisor checks cancellation, timeout, and overflow every 10 ms. Processes are killed and reaped on those conditions. No generic mutation commands are exposed. Operation history is not durable; closing the application ends the session, and completed registrations remain in SQLite.

Implementation references: [Tauri project setup](https://v2.tauri.app/start/create-project/), [native dialog plugin](https://v2.tauri.app/plugin/dialog/), and [Git configuration precedence/options](https://git-scm.com/docs/git-config). The specific tested policy and its limitations are documented in [SECURITY.md](SECURITY.md).

## Phase 2A: GitHub provider and unified library

The existing Git adapter and local project schema remain unchanged. `github.rs` is a separate metadata-only policy; it does not expose a generic runner. Executable discovery uses fixed installation paths and is repeated when rechecking, logging in, or refreshing so installation does not require restarting Stow. The provider boundary returns a validated account and repository snapshot. No speculative multi-provider framework or new dependencies were added.

Authentication and cached data are independent. `github_status` runs outside the UI thread, checks CLI version/usability, and parses the active account's structured status. Local initialization does not wait for GitHub. Missing/unusable CLI, no login, invalid credentials, uncertain status, unreachable network, and unsupported configurations are distinct. Raw authentication diagnostics are discarded. Terminal login uses only fixed gh arguments and safely quoted OS configuration paths; Stow sees only the handoff result. Completion is checked explicitly by the user.

`start_github_refresh` uses the existing operation admission mutex, operation ID/history, cancellation flag, progress, and completion events. Only one operation can run, preventing duplicate refreshes and older results overwriting newer results. Progress is validated page/repository counts, not an invented network percentage; collection can be indeterminate until gh exits. Events are throttled. Blocking work runs off the UI thread, and no database lock spans network IO.

The adapter verifies `/user` account identity before and after `gh api --paginate --slurp /user/repos?...`; gh follows Link pagination, and any nonzero exit discards all pages. Explicit page arrays inside the slurped JSON result are parsed and validated only after successful command completion. The response has a 32 MiB bound; records/pages have additional bounds. Duplicate identities/names or malformed data abort the whole refresh. This is staged, bounded in-memory retrieval, not persistent raw response storage. Successful retrieval does not imply unrestricted organization access or an atomic server-side snapshot. External account switching away and back during pagination cannot be ruled out without owning credentials, which Stow intentionally does not do.

Migration `002_github.sql` advances `user_version` to 2 transactionally, preserving `projects`. `remote_repositories` uses `(provider, host, provider_id)` identity and a validated metadata JSON snapshot. `github_accounts` stores stable account ID/login and last successful refresh. `github_access` stores availability and last-seen time per account/repository. `github_names` retains canonical/historical names for conservative matching. `github_refresh` stores the last selected successful account plus last-attempt/error metadata. The cached metadata records its last source account. A successful transaction upserts stable identities, marks unseen memberships `not_returned`, records names, and advances success timestamps. Failed/cancelled transactions preserve prior metadata; the safe attempt error may be recorded separately. Records are never deleted merely because they disappear from a provider response. Historical cache growth is retained intentionally; no eviction policy is implemented.

`repository_library` returns local projects, a selected account's cached remotes, and relationship candidates. An absent account parameter selects the last successful account. Explicit selection allows old account libraries offline; it never changes gh's account. Relationships are derived from stored local remote snapshots and current/historical provider names. Only one unambiguous current identity links; historical and conflicting names remain uncertain, including when another account supplied the history. Multiple local workspaces can reference one remote without merging local registrations.

The added narrow IPC commands are `github_status`, `github_login`, `start_github_refresh`, `repository_library`, and `open_github_repository`. URL opening accepts a cached repository ID/account selection, not a renderer-supplied URL. Rust reconstructs and validates the HTTPS GitHub.com URL before calling macOS `/usr/bin/open`. No new Tauri plugins or renderer capabilities were introduced. `frontend/library.ts` supplies deterministic presentation/search/filter/sort logic, covered by Node's built-in test runner.

Provider contracts: [gh auth status](https://cli.github.com/manual/gh_auth_status), [gh api pagination](https://cli.github.com/manual/gh_api), [authenticated-user repositories](https://docs.github.com/en/rest/repos/repos#list-repositories-for-the-authenticated-user), and [official login](https://cli.github.com/manual/gh_auth_login).


## Phase 2B: clone and open

`clone.rs` implements a creation-only workflow using the existing Git executable and read-only verification adapter, operation queue, registry, and provider identity validation. `safe_fs.rs` provides pinned directory handles, exclusive creation/promotion, identity checks, and cooperating-process locks. `editor.rs` contains fixed editor discovery and bounded launch. `frontend/WorkspaceControls.tsx` adds preferences, destination preview/override, clone actions, and retained-data recovery; `frontend/workspace.ts` defines recovery-action eligibility.

Migration `003_clone.sql` advances schema 2 to 3, preserving projects and provider caches. `workspace_preferences` stores the default root and editor ID. `clone_jobs` stores bounded provider metadata, intended paths, directory identities, phase/state, verification snapshot, safe errors, and project ID. Journals remain available after restart; regular operation history remains session-local. The durable phases are `reserved → staging → creating → verified → promoting → promoted → registered`. A failed/cancelled operation retains its last phase. An interrupted `running` journal is displayed as in progress or interrupted; only an explicit recovery request proceeds.

The workflow journals intent, creates private staging, verifies live repository identity, populates and inspects the repository, journals promotion intent, exclusively renames to the final destination, and atomically inserts the project plus completed journal in SQLite. There is no cross-filesystem/database transaction. Explicit recovery revalidates pinned identities and the recorded inspection; the `promoting` phase can resolve a rename that completed before its next database write. Incomplete unverified stages only offer review/retry. Successful registration naturally feeds the existing derived local/remote library. Editor opening happens afterwards and cannot invalidate the clone.

Added IPC: `workspace_settings`, `save_workspace_preferences`, `validate_clone_destination`, `start_clone`, `start_clone_recovery`, `open_project`, and `reveal_clone`. Clone/recovery use the existing admission lock, one-operation queue, cancellation token, throttled notifications and polling. Progress adds optional text and transfer percentage. Preferences/journals are backend snapshots; no filesystem implementation is moved into React. Tauri capabilities remain unchanged. The direct `libc` declaration reuses the existing lockfile package for required OS safety primitives.

## Phase 3A: sync assessment and explicit fetch

`sync.rs` adds structured local assessment and a dedicated fetch policy; the inspection and clone command policies remain separate. `git_process.rs` extracts clone's bounded process-group supervision so both mutation policies share cancellation, draining, and safe constant diagnostics without sharing command authority. No dependencies were added.

Assessment reuses `Git::inspect`, resolves configured upstreams, and uses `rev-list --left-right --count HEAD_OID...UPSTREAM_OID` for cached ancestry. Detached/unborn HEAD, absent/ambiguous upstream, missing tracking refs, operation markers, and unsupported history remain distinct states. The first explicit sync inspection binds legacy registrations to root/.git device and inode identities; subsequent inspections cannot silently rebind a replaced directory. An assessment token, registration ID, and selected remote name form the narrow fetch request; backend checks run again at execution.

Fetch uses the existing admission mutex, one-operation queue, cancellation, throttled events, and polling. It validates private ordinary metadata, obtains root/.git advisory locks, creates a private bare staging directory inside `.git`, seeds staged heads from the selected cached tracking namespace, and fetches into that heads namespace with no force, prune, tags, or automatic maintenance. The original object store is shared to avoid duplicating existing packs. Git enforces fast-forward-only updates in the heads namespace. A prepared `update-ref` transaction applies expected-old updates only to the selected remote-tracking namespace, with a symbolic/detached HEAD guard. Post-fetch local inspection refreshes the registry and assessment. Working-tree/index/local branch writes are absent from this policy.

Schema 4 adds `sync_states` and `fetch_jobs`, both tied to registered projects. The journal records reserved/transferring/applying_refs/verifying/verified phases and running/completed/failed/cancelled outcomes. Successful completion is saved only after reinspection. Ref updates and SQLite writes are **not** one atomic transaction. Database failure after ref application returns an error and never creates a success timestamp; the earlier phase remains useful when further writes also fail. Running journals after restart remain explicitly unfinished, with no automatic resume. All journals persist; UI history reads the latest 50. Staging metadata is never deleted automatically.

IPC adds `sync_status`, `start_sync_inspection`, and `start_fetch`; React `SyncDetails.tsx` renders the structured snapshot and existing operation progress. Last-success lookup matches both remote name and validated URL. Local inspection time, last verified fetch outcome, and unknown current remote availability are separate labels. A success timestamp is not a remote backup certificate, and retained tracking refs for deleted branches are not proof of current remote presence.


## Phase 3B: confirmed fast-forward pull

`pull.rs` holds a prepared pull (root/.git directory capabilities, exact config/index/ref snapshot and upstream selection). `start_pull` reserves the existing queue, performs preflight on a blocking worker and persists `awaiting_confirmation`. `confirm_pull` accepts the current backend journal ID exactly once over an in-process channel. The worker retains the workspace locks while awaiting confirmation/cancellation for up to five minutes. No arbitrary commands enter IPC. Selection changes do not authorize a different workspace.

The prepared operation rechecks its snapshot, calls Phase 3A fetch while retaining locks, checks all tracking refs against that fetch's verbose porcelain report, and checks ancestry and both trees. It repeats critical validation immediately before a separate integration runner executes `merge --ff-only` against the verified commit. The equal-HEAD path skips mutation. Integration deliberately does not use the cancellable transfer supervisor. Post-integration inspection updates registry and sync state before durable success; frontend operation events/polling refresh the details and history.

Schema 5 adds `pull_jobs` with preflight/awaiting_confirmation/fetching/ready/integrating/verifying/verified phases and completed/failed/cancelled/requires_inspection outcomes. It records branch/upstream/start/target/observed HEAD and relation. Desktop startup reconciles unfinished pull journals conservatively, without touching repositories. Readers opening the registry do not reconcile active operations. The transient operation queue gains `requires_inspection` for uncertain pulls. Fetch history remains independent, including a successful fetch followed by a refused pull. No dependency or Tauri capability changes were needed.
