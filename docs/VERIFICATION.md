# Verification record

The original Phase 1 record is retained below. The Phase 2A implementation and verification record follows it, including the final results from 15–16 October 2026.

Verified on 15 October 2026, macOS 26.5.2 / Apple Silicon, with Rust 1.99.0, Node 26.4.0, npm 12.1.0, and Git 2.55.0. The workspace was initially empty and was initialized as a Git repository. No existing user source was replaced.

## Automated checks

| Check | Result |
| --- | --- |
| `cargo test --workspace --no-default-features --locked --offline` | **23 passed**: 4 adapter unit tests and 19 temporary-repository integration tests |
| `cargo clippy --workspace --all-targets --no-default-features --locked --offline -- -D warnings` | Passed, no warnings |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, including desktop code, no warnings |
| `cargo fmt --all -- --check` | Passed |
| `npm run build` | TypeScript check and production Vite build passed |
| `npm run tauri -- build --debug --bundles app` | Passed; generated `target/debug/bundle/macos/Stow.app` |
| `npm audit --audit-level=high` | Zero reported vulnerabilities at verification time, including dev dependencies |

Coverage includes registration, uniqueness, restart persistence, unregister/file preservation, missing workspaces, migration-version rejection, normal/empty/excluded/nested discovery, permission errors, symlinks, traversal limits, bare repositories, standard linked worktrees, submodules, branch/upstream/remote states, detached/unborn HEAD, dirty/staged/untracked status, conflict/rename parsing, invalid paths, missing Git, unexpected errors, malicious helpers/filters, config includes, credential redaction, cancellation, timeout, output limits, and operation history.

A failing include-config test revealed that Git's repository setup can read includes before `git config --local --no-includes` processes its options. Preflight was changed to resolve metadata pointers and parse the explicit config file from outside the repository. The regression now passes. No configured helpers ran in the malicious-config fixtures.

## Real desktop smoke test

The built application was launched and driven through its actual macOS window, native directory picker, and Tauri IPC. Only two disposable repositories under a temporary directory were used.

- Empty registry appeared correctly; Git version was detected.
- Native directory selection followed by scan registered both repositories.
- One repository displayed clean status and a redacted remote URL.
- The other displayed exactly one staged, one unstaged, and one untracked entry.
- Individual inspection completed and updated the success timestamp.
- Quit and relaunch retained both registrations and metadata.
- Search filtered the persisted list.
- Moving a temporary workspace and refreshing reported `missing`, retained its previous metadata, and displayed the error. Restoring it and inspecting recovered `available` status.
- Unregister removed the selected row. Independent filesystem assertions confirmed the `.git` directory, modified tracked content, and untracked file still existed.
- Both temporary registrations were removed through the UI. Temporary fixtures were then cleaned up. The app's registry was left empty.

## Unverified or intentionally limited

- Release optimization build, code signing/notarization for distribution, Intel macOS, Linux, and Windows have not been verified. The successful desktop artifact is a local debug bundle with production frontend assets.
- Large-scan UI stress, slow/network filesystem behavior, OS resource exhaustion, process-tree isolation, hostile concurrent repository mutation, and power-loss recovery have not been validated.
- Cancellation, timeout, permission errors, missing Git, and database failures are covered by core tests; those failure scenarios were not all injected into the real desktop window.
- npm's audit does not audit Rust crates. No additional Rust vulnerability-audit tool was installed.

## Installation and disk overhead

Installed the minimal stable Rust toolchain with rustfmt and Clippy; Node, npm, Xcode, and Git already existed. Downloaded one npm dependency tree and Cargo dependencies. No Docker, cloud service, global Tauri CLI, or extra browser/test framework was installed.

Measured after verification (rounded on-disk sizes):

| Artifact | Size |
| --- | --- |
| Shared `target/`, including builds/checks/tests/bundle | 1.3 GB |
| `node_modules/` | 87 MB |
| Production frontend `dist/` | 240 KB |
| Debug `Stow.app` (already included in `target/`) | 28 MB |
| Rust toolchain directory | 470 MB |
| Cargo registry/cache (including pre-existing contents) | 216 MB |
| Application icon assets | 48 KB |

About 8.7 GiB free space remained. No unrelated caches or toolchains were removed. Future release compilation will add another profile within the same shared target directory.

## Phase 2A — implementation and verification, 15–16 October 2026

### Starting architecture and preservation

Focused inspection confirmed a modular Rust core, a version-1 SQLite registry, a hardened read-only Git adapter, a single cancellable in-memory operation queue, narrow Tauri IPC, and React snapshot rendering. All initial source files were untracked/uncommitted. They were extended in place; no commit, checkout, reset, workspace-file deletion, or cache cleanup was performed. The existing Git, scanning, operation, and platform modules and the 19 original integration tests were retained unchanged.

Before editing, the current checkout passed all 23 existing Rust tests, formatting, full-feature Clippy, and the frontend production build. These were fresh checks, not reliance on the previous report.

### Implemented behavior and design

