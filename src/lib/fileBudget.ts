/**
 * Limits on files attached to a user message, mirroring Rust
 * `file_attachments.rs`. The host re-checks every one of them; these copies
 * let the renderer turn a file away before its bytes cross the bridge.
 */

/** Files one user message, queued message or steer may carry. Images are counted separately. */
export const MAX_MESSAGE_FILES = 20;
/**
 * Largest text file, and largest text read out of a PDF. The model reads the
 * whole of it on every request, so this bounds what one attachment costs.
 */
export const MAX_FILE_ATTACHMENT_TEXT_BYTES = 512 * 1024;
export const MAX_FILE_ATTACHMENT_PDF_BYTES = 10 * 1024 * 1024;
/**
 * Estimated tokens of file text one message may carry in all. Twenty files at
 * the per-file limit would be ten megabytes the model rereads on every request,
 * past the 8 MiB the host lets one request's history serialize to; this keeps a
 * single message to about 2 MB of text.
 */
export const MAX_MESSAGE_FILE_TOKENS = 400_000;
export const MAX_FILE_ATTACHMENT_TOKENS = 10_000_000;
export const MAX_PDF_PAGES = 100_000;
/** Largest file the host will read back from a native drop. */
export const MAX_DROPPED_FILE_BYTES = 10 * 1024 * 1024;
