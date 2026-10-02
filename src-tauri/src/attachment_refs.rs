//! Which attachments each conversation body names, kept while the bodies
//! themselves are not.
//!
//! The document snapshot holds conversations without their contexts: bodies
//! live in the conversation store, read through the shared memory pool
//! (`ConversationStore::conversation`). Attachment reclamation still has to
//! know every image and file any body names — the attachment directories
//! quarantine an entry nothing has named for a day — so the snapshot keeps
//! this index beside it: per conversation, the ids its contexts and branches
//! name. Queued messages stay on the snapshot's conversations and are read
//! from there.
//!
//! A body passes through the snapshot at startup, at every conversation
//! command (`conversations::sync_snapshot`) and when a workspace is re-read;
//! each of those records it and strips it. A conversation that arrives without
//! a body keeps what was recorded for it last, which errs towards keeping an
//! attachment rather than reclaiming one still in use.

use std::collections::{HashMap, HashSet};

use crate::model::{AppDocument, Conversation};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttachmentRefs {
    by_conversation: HashMap<String, BodyRefs>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct BodyRefs {
    images: HashSet<String>,
    files: HashSet<String>,
}

impl BodyRefs {
    fn of(conversation: &Conversation) -> Self {
        let mut refs = Self::default();
        let roots = std::iter::once(conversation.contexts.as_slice()).chain(
            conversation
                .branches
                .iter()
                .map(|branch| branch.contexts.as_slice()),
        );
        for contexts in roots {
            crate::image_attachments::collect_context_image_ids(contexts, &mut refs.images);
            crate::file_attachments::collect_context_file_ids(contexts, &mut refs.files);
        }
        refs
    }
}

/// Whether the conversation carries a body: contexts, or branches.
pub fn has_body(conversation: &Conversation) -> bool {
    !conversation.contexts.is_empty() || !conversation.branches.is_empty()
}

/// Drops the conversation's body — its contexts and its branches — keeping
/// metadata, settings and queued messages.
///
/// Branch records go with the contexts, not with the metadata: a branch is
/// anchored at a user message of the main timeline, so a conversation holding
/// branch records without that timeline fails its shape check, and a snapshot
/// holding one would refuse every save.
pub fn strip_body(conversation: &mut Conversation) {
    conversation.contexts = Vec::new();
    conversation.branches = Vec::new();
}

impl AttachmentRefs {
    /// Records `conversation`'s body as the authoritative one, also when it is
    /// empty.
    pub fn record(&mut self, conversation: &Conversation) {
        self.by_conversation
            .insert(conversation.id.clone(), BodyRefs::of(conversation));
    }

    /// Strips every body from `document`, recording each one that is present,
    /// and forgets conversations the document no longer holds.
    pub fn hollow(&mut self, document: &mut AppDocument) {
        let mut live = HashSet::new();
        for workspace in &mut document.workspaces {
            for conversation in &mut workspace.conversations {
                live.insert(conversation.id.clone());
                if has_body(conversation) {
                    self.record(conversation);
                    strip_body(conversation);
                }
            }
        }
        self.by_conversation.retain(|id, _| live.contains(id));
    }

    /// Forgets every conversation not in `present`.
    pub fn retain_conversations(&mut self, present: &HashSet<String>) {
        self.by_conversation.retain(|id, _| present.contains(id));
    }

    /// Every image id `document` names: queued messages and any body still on
    /// it, plus what is recorded for each of its conversations.
    pub fn image_ids(&self, document: &AppDocument) -> HashSet<String> {
        let mut ids = crate::image_attachments::referenced_image_ids(document);
        for refs in self.present(document) {
            ids.extend(refs.images.iter().cloned());
        }
        ids
    }

    /// [`Self::image_ids`] for file attachments.
    pub fn file_ids(&self, document: &AppDocument) -> HashSet<String> {
        let mut ids = crate::file_attachments::referenced_file_ids(document);
        for refs in self.present(document) {
            ids.extend(refs.files.iter().cloned());
        }
        ids
    }

    fn present<'a>(&'a self, document: &'a AppDocument) -> impl Iterator<Item = &'a BodyRefs> {
        document
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .filter_map(|conversation| self.by_conversation.get(&conversation.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContextItem, FileAttachment, FileAttachmentFormat, ImageAttachment};

    fn image(id: char) -> ImageAttachment {
        ImageAttachment {
            id: id.to_string().repeat(64),
            name: "a.png".into(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            bytes: 68,
            short_id: None,
        }
    }

    fn file(id: char) -> FileAttachment {
        FileAttachment {
            id: id.to_string().repeat(64),
            name: "a.txt".into(),
            format: FileAttachmentFormat::Text,
            bytes: 1,
            tokens: 1,
            pages: None,
        }
    }

    fn user_with(images: Vec<ImageAttachment>, files: Vec<FileAttachment>) -> ContextItem {
        ContextItem::User {
            id: "u".into(),
            content: "look".into(),
            images,
            files,
            created_at: "2026-09-30T00:00:00.000Z".into(),
        }
    }

    fn document_with_body() -> AppDocument {
        let mut document = crate::catalog::default_document();
        let conversation = &mut document.workspaces[0].conversations[0];
        conversation.contexts = vec![user_with(vec![image('a')], vec![file('f')])];
        document
    }

    #[test]
    fn hollowing_keeps_what_the_bodies_named() {
        let mut document = document_with_body();
        let full_images = crate::image_attachments::referenced_image_ids(&document);
        let full_files = crate::file_attachments::referenced_file_ids(&document);
        let mut refs = AttachmentRefs::default();
        refs.hollow(&mut document);
        assert!(document
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.conversations.iter())
            .all(|conversation| !has_body(conversation)));
        assert_eq!(refs.image_ids(&document), full_images);
        assert_eq!(refs.file_ids(&document), full_files);
        assert!(refs.image_ids(&document).contains(&"a".repeat(64)));
    }

    #[test]
    fn a_hollow_conversation_keeps_its_record_until_a_body_replaces_it() {
        let mut document = document_with_body();
        let mut refs = AttachmentRefs::default();
        refs.hollow(&mut document);
        // Committed again without a body: the record stands.
        refs.hollow(&mut document);
        assert!(refs.image_ids(&document).contains(&"a".repeat(64)));
        // An authoritative empty body replaces it.
        let emptied = document.workspaces[0].conversations[0].clone();
        refs.record(&emptied);
        assert!(!refs.image_ids(&document).contains(&"a".repeat(64)));
    }

    #[test]
    fn a_conversation_that_leaves_the_document_takes_its_record_along() {
        let mut document = document_with_body();
        let mut refs = AttachmentRefs::default();
        refs.hollow(&mut document);
        document.workspaces[0].conversations.clear();
        refs.hollow(&mut document);
        assert!(refs.by_conversation.is_empty());
    }
}