- Fixed-path GitHub CLI discovery, version/usability checks, and structured authentication discovery distinguish missing CLI, absent credentials, valid active account, invalid credentials, uncertain status, network failure, and unsupported host/account configurations. Rechecking rediscovers the executable. Authentication is independent of the cached library.
- The explicit Terminal action runs official `gh auth login --hostname github.com --web --git-protocol https --skip-ssh-key`; manual instructions are also available. Credentials remain with gh. Stow never requests raw tokens, creates an OAuth app, switches accounts automatically, or runs login through its noninteractive API runner. The macOS bundle includes the Terminal Automation purpose string.
- Manual background refresh uses the existing operation queue, progress/events, cancellation, and admission control. Fixed GET `/user` identity checks surround `gh api --paginate --slurp /user/repos` with explicit visibility, affiliations, page size, and ordering. The process output, execution duration, page sizes, and total repository count are bounded. Failed/partial/malformed responses never reach the cache transaction.
- Migration `002_github.sql` advances schema version 1 to 2 without altering local projects. It adds stable provider/host/repository identity, validated metadata, stable account identities, per-account last-seen availability, historical names, and refresh status. Rename/transfer updates preserve repository IDs. Missing results become `not_returned`, never automatic deletions. Transaction failure and cancellation preserve previous metadata and success timestamps.
- The unified library retains local workspaces, provider records, and relationships separately. HTTPS/SSH GitHub.com URLs from any remote name are normalized conservatively. Current unambiguous matches link; historical names, multiple candidates, unresolved additional GitHub remotes, and stale/missing local workspaces remain uncertain. No relation establishes recoverability.
- The UI provides name/owner/path/branch search, local/remote-only and visibility filters, name/recent-push sorting, local details and inspection, remote metadata, validated GitHub URL handoff, cached-account selection, manual refresh, recheck, and persistent cache freshness/error labels. No new dependency, component framework, renderer shell permission, or synchronization daemon was added.

### Files changed

