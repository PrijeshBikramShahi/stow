# Roadmap

Completed: Phase 1 local discovery/inspection/registry and Phase 2A optional GitHub.com authentication discovery, manual paginated listing, durable account-aware metadata cache, and unified local/remote library; Phase 2B safe clone, durable recovery, workspace preferences, and editor/Finder handoff; Phase 3A structured synchronization preflight and explicitly authorized constrained fetch; Phase 3B confirmed fast-forward-only pull with durable conservative recovery. Later stages remain intentionally unimplemented:

1. **GitHub follow-ups:** verify live multi-account/SSO/Enterprise behavior, refine permission diagnostics and large-library handling, and consider generated IPC contracts. gh owns credential storage.
2. **Clone follow-ups:** broader filesystem/platform verification, real private/SSO clone coverage, stronger editor trust isolation, and large-repository resource limits.
3. **Commit, push and broader integration:** a separate authenticated execution policy, explicit mutation intent, status reconciliation, and failure recovery.
4. **Stow Project preflight:** assess unpublished branches/commits, dirty files, ignored files, submodules, linked worktrees, and recoverability. A clean status alone is insufficient.
5. **Safe workspace removal:** reviewed deletion plan, canonical-path/identity safeguards, restore guarantees, explicit confirmation, and crash-safe recovery.
6. **Remote restoration:** restore registered projects without losing identity, branch intent, or backups.
7. **Optional local-file backups:** opt-in coverage, encryption, manifests, integrity checks, retention, and tested restore.
8. **Storage optimization and hibernation:** measure first; selectively clean reproducible artifacts with safety verification.
9. **Productivity:** keyboard workflow, reusable CLI, optional automation, more Git providers, and cross-platform verification.
10. **Final UI refinement:** redesign presentation around proven backend workflows, accessibility, and measured usability.

Recommended next slice: design and review Phase 3C explicit push preflight/authorization, with remote identity, expected-old remote state, publication visibility, and interruption handling defined before mutation. Keep merge commits/rebase, automatic synchronization, archive/removal, and backup eligibility separately scoped. No Phase 3C or Stow Project implementation is included here.
