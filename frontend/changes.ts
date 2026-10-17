import type { ChangeReview, CommitPreview } from './api';
export function commitReason(review: ChangeReview | null, message: string): string | null {
 if (!review) return 'Refresh changes before preparing a commit.';
 if (!review.entries.some(e => e.staged)) return 'Stage files before preparing a commit.';
 if (!message.trim()) return 'Enter a commit message.';
 if (message.includes('\0') || new TextEncoder().encode(message).length > 65536) return 'Commit message must be at most 64 KiB without NUL characters.';
 if (!review.author) return review.author_error?.message ?? 'Configure a Git author name and email in your terminal.';
 return null;
}
export function pendingCommit(preview: CommitPreview | null | undefined, operation?: {kind: string; state: string}): CommitPreview | null {
 return operation?.kind === 'commit' && operation.state === 'running' && preview?.job.state === 'running' && preview.job.phase === 'awaiting_confirmation' ? preview : null;
}
// Quoted display makes tabs/newlines visible while the exact original string is
// sent back to Rust. Never trim or split paths for selection.
export function displayPath(path: string): string { return JSON.stringify(path); }