| Area | Files |
| --- | --- |
| New GitHub/provider and matching code | `src-tauri/src/github.rs`, `src-tauri/src/library.rs` |
| Persistence/migration | `src-tauri/src/database.rs`, `src-tauri/migrations/002_github.sql` |
| Core/IPC/desktop wiring | `src-tauri/src/core.rs`, `src-tauri/src/ipc.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/main.rs`, `src-tauri/Info.plist` |
| Frontend | `frontend/api.ts`, `frontend/App.tsx`, `frontend/library.ts`, `frontend/styles.css` |
| Tests/scripts | `src-tauri/src/github_tests.rs`, `tests/library.test.mjs`, `package.json` |
| Documentation | `README.md`, `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, `docs/VERIFICATION.md`, `docs/ROADMAP.md` |

No dependency changes were needed in either lockfile or Cargo manifest. The Tauri capability policy is unchanged.

### Automated results

| Check | Final result |
| --- | --- |
| `cargo test --workspace --no-default-features --locked --offline` | **44 passed**: 25 unit tests (4 existing Git + 21 GitHub/cache/matching tests), 19 existing integration tests |
| `npm run test:frontend` | **5 passed**, Node built-in test runner, no added test framework |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, including desktop commands |
| `cargo fmt --all -- --check` | Passed |
| `npm run build` | TypeScript and Vite production build passed |
| `npm run tauri -- build --debug --bundles app` | Passed; final `target/debug/bundle/macos/Stow.app`, 29.48 MiB |
| Bundle usage description | Verified `NSAppleEventsUsageDescription` in the generated Info.plist |

Deterministic tests require no GitHub account, token, or network. Added coverage includes missing/available gh, auth recheck, structured errors despite exit zero, multiple accounts without switching, unsupported host, failed Terminal handoff, empty/single/multiple-page listing, more than 100 total results, private/organization-owner/fork/archived/empty repository metadata, missing default branch, invalid fields/URLs, duplicate IDs, page bounds, timeout, cancellation, output overflow, safe error classification, fixed execution environment, account change during fetch, partial-pagination failure preserving cache, version-1 migration, restart persistence, rename/transfer reconciliation, absent-record retention, per-account separation, database-trigger-induced rollback, conservative URL matching, ambiguous/history matches, duplicate-operation exclusion, and URL revalidation. Frontend tests cover composed filtering, owner/name/path/branch search, activity sorting, unified rows, and uncertain associations.

An initial parallel fixture run exposed overly short fixture process deadlines under load. Normal fake-executable deadlines were increased; explicit short timeout/cancellation tests were retained. The final complete run passed.

### Real macOS desktop checks

The final bundle was launched through its real macOS window, with native dialog and actual Tauri IPC. Existing gh 2.102.0 and its saved active account were detected successfully. No credentials were printed, entered, or changed during verification.

- A manual refresh retrieved **104 repositories across 2 pages**. Read-only SQLite checks confirmed **16 public**, **88 private**, and **6 non-owned** entries. These counts describe this credential's returned accessible set, not complete organization permission coverage.
- The first live check exposed gh's piped REST pagination behavior: without `--slurp`, page arrays are merged. The original parser rejected that valid larger array safely. The command was corrected to `--paginate --slurp`, a regression fixture with 101 repositories across two explicit pages was added, and live refresh then succeeded.
- Visibility filtering returned 88 private records; owner search narrowed the real library to 2 matching records. Name search found a public record; its remote-only details displayed actual owner, default branch, push/update/fetch timestamps, and cache availability.
- The validated **Open on GitHub** IPC handoff completed without an application error. Browser navigation/content was not separately asserted.
- Native directory selection and a read-only scan registered the current Stow workspace. Local details correctly showed its unborn `main` branch, no remotes, and existing untracked files. No workspace Git mutation occurred.
- Quit and relaunch retained the **104 remote records plus 1 local registration**. The last successful GitHub fetch timestamp stayed unchanged: opening Stow rechecked authentication without automatically refreshing repositories.
- Cancelling a second real remote refresh produced a cancelled operation and a stale/error notice while retaining all 104 cached repositories and the previous success timestamp. The cancellation error and cache also survived another restart.
- The smoke-test local registration was removed through **Unregister**. The source folder, `.git`, and source/lock files remained present. The user's GitHub metadata cache is retained for normal app use. A final manual refresh on 16 October completed successfully with 2 pages / 104 repositories, cleared the cancellation error, and left the app open with the refreshed library.

### Remaining verification limits and supported scope

Interactive Terminal/browser login completion and the macOS Automation permission prompt were not exercised, because a valid saved login already existed. The handoff implementation, safe failure path, bundle purpose string, manual instructions, and subsequent auth discovery are implemented; a new-login end-to-end run remains unverified. No credentials or accounts were changed to manufacture a test.

Actual network-off/invalid-token/rate-limit scenarios, multiple real accounts, real Enterprise servers, live rename/transfer events, and organization SSO limitations were not induced on the user's account. Relevant parsing/cache failure cases are tested with fixtures; Enterprise listing is explicitly unsupported. The API can return a permission-limited or concurrently changing set, and before/after identity checks cannot rule out an external switch away and back during the request. Historical cache records are intentionally retained without an eviction policy. Token environment overrides, custom proxy/certificate environments, nonstandard gh installation paths, and arbitrary remote URL formats are unsupported.

The verified artifact is an Apple Silicon macOS debug bundle with production frontend assets. Release optimization, signed/notarized distribution, Intel macOS, Windows, and Linux remain unverified. Native browser navigation was not independently checked. Automated matching tests cover linked/uncertain relationships; the real workspace used in this smoke test had no remote. No added functionality clones, pushes, pulls, commits, archives, restores, or deletes workspace files.

### Resource measurements

No dependency installation, Docker image, new framework, duplicate target directory, or cache removal was performed. Final allocated sizes from `du -sk`:

| Artifact | Final size | Comparison |
| --- | --- | --- |
| Shared `target/` | 1,328,072 KiB (about 1.27 GiB) | Initial measurement rounded to 1.3 GiB; exact delta unavailable |
| `node_modules/` | 86,300 KiB (about 84.3 MiB) | Initial rounded measurement 87 MiB; no installs/removals performed in this phase |
| Frontend `dist/` | 248 KiB | Initial 240 KiB; +8 KiB allocated |
| Debug app, included in target | 30,188 KiB (about 29.48 MiB) | Initial rounded 28 MiB; approximately +1.5 MiB |
| App-data directory including SQLite sidecars | About 380 KiB at final measurement | Includes the live cached library and varies with WAL activity |

### Recommended Phase 2B

Controlled clone and open-in-editor only: explicit destination selection, collision and symlink checks, a separate authenticated clone policy, cancellable progress, interrupted-clone recovery, post-clone registration/identity reconciliation, and validated editor discovery. Keep push/pull/commit, archive/removal, and automatic synchronization outside that next slice. Phase 2B was not started.

## Phase 2B — Clone & Open (16 October 2026)

### Delivered

Saved workspace root/editor preferences; per-clone root/name overrides and destination validation; remote-only Clone/Clone & Open; fixed GitHub HTTPS creation policy using saved gh authentication; progress/cancellation/deadlines; pinned directory identities and exclusive promotion; durable clone journals; explicit recovery of verified staging or promoted-but-unregistered workspaces; unified library updates; VS Code/Cursor discovery and registered-workspace opening; Finder fallback; separate editor failure reporting. No Phase 3 synchronization or deletion was added.

### Files changed in this phase

| Area | Files |
| --- | --- |
| Clone/filesystem/editor | `src-tauri/src/clone.rs`, `safe_fs.rs`, `editor.rs`, `clone_tests.rs` |
| Existing adapters and services | `src-tauri/src/core.rs`, `git.rs`, `github.rs`, `github_tests.rs`, `database.rs`, `projects.rs`, `ipc.rs`, `lib.rs`, `main.rs` |
| Persistence/dependency | `src-tauri/migrations/003_clone.sql`, `src-tauri/Cargo.toml`, `Cargo.lock` |
| Frontend | `frontend/api.ts`, `App.tsx`, `WorkspaceControls.tsx`, `workspace.ts`, `tests/workspace.test.mjs` |
| Documentation | `README.md`, `docs/ARCHITECTURE.md`, `SECURITY.md`, `ROADMAP.md`, `VERIFICATION.md` |

`libc` was already transitive; declaring it directly adds no new downloaded package and supplies required OS safety primitives. No npm dependency, new target directory, Docker image, or renderer capability was added.

### Final automated results

| Check | Result |
| --- | --- |
| `cargo test --workspace --no-default-features --locked --offline` | **64 passed**: 45 unit tests and 19 integration tests; subsequently added opt-in live smoke compiled and passed separately |
| `npm run test:frontend` | **6 passed** |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, including desktop IPC and opt-in smoke test |
| TypeScript/Vite production assets | Passed, including the final desktop build |
| `npm run tauri -- build --debug --bundles app` | Passed; final Apple Silicon debug `target/debug/bundle/macos/Stow.app`, **30.57 MiB** |
| Opt-in `clone::tests::live_github_backend_smoke` | **1 passed** against real GitHub using the production clone workflow |

Focused tests cover real isolated Git population/registration, empty repositories, persistence and unified links, preferences, path traversal, existing/dangling-symlink collisions, intermediate symlinks, root permissions and extended ACLs, cooperating-process locks, moved root/staging identities, destination creation during population, verified-stage recovery, failed fetch retention, cancellation before/after verification, process timeouts, pipe-holding helper descendants, output bounds/progress sanitization, database-trigger-induced registration failure, the rename/journal crash gap, changed-workspace recovery refusal, discovery exclusion of staging even when explicitly selected, live provider ID/URL revalidation, editor success/failure/missing executable, and editor symlink rejection. Existing inspection, malicious Git configuration, cache, migration, queue, and frontend library regressions remain passing.

Development tests caught a moved-root creation check and the macOS no-ACL `ENOENT` convention; both were fixed. Final process-supervision review identified a PID-reuse window after reaping the group leader. The runner now uses `waitid(WNOWAIT)` until group termination, with a descendant regression test. The desktop bundle was refreshed once for that security fix; no release build or redundant target directory was created.

### Live backend and desktop verification limits

A real production-backend clone of the public `PrijeshBikramShahi/PrijeshBikramShahi` profile repository succeeded using saved gh authentication for identity checks. Progress reached receiving objects, verification, exclusive promotion, and registration. Read-only inspection of the separate smoke registry confirmed `completed/registered`, one project, branch `main`, upstream `origin/main`, a clean worktree, and the canonical credential-free HTTPS origin. The clone, journal, empty staging container, and independent SQLite database are intentionally retained under `/private/tmp/stow-phase2b-smoke-live-kVGBAhVx` (156 KiB allocated). The user's application registry and preferences were not changed by this smoke test.

The first live attempt hit sandbox network restrictions before staging; the authorized network-enabled retry passed. Its independent empty smoke registry is retained at `/private/tmp/stow-phase2b-smoke-_eq22el0`. These paths are temporary OS storage, not durable user projects or backups. The opt-in test is ignored by ordinary test runs and requires an explicit new private temporary root in `STOW_LIVE_CLONE_ROOT`.

The first desktop attempts were blocked by locked macOS. After the user resumed the test, the final bundle was launched and the following real macOS checks passed:

- Native directory selection chose a new private root, `/private/tmp/stow-phase2b-desktop-bWmBew6c`. Saving its default root and VS Code preference succeeded. Cursor correctly appeared unavailable.
- Destination preview rejected `../escape`. A valid `profile-desktop` folder passed preview. **Clone & Open** completed through actual Tauri IPC in nine seconds, with a visible running operation, cancellation control, and registered/100% completion state.
- The profile repository became **Local and remote linked · Clean**, with branch `main`, upstream `origin/main`, and commit `5c54d6e0e2505ebc57eda53d6ced7e22d5083df0`. Read-only SQLite inspection confirmed the completed journal, registered project, and no editor error.
- VS Code visibly opened `profile-desktop` and its README in the Explorer. Its banner showed **Restricted Mode**, and a notification confirmed all installed extensions were temporarily disabled. No trust or enable-extensions action was taken.
- **Reveal in Finder** completed; Finder visibly selected the exact cloned folder and displayed its README.
- Quit/relaunch preserved the root/editor preference, clone registration, and unified remote link. The prior GitHub cache timestamp stayed unchanged.
- Attempting to clone a second public repository to the existing `profile-desktop` destination failed with the collision message. The successful workspace remained intact.
- A separate `Auth_System` attempt named `cancel-smoke` was cancelled through the live UI before durable verification completed. The UI showed the cancelled journal and retained staging path with review/retry instructions, and offered no finish/promotion action. Read-only checks confirmed staging exists, the final destination does not exist, and no project was registered for this attempt.
- A further restart retained that cancelled journal and its recovery message. Original preferences were restored to no default root and Finder. The successful smoke registration remains available for review. No clone/staging files were deleted.

The desktop smoke root contains the successful clone, its empty staging container, and cancelled staging data (400 KiB allocated). These are temporary OS files, not durable project storage. Existing application cache records and unrelated projects were preserved. This continuation changed only this verification document; no rebuild or repeated automated suite was needed.

Remaining limits: real Cursor launching, real editor failure, private-clone credential-helper/SSO coverage, account changes during clone, forced desktop termination, network/removable filesystems, power-loss behavior, release signing/notarization, and other platforms remain unverified. Editor failures and interruption/recovery boundaries have deterministic fixture coverage. Public cloning does not establish private authentication coverage.

### Next slice

Phase 3 should start with read-only synchronization preflight and explicitly requested fetch into registered workspaces under a separately reviewed policy. Pull/merge, commit/push, automatic synchronization, archive, and deletion remain outside this implementation.

## Phase 3A — sync preflight and explicit fetch (2026-10-16)

Implemented structured local sync assessment, explicit selected-remote fetch, state refresh, durable fetch history, and minimal local-detail controls. No dependencies, capabilities, task queue, automatic scheduling, or later-phase actions were added. Existing inspection policy remains intact; clone retains its separate creation-only policy while sharing extracted process supervision.

### Files changed

| Area | Files |
| --- | --- |
| Sync assessment / fetch policy | `src-tauri/src/sync.rs`, `sync_tests.rs` |
| Shared supervision / reuse | `src-tauri/src/git_process.rs`, `clone.rs`, `clone_tests.rs`; crate-visible inspection helpers in `git.rs` |
| Persistence / error / IPC | `src-tauri/migrations/004_sync.sql`, `src-tauri/src/database.rs`, `core.rs`, `lib.rs`, `ipc.rs`, `main.rs`; migration expectation in `github_tests.rs` |
| Frontend | `frontend/SyncDetails.tsx`, `sync.ts`, `api.ts`, `App.tsx`, `tests/sync.test.mjs` |
| Documentation | `README.md`, `docs/ARCHITECTURE.md`, `SECURITY.md`, `ROADMAP.md`, `VERIFICATION.md` |

The source tree was already entirely untracked at the start of this work; existing files and earlier-phase changes were preserved. No commit was created.

### Automated verification

| Check | Result |
| --- | --- |
| Focused sync fixture suite | **14 passed** |
| `cargo test --workspace --no-default-features --locked --offline` | **78 passed** (59 unit, 19 integration); 1 opt-in live Phase 2B test intentionally ignored |
| `npm run test:frontend` | **8 passed** |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, including desktop IPC |
| `npm run build` | Passed TypeScript and Vite |
| `npm run tauri -- build --debug --bundles app` | Passed; one Phase 3A macOS desktop build, **31.26 MiB** bundle |

New coverage includes clean/ahead/behind/diverged states; missing/ambiguous upstream and tracking refs; detached HEAD and operation markers; private metadata, symlink and path substitution refusal; advisory lock contention; changed remote authorization; unsafe config/transports/custom refspecs/shallow history; unavailable remote; mocked authentication/network failures; timeout and process cancellation; cancellation after transfer; expected-old ref and HEAD concurrency guards; no force/prune/tag changes; byte-preserved index and dirty/untracked files; refreshed registry/assessment; successful transfer followed by database failure; persisted success/failure/unfinished history. Existing clone/recovery, discovery, registry, editor, credential-redaction, process-group, GitHub cache, and frontend library tests remain passing.

Development tests caught two integration issues: harmless macOS bare-repository core settings needed recognition, and Git rejects combining symbolic HEAD verification with a separate verification of its current branch in one transaction. The final transaction locks/verifies HEAD and selected tracking refs, never writes local branches, and reinspects to detect concurrent branch commits. Concurrent HEAD/ref fixture tests pass. Raw fixture diagnostics used during diagnosis were removed; production errors remain constant/sanitized.

### Real desktop smoke

The final debug bundle was launched through macOS desktop automation. A new isolated source, bare remote, and ordinary workspace were created under `/private/tmp/stow-phase3a-desktop-2z4mhv_m`; the workspace was added through the native directory picker and scanned through actual Tauri IPC. Only this temporary fixture received fetch mutations. Existing application preferences, GitHub cache, and all Phase 2B retained fixtures were preserved.

- Before fetching, **Inspect sync** displayed branch `main`, cached `origin/main`, “Up to date with last fetched state,” one unstaged file, one untracked file, and unknown successful Stow fetch time. The bare source already had one additional commit, demonstrating that local inspection did not fetch implicitly.
- **Fetch selected remote** displayed a running operation, disabled conflicting controls, and a Cancel button, then completed at 100%. The UI changed to one commit behind the cached upstream while preserving dirty counts and the local HEAD.
- Read-only checks confirmed HEAD `54ac173bfe2cfe7bcd8e1ceb7c9c1320924b2906`, identical index SHA-256, unchanged dirty/untracked bytes, no checkout of the remote-added file, and updated `origin/main` at `d665e528f5f1997a7d57cf5017872ab1618a9885`. SQLite recorded `completed/verified` and refreshed the sync assessment.
- A second fetch finished before the automation could click Cancel. It is correctly recorded as another success; **live cancellation was not verified**. Cancellation/timeouts and retained-data behavior passed deterministic process/fixture tests.
- Temporarily renaming the fixture bare remote caused execution-time validation to reject the stale preflight with an actionable “inspect sync and review again” error. No new success journal appeared. The fixture remote was then restored without deleting data.
- The history UI displayed both verified attempts and retained metadata paths. Quit/relaunch preserved the registration, one-behind assessment, local inspection time, successful fetch time, and history. No automatic fetch occurred. The prior unfinished Phase 2B clone remained visible; the GitHub repository-cache timestamp stayed unchanged.

The fixture and its registration remain for review. Root allocation is **412 KiB**, including two retained metadata stages. This is temporary OS storage, not durable backup. No Phase 2B fixture was cleaned up.

### Resource measurements and limits

Same shared Cargo target before/after: **1,398,032 → 1,404,328 KiB allocated** (+6,296 KiB, about 6.15 MiB). Desktop bundle: **30.57 → 31.26 MiB** (+0.69 MiB). Frontend `dist`: **256 → 260 KiB allocated**; current JavaScript **255.97 kB / 78.82 kB gzip**. No additional target directory or installed dependency was introduced. `node_modules` measured 84,512 KiB at completion; allocation changes are not attributed to this feature. Runtime CPU/RAM and large-repository overhead were not benchmarked. Existing objects are shared with the private stage rather than copied; new objects, staged refs, and journals accumulate because there is no automatic cleanup/quota.

Git 2.55.0 and gh 2.102.0 were present. Fetch requires Git 2.46+ and deliberately rejects SSH/Enterprise/custom helpers, extra Git config, linked/shared worktrees, shallow/partial/alternate/replace-ref history, nonordinary refspecs, and unsafe filesystem metadata. Private/SSO HTTPS fetch was not exercised in this phase; successful local-bare fetch does not establish network-authentication coverage. Real UI cancellation, forced app termination mid-fetch, storage power loss, malicious same-user races, network/removable filesystems, release signing/notarization, and other platforms remain unverified. See the enforced scope and concurrency limits in `SECURITY.md`.

Recommended Phase 3B: separately reviewed fast-forward-only pull planning/execution for supported clean ordinary branches, with renewed identity/HEAD/upstream authorization and interruption/preservation tests. No Phase 3B work was started.

## Phase 3B — confirmed fast-forward pull (2026-10-16)

Implemented only the requested pull slice: locked preflight and explicit confirmation, constrained fetch, repeated source/index/ref/config validation, ancestry checks, exact-commit fast-forward integration, successful no-op, observed-state verification, durable pull history, startup reconciliation, and minimal Sync details controls. No dependency or capability changes, staging/commits/pushes in the Stow repository, or existing-fixture cleanup occurred. The entirely untracked source tree remains untracked.

### Files changed

- New integration policy and fixture coverage: `src-tauri/src/pull.rs`, `pull_tests.rs`.
- Reused fetch policy: `src-tauri/src/sync.rs` (held-lock entry point and actual advertised ref report), `sync_tests.rs` (test module).
- Persistence/IPC/queue: `src-tauri/migrations/005_pull.sql`, `src-tauri/src/database.rs`, `ipc.rs`, `main.rs`, `operations.rs`, `lib.rs`; migration version expectation in `github_tests.rs`.
- UI: `frontend/SyncDetails.tsx`, `sync.ts`, `api.ts`, `App.tsx`, `tests/sync.test.mjs`.
- Affected documentation: `README.md`, `docs/SECURITY.md`, `ARCHITECTURE.md`, `ROADMAP.md`, and this report. No clone implementation or existing clone tests were changed.

### Automated verification

| Check | Result |
| --- | --- |
| Pull fixture suite | 12 passed in the full run; additional tracked modification/rename/removal scenario verified separately |
| `cargo test --workspace --no-default-features --locked --offline -- --test-threads=4` | 90 passed (71 unit, 19 integration), 1 opt-in live clone test ignored |
| `npm run test:frontend` | 9 passed |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, including desktop IPC |
| `npm run build` | TypeScript and Vite passed |
| `npm run tauri -- build --debug --bundles app` | Passed; one macOS desktop build, 31.60 MiB bundle |

The first full run at default test concurrency encountered a `Busy` advisory-lock rejection in the existing `clone::tests::cancellation_after_verification_is_recoverable` test. The complete suite passed when rerun with four test threads. This is recorded as a concurrency-sensitive verification limitation, not silently counted as a clean first run; its underlying cause has not been conclusively established. No production lock rejection was weakened to make tests pass.

Coverage exercises fresh and cached-behind updates; equal-HEAD no-op; ahead-only/diverged refusal; staged/unstaged/untracked/ignored files; detached/missing/ambiguous upstream; unavailable/deleted remote branches; fetch failure/cancellation; queue and descriptor-lock contention; post-confirmation and post-fetch source changes; immediate pre-integration validation; hooks/filter/attributes/sparse/symlink/hidden-index rejection; interruption phases; database failure after integration; HEAD/index/files/unrelated branch preservation; durable history and refreshed registry state. Phase 3A fetch and Phase 2B clone regressions passed. During development, verbose fetch porcelain handling was corrected to preserve the space fast-forward flag and include unchanged advertised refs; cached deleted refs remain ineligible for pull.

### Real macOS desktop smoke

The final bundle ran against a new disposable source, local bare remote and registered workspace under `/private/tmp/stow-phase3b-desktop-k5_8jee6`. The native picker and actual Tauri IPC were used. No real user project or prior Phase 2B/3A fixture received Git mutations. The smoke fixture and its registration remain for review; no cleanup was performed.

- Initial **Inspect sync** showed cached up-to-date state while the local bare remote already had a newer commit, confirming no implicit fetch during inspection.
- **Pull (Fast-Forward Only)** reserved the operation and disabled conflicting controls. Confirmation displayed `main`, its exact starting HEAD, and `refs/remotes/origin/main`, plus the filesystem/cancellation explanation.
- **Decline pull** produced a durable `cancelled/awaiting_confirmation` record without a fetch or branch change.
- A new confirmed pull fetched and fast-forwarded from `f5cb5cf9dbf82beff28018ce511f7182ce73b3b1` to `3cf91ff0fc61da33c32b687f46baa06469bc4579`. UI progress reached completed/100%, and details/history refreshed to the target HEAD, clean counts, `up_to_date`, and a successful fetch timestamp.
- Independent checks confirmed the new file contents, clean Git status, index tree equal to HEAD's tree, and the unrelated branch still at the starting commit. Read-only SQLite inspection confirmed schema 5 and `completed/verified` with the target and observed HEAD.
- Another confirmed pull reported **Already up to date.** It performed a second fetch, preserved index bytes and working-file mtime, and persisted a second verified result.
- Stow was quit while a subsequent attempt waited for confirmation, before fetch/integration authorization. Relaunch preserved both verified successes and the declined record, converted the unfinished attempt to `requires_inspection`, displayed manual inspection guidance, and performed no extra fetch. The GitHub cache timestamp and earlier unfinished clone remained unchanged.

Live process cancellation during transfer, forced termination during working-tree mutation, storage power loss, private/SSO HTTPS pull, and non-macOS behavior were not exercised. Deterministic fixtures cover cancellation boundaries, uncertain durable outcomes, and restart classification; they do not establish OS crash atomicity. The separate integration process is intentionally not killed or timed out once started. See the concurrency and support limits in `SECURITY.md`.

### Resource usage and next slice

Shared Cargo target allocation: **1,404,328 → 1,407,396 KiB** (+3,068 KiB, approximately 3.00 MiB). Bundle: **31.26 → 31.60 MiB** (+0.34 MiB). Frontend `dist`: **260 → 264 KiB**; JavaScript **259.54 kB / 79.84 kB gzip** versus Phase 3A's 255.97 kB / 78.82 kB. Smoke root: **428 KiB**, including two retained fetch stages. No second target directory or dependency installation was introduced. Runtime CPU/RAM and large-repository traversal cost were not benchmarked; repeated validation and retained journals/staging add bounded per-check work and accumulating disk usage.

The MVP intentionally rejects many otherwise valid repositories, including ignored files, hooks, attributes, symlinks, submodules, custom settings and nonordinary indexes. Advisory locks coordinate Stow, not malicious same-user writers; external editors/Git must remain idle. Integration can wait indefinitely on stalled Git/storage, and HEAD/index/files/database updates are not atomic. Recommended next slice is separately reviewed explicit push preflight and authorization (Phase 3C); no Phase 3C or Stow Project work was begun.

## Phases 3C and 3D — authorized push and local commits (2026-10-17)

Implemented the two supplied slices: refreshed single-branch push authorization and exact remote-ref verification; bounded local change review, separate staged/working diffs, selective file staging/unstaging, and confirmed staged-content commits. They share workspace validation and the operation queue. No new dependencies, branch-management features, Stow Project, archive preflight, deletion, or automatic push were introduced.

### Changed components and policy

- Backend: new `workspace.rs`, `push.rs`, `changes.rs`, their fixture suites, and migrations 006/007. Updated sync policy reuse, transfer error classification, registry journals, operation recovery, and Tauri IPC/registration.
- Frontend: new `ChangesDetails.tsx` and change helpers; extended API types, SyncDetails push confirmation/history, App integration, minimal styling, and focused helper tests.
- Documentation: README, architecture, security, roadmap, and this verification record.

Push approval binds the repository, branch, exact HEAD and existing upstream destination after a constrained fetch. Execution refreshes and revalidates that state, requires ancestry, and publishes one explicit non-force refspec from private metadata. Independent remote-ref verification and durable refreshed sync state are required for verified success. Remote rejection, authentication/network errors before dispatch, cancellation, and uncertain publication remain distinct. Uncertain attempts never retry automatically. Working files and the original index are excluded from push mutation.

Change snapshots retain bounded file/index state in memory; filenames travel through NUL-delimited machine formats and literal pathspecs. Index edits are prepared privately, checked for stale state, and installed under the native index lock. Commit confirmation binds the staged tree, exact message, author, branch and parent. Publication reserves native index/HEAD locks and compare-and-swaps the approved direct branch. Hooks/signing are unsupported and project tests are not implicitly run. Author settings are read without rewriting configuration. The branch reflog records publication; the main HEAD reflog is preserved. Incomplete local operations require inspection, without resetting files or removing foreign locks.

### Automated verification

Before the final regression run, the complete suite passed 115 tests (96 unit and 19 integration), with one opt-in live clone test ignored. Subsequent focused verification passed all 13 then-existing change fixtures after durability/HEAD validation fixes, and a new no-remote commit/native-lock fixture passed separately. The final four-thread run passed 96 unit tests but failed the preexisting `clone::tests::root_lock_and_exclusive_promotion` test at immediate reacquisition after dropping a directory lock (`Busy`, line 196). The serial rerun result is recorded below. This resembles the earlier Phase 3B concurrency-sensitive lock limitation; its cause has not been conclusively established, and no production locking policy or existing test was weakened.

| Check | Result |
| --- | --- |
| `cargo test --workspace --no-default-features --locked --offline -- --test-threads=1` | Final serial run: 116 passed (97 unit, 19 integration); 1 opt-in live clone test ignored |
| `npm run test:frontend` | 12 passed |
| `cargo fmt --all -- --check` and `git diff --check` | Passed |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, including desktop IPC |
| `npm run build` | TypeScript and Vite passed |
| `npm run tauri -- build --debug --bundles app` | Passed; final macOS debug bundle 33.02 MiB |

Coverage includes fast-forward and no-op push; dirty-file preservation; stale HEAD/identity/destination and remote races; divergence and unsupported configuration; authentication/network classification; rejection versus uncertain verification; cancellation boundaries and journal failures/restart reconciliation. Local-change fixtures exercise modified/new/deleted/renamed/binary files; spaces, Unicode, tabs, newlines and pathspec characters; index-only unstage; staged-only commit with remaining unstaged files; missing author/empty message/empty tree; stale index/HEAD/author; native lock conflicts; hooks/signing/attributes/symlink/hidden-index restrictions; size bounds; compare-and-swap branch protection; deferred cancellation and uncertain durability; and operation without any remote. Existing clone, fetch, pull, discovery, registry and cache regressions are included.

Development testing caught Git's duplicate implicit HEAD update when combining symbolic HEAD verification with an update to its current branch. The final implementation uses private detached metadata with an explicit commondir, plus the real HEAD reservation and named-branch compare-and-swap. A dedicated fixture proves external branch switching is rejected while those native locks are held. Two desktop builds were necessary: the final rebuild incorporated corrected footer text and the durability outcome fix. No additional build directory was created.

### Real macOS desktop smoke

The final bundle was exercised through native macOS UI automation and actual Tauri IPC. A new disposable source, bare remote and registered workspace remain under `/private/tmp/stow-phase3cd-desktop-ft5x7h6v`. Prior fixtures, registrations and cached account data were preserved. Automated feature tests published only to disposable local bare remotes; separately authorized publication of Stow itself to GitHub is not counted as feature verification.

- Reviewed a working diff, selectively staged `initial.txt` and `new नमस्ते.txt`, unstaged the Unicode file, then restaged it. Unselected and ignored files remained intact.
- Changed `initial.txt` again after staging. Commit confirmation displayed the exact staged tree, parent, message and author. Declining persisted `cancelled/awaiting_confirmation` without moving HEAD.
- A renewed confirmed commit produced `5f821665cdf75beedbd5e941f83b44b10295a823`, parent `5323902365a4a82090adb201aa5b934f2a4f8a85`, tree `b1b56a42033b35b5786580a7ea37f3d7c02f5f54`. Independent inspection confirmed only staged content was committed; the later working edit, unselected file and ignored file survived. UI counts became zero staged, one unstaged, one untracked, and one commit ahead. The remote still held the parent, proving no implicit push.
- Push review showed the exact commit, `origin`, `refs/heads/main`, the fixture destination and one commit summary. Confirmation completed as `published_verified/verified`. Independent local-bare inspection matched the exact approved SHA; the UI became up to date while preserving working-file counts. The post-push index SHA-256 was `3d240ef89ea3b9ccdf844f5f70c9cba626b43b06547f12c06293a08afbfd228f`.
- Quit while a second push awaited confirmation. Relaunch marked it `uncertain/awaiting_confirmation`, retained the verified result and local change history, showed manual inspection guidance, and performed no extra fetch/retry. Schema 7 and persisted histories were checked read-only. Local and remote branch SHAs remained equal; working content remained unchanged. The prior unfinished clone and GitHub cache timestamp were preserved.

### Resource overhead, limits, and next slice

Shared Cargo target allocation measured **1,407,396 → 1,420,844 KiB** (+13,448 KiB, about 13.13 MiB) after the desktop build. Frontend `dist` grew **264 → 272 KiB**; JavaScript is **269.55 kB / 82.22 kB gzip**, CSS **3.79 kB / 1.46 kB gzip**. The app bundle grew **31.60 → 33.02 MiB** (+1.43 MiB allocated), and the retained smoke root measured **496 KiB**. These are disk measurements, not runtime CPU/RAM benchmarks; subsequent test artifacts can change target allocation. Private metadata, objects and journals accumulate without automatic cleanup.

Live private/SSO HTTPS push through Stow, forced termination during publication or local mutation, storage power loss, malicious same-user races, large repositories, network/removable filesystems, release signing/notarization, and other platforms remain unverified. Desktop restart coverage interrupted push before confirmation; interrupted commit recovery is fixture-tested. Local critical sections deliberately defer cancellation/timeouts and can wait on stalled Git/storage. HEAD, index, files, remote state and SQLite are not one atomic transaction. See `SECURITY.md` for supported repository/configuration and output limits.

Recommended Phase 4A is a separately scoped, read-only recoverability assessment with evidence freshness and explicit unknowns. No archive preflight, Stow Project, or deletion implementation was started.
