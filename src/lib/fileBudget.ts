/**
 * Limits on each file attached to a user message, mirroring Rust
 * `file_attachments.rs`. The host re-checks every one of them; these copies
 * let the renderer turn a file away before its bytes cross the bridge. How
 * many files a message carries is not budgeted; what they come to is, by
 * {@link MAX_MESSAGE_ATTACHMENT_BYTES}.
 */

/**
 * Largest stored text file — documents from before {@link MAX_TEXT_FILE_UPLOAD_BYTES}
 * may hold one this large — and largest text read out of a PDF.
 */
export const MAX_FILE_ATTACHMENT_TEXT_BYTES = 512 * 1024;
/** Largest text file a message takes now: Claude Code attaches none over 256 KB. */
export const MAX_TEXT_FILE_UPLOAD_BYTES = 262_144;
/** Claude Code's Read reads a whole PDF up to 20 MB. */
export const MAX_FILE_ATTACHMENT_PDF_BYTES = 20 * 1024 * 1024;
/**
 * A text file over this many tokens is sent as its first
 * {@link TRUNCATED_TEXT_FILE_LINES} lines, as Claude Code's Read cuts one; a file
 * still over it in those lines is not attached.
 */
export const MAX_TEXT_FILE_TOKENS = 25_000;
export const TRUNCATED_TEXT_FILE_LINES = 2_000;
export const MAX_FILE_ATTACHMENT_TOKENS = 10_000_000;
export const MAX_PDF_PAGES = 100_000;
/**
 * What one user message's attachments — images and files together — may come
 * to: what it already carries, as stored, and what is being added, as picked
 * from disk. Checked where files are attached, in the renderer only: the host
 * uploads one file at a time and never sees the message whole, and documents
 * from before this limit may carry more.
 */
export const MAX_MESSAGE_ATTACHMENT_BYTES = 32 * 1024 * 1024;