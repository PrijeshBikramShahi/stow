# Stow

**Your projects. On demand.**

Stow is a local-first, Git-powered desktop workspace manager. Stow discovers, registers, and inspects existing local repositories and optionally caches accessible GitHub repositories. It requires no account, cloud service, telemetry, container, or paid API.

## Available now

- Tauri 2 desktop app with a minimal React/TypeScript interface.
- Native selection of multiple scan directories; bounded background discovery with progress and cancellation.
- SQLite registry with migrations, canonical-path deduplication, and persistence across restarts.
- Search, individual inspection, refresh all, and unregister (database record only).
- Branch, detached/unborn HEAD, commit, redacted remote URLs, upstream, and staged/unstaged/untracked/conflict counts.
- Explicit missing, inaccessible, unsupported, database, and Git errors; last successful metadata is retained and labelled stale.
- Normal repositories and standard linked worktrees. Submodules are not registered independently.

## GitHub library (Phase 2A)

GitHub is optional; local scanning and inspection work independently. Remote-only entries contain metadata, never cloned files.

1. Install a current official [GitHub CLI](https://cli.github.com/) using its [installation instructions](https://github.com/cli/cli#installation). Stow checks `/opt/homebrew/bin/gh`, `/usr/local/bin/gh`, `/opt/local/bin/gh`, and `/usr/bin/gh`, independently of the macOS GUI PATH. Homebrew is not required. After installation, choose **Recheck authentication**.
2. Choose **Connect in Terminal**. Stow asks macOS Terminal to run `gh auth login --hostname github.com --web --git-protocol https --skip-ssh-key`. Complete the official browser/Terminal prompts yourself, then choose **Recheck authentication**. If macOS blocks the handoff, run that command in Terminal yourself. Stow does not need Automation permission for local inspection or remote listing.
3. Choose **Refresh GitHub repositories**. The supported active GitHub.com account is used; Stow never switches accounts. Authentication checks do not automatically refresh repositories.
4. Search by name or owner; filter local/remote-only and public/private; sort by name or recent remote push. Select a row for local inspection or cached remote details, then **Open on GitHub** if desired.

Stow uses `gh auth status --json hosts` and checks the active account's structured status, including errors even on exit code zero. The official CLI manages authentication storage. Stow never requests, reads, stores, or logs raw tokens. Saved gh configuration uses `HOME`, or absolute `GH_CONFIG_DIR`/`XDG_CONFIG_HOME` when explicitly present in Stow's launch environment. Token environment variables are intentionally ignored; use a saved gh login. No OAuth app or manually created PAT is required. Login may affect gh's saved active account/protocol; it does not modify local repositories.

Repository listing requests `/user/repos`, including owner, collaborator, and organization-member affiliations and both public/private visibility. Access depends on your credentials, scopes, organization policies, and SSO. A successful response cannot prove that every organization repository was permitted. GitHub.com is supported; Enterprise host configuration is detected but listing is unsupported.

The SQLite cache stores validated repository metadata, stable GitHub repository/account IDs, per-account last-seen availability, historical names, and fetch timestamps. Complete validated results are applied in one transaction. Failed, cancelled, oversized, or partial requests retain the prior cache. Repositories absent from a successful result are marked **not returned** (access loss, permissions, or another unknown reason); their history is preserved. Each cached account remains selectable after switching accounts or going offline. These caches are local history, not separate OS-user security boundaries.

All remote data is labelled cached. The UI flags unconfirmed/disconnected results, failed refreshes, and data older than 24 hours as stale. Stow performs no automatic repository refresh or synchronization daemon. Refresh operations share the local operation queue and support cancellation. API output is capped at 32 MiB; one paginated request at 120 seconds; validated results at 10,000 repositories and 1,000 pages. Limits fail safely without truncating the cache.

Matching uses conventional GitHub.com HTTPS/SSH remote URLs from all remote names, case-insensitive host/owner/repository names, and an optional `.git` suffix. Names alone never establish identity. Multiple candidates, historical names after renames/transfers, URL reuse, missing local workspaces, or unresolved additional GitHub remotes produce an uncertain link. Historical names are never silently promoted to verified links. Local Git snapshots remain separate. No link establishes recoverability or permission to remove a workspace.

## Clone & Open (Phase 2B)

1. Expand **Workspace preferences**, choose an existing default workspace directory, select VS Code, Cursor, or Finder, and save.
2. Select a remote-only GitHub repository. Override the destination root or new folder name if needed. **Check destination** is a preview; cloning validates the filesystem again.
3. Choose **Clone & Open** or **Clone only**. Progress and cancellation use the existing operation panel. A successful clone becomes a registered local workspace linked to its remote.
4. Select a local workspace to **Open workspace** or **Reveal in Finder**. Editor failure does not undo cloning or registration.

Use a local root owned by you, not writable by other groups/users, without extended ACL entries, and with no symlink components (use `/private/tmp`, not macOS's `/tmp` alias). Folder names use ASCII letters, digits, dots, underscores, and hyphens and cannot begin with a dot or hyphen. Stow never replaces an existing destination. HTTPS GitHub.com and saved gh authentication are supported; SSH, Enterprise, custom helpers/proxies, submodule population, and Git LFS hydration are not.

A private `.stow-clone-<id>` directory and durable SQLite journal are created before fetching. Failed, cancelled, or interrupted data is retained, including empty staging containers after success. **Unfinished clones** shows recorded paths and review actions across restarts. **Finish verified clone** rechecks completed Git data before finishing promotion/registration. Unverified partial attempts must be reviewed manually and retried as a new clone. There is no staging deletion/cleanup command. A successful filesystem promotion followed by database failure leaves the workspace intact for explicit recovery.

Editors are detected in `/Applications` and `~/Applications`. Handoff requests a new window with extensions disabled; editor trust/security settings and subsequent user actions remain the editor's responsibility. See [security decisions and limits](docs/SECURITY.md#phase-2b-clone-and-editor-boundary).

## Sync preflight & fetch (Phase 3A)

Select a registered local workspace and choose **Inspect sync**. The assessment shows branch/HEAD, upstream, file counts, ahead/behind or divergence, operation markers, and unsupported configurations. Counts compare local commits with **cached** tracking refs; clean files, an upstream, and a successful fetch do not prove remote recovery or a successful push.

Choose a configured remote and **Fetch selected remote** to refresh its tracking refs explicitly. The shared operation panel provides progress and cancellation. Stow updates objects and remote-tracking metadata, then reinspects and saves the outcome; it never changes checked-out files, HEAD, local branches, or tags. Rewound/diverged remote history is rejected rather than force-updated. Deleted remote branches remain cached because fetch never prunes. Fetch timestamps apply to the selected remote name and validated URL, separately from local inspection time.

Fetch requires Git 2.46+, a private ordinary local `.git` directory, standard tracking mappings, and canonical `https://github.com/owner/repository.git` or an absolute private local bare-repository path. Saved gh authentication is used for GitHub. SSH, custom transports/helpers/configuration, linked/shared worktrees, shallow/partial/alternate history, and replace refs are unsupported for fetch; inspection support is unchanged. Rejection messages explain what to review in a terminal; Stow never rewrites configuration to bypass checks.

**Recent fetch history** persists through restart. Failed, cancelled, or interrupted attempts never become successful automatically. Objects or refs may already have changed when an attempt stops; inspect again before retrying. Private `.git/.stow-fetch-<id>` metadata is retained for review, including after success; no automatic retry, rollback, scheduling, or cleanup occurs. See [fetch policy](docs/SECURITY.md#phase-3a-synchronization-and-fetch-boundary).

## Fast-forward pull (Phase 3B)

After **Inspect sync**, choose **Pull (Fast-Forward Only)**. Stow reserves the operation queue and workspace, checks its mutation policy, then asks you to confirm the exact branch, starting HEAD, and upstream. Confirmation expires after five minutes. It authorizes fetching that configured remote and advancing to its newly fetched upstream commit, only if it descends from the unchanged local HEAD. An equal commit is a successful no-op. Ahead-only and diverged branches are blocked.

Pull requires clean tracked files and index, **no untracked or ignored files**, ordinary index entries, private ordinary files, no hooks (apart from unused sample files), attributes, submodules, symlinks, hardlinks, sparse checkout, or extra Git settings. Both current and fetched trees are checked. Fetch restrictions above also apply. Deleted remote branches cannot authorize a pull from their retained cached refs. Unsupported cases must be handled in a terminal; Stow does not alter configuration to enable them.

Keep editors and other Git tools idle during pull. Cancellation works before/during fetch and before integration. Once the working-tree update begins, Stow waits for Git and verifies the observed state, even if cancellation was requested. It never kills that update for a timeout. HEAD, index, files, and SQLite are not atomically updated together. **Requires inspection** means completion is uncertain: stop other Git activity, inspect status/HEAD/history/files and any locks in your terminal, then explicitly inspect again. Stow never resets, removes locks, automatically resumes, or retries. Unfinished pull journals become **requires inspection** on startup. Recent pull history includes the verified target and observed post-update HEAD/status when available.

## Authorized push (Phase 3C)

Choose **Review push** after inspecting sync. Preflight fetches the configured upstream, requires an existing destination branch, and shows the exact local commit, destination URL/ref, ahead count, and up to 20 commit summaries. **Confirm single-branch push** authorizes only that snapshot for five minutes. Stow rechecks HEAD/config/destination, refreshes the remote again, requires ancestry, and sends one fully qualified non-force refspec. It never publishes tags, other branches, or a first-time branch.

Dirty files do not block publication of committed changes. **Staged, unstaged, untracked, and ignored content is excluded.** Hooks, custom push settings, push URLs, mirror behavior, and unsupported transports/configuration are rejected. The existing fetch restrictions also apply. After dispatch, Stow independently queries the exact remote ref and refreshes sync before recording verified success. A remote race can reject the push; cancellation, failed verification, or interruption can leave an **uncertain** outcome. Inspect the exact remote branch and run a fresh preflight before deciding to retry. There is no automatic retry or promise of lasting remote recoverability.

## Review, stage, and commit (Phase 3D)

The **Changes** section separates staged, unstaged, and untracked files. Choose a file's staged or working diff, select individual files, and use **Stage selected files** or **Unstage selected files**. Rename selections include both displayed paths; filenames with spaces, Unicode, tabs, newlines, and pathspec characters are preserved literally. Binary content is identified without rendering it. Ignored files are excluded by local ignore rules. Unstaging changes only the index and preserves working files.

Enter your message and choose **Review commit**. Confirmation displays the repository, branch, parent SHA, staged tree and file summary, exact message, and author. The backend verifies the approved index, HEAD, branch, configuration, and author again. Commit creation uses only the staged snapshot, does not amend, and never pushes automatically. After completion, changes and local ahead/behind state refresh; **Review push** remains a separate action.

These operations require a private ordinary repository with an existing commit, normal files/index, and no attributes, filters, submodules, symlinks, hardlinks, sparse/hidden index entries, or unsupported settings. A remote is not required for staging or commits. Direct local Git author settings override direct `$XDG_CONFIG_HOME/git/config` and `~/.gitconfig` values. Missing names/emails, global includes, hook overrides, and signing requirements block commits; settings are never rewritten. Non-sample repository hooks block committing. **Hooks, signing, and project tests are not run.**

Keep other Git tools idle during mutations. Index edits are prepared privately, then installed under the native index lock only after stale-selection checks. Commits reserve the native index and HEAD, create the exact tree/commit, and compare-and-swap the approved branch. Branch reflogs record the update; this constrained implementation preserves the main HEAD reflog instead of adding an entry there. Cancellation is checked before publication but deferred through a local atomic write and verification. An interrupted operation can leave Git locks and retained `.git/.stow-changes-*` metadata. Refresh HEAD/changes and inspect locks/history before another attempt; Stow does not remove others' locks, reset files, or retry automatically.

Review limits are 10,000 changed entries, 8 MiB per changed file, 32 MiB combined changed-file snapshots, 32 MiB index, and 512 KiB displayed diff. Choose at most 1,000 files per index operation. Messages are limited to 64 KiB. Oversized or unsupported changes require terminal review; there is no hunk-level staging. File contents and commit messages are not stored in operation journals.

## Prerequisites

The initial supported platform is macOS on Apple Silicon. Install Xcode Command Line Tools, a current stable Rust toolchain, Node.js 22.12+ (or a compatible newer version), npm, and current system Git. The adapter looks for Git in `/opt/homebrew/bin/git`, `/usr/local/bin/git`, then `/usr/bin/git`; it never uses a repository-controlled PATH. Linux and Windows support remain unverified and require platform changes.

```sh
npm ci
npm run desktop
```

`npm run dev` launches only the frontend; browser previews cannot access Git or SQLite. Use the desktop command for the working application.

To build a macOS application bundle:

```sh
npm run desktop:build
open target/release/bundle/macos/Stow.app
```

For a faster local verification bundle, use `npm run tauri -- build --debug --bundles app`, then open `target/debug/bundle/macos/Stow.app`. These local bundles are not notarized distribution releases.

## Using Stow

1. Choose **Add scan directory** and select one or more directories (up to 16).
2. Click **Scan selected directories**. Successful discoveries are persisted immediately.
3. Select a project for details; use **Inspect project** or **Inspect all local** for current local metadata.
4. **Unregister** removes only the database record. The project folder is never deleted.

Metadata represents the last successful inspection; Stow does not watch filesystem changes. Cancellation preserves discoveries already saved. Scan roots are session-local and are not automatically rescanned. Operations with skipped repositories can complete with issues; expand the issue list to see why.

On macOS the registry is `~/Library/Application Support/dev.stow.desktop/registry.sqlite3`. SQLite may also create WAL/SHM sidecars. Do not delete these while the application is running. Operation history is in-memory, capped at 100, and resets at restart. Clone/fetch/pull/push/change journals, sync assessments, and workspace preferences persist separately.

## Development checks

```sh
npm test
npm run check
npm run test:frontend
cargo check --workspace
```

The tests create small disposable Git repositories under the OS temporary directory. They never run mutations against user repositories. Run tests as an ordinary user so permission-denied cases are meaningful. Both Rust and frontend lockfiles should be kept in version control. One Cargo workspace shares `target/`; dev/test debug symbols and incremental compilation are disabled to reduce disk use.

## Limits of this slice

- Scans are limited to six levels and 20,000 entry visits. Directory symlinks and common dependency/build folders are skipped. Every repository is a traversal boundary; select an intentionally independent nested repository explicitly if needed.
- Bare repositories, submodules as projects, config includes, configured clean/smudge filters (including Git LFS), `core.worktree` redirection, partial clones, worktree-specific config extensions, and non-UTF-8 metadata/paths are unsupported. Rejected configs are never rewritten.
- Read-only Git commands have a 15-second timeout and a 4 MiB output cap per stream. Push commands use a 120-second bound; local mutation critical sections deliberately defer cancellation/timeouts. Very large repositories may need a future inspection strategy.
- Submodule changes, ignored files, backup coverage, all-branch unpublished history, and recoverability are **not** assessed. Fetch records one successful contact, not ongoing remote reachability. “Clean” is not a deletion-safety verdict.
- Global/system Git execution settings and global ignore rules are intentionally ignored; counts can therefore differ from terminal Git. Commit author resolution reads the limited direct global identity settings described above.
- Conventional URL userinfo, query, and fragment credentials are stripped. Nonstandard/local remote URLs are withheld. Arbitrary secrets embedded in repository names or URL path segments cannot be identified reliably.
- This is defense-in-depth for local inspection, not an OS sandbox for hostile repositories. Concurrent filesystem/config replacement is not fully prevented. See [security policy](docs/SECURITY.md).
- No cleanup, archive, workspace deletion, branch creation/switching, amend, merge/rebase, or automatic synchronization is implemented.

See [architecture](docs/ARCHITECTURE.md), [roadmap](docs/ROADMAP.md), and [verification record](docs/VERIFICATION.md).
