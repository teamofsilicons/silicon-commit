//! Relationship and IAM-capability authorization policy.

use crate::{
    application::ports::VerifiedActor,
    domain::{Project, Todo, ValidatedTodoPatch},
};

/// Whether a caller may read organization-visible work.
///
/// Reaching this policy means IAM already verified an active membership for the
/// exact organization, so public-in-organization resources are readable.
#[must_use]
pub const fn can_read_organization_work(_actor: &VerifiedActor) -> bool {
    true
}

/// Whether a caller may apply every field in one todo patch.
#[must_use]
pub fn can_patch_todo(actor: &VerifiedActor, todo: &Todo, patch: &ValidatedTodoPatch) -> bool {
    if actor.manages_todos() {
        return true;
    }

    let is_assigner = actor.actor.principal_id == todo.assigned_by.principal_id;
    let is_assignee = actor.actor.principal_id == todo.assigned_to.principal_id;
    let changes_assigner_owned_fields = patch.title.is_some()
        || !patch.description.is_absent()
        || patch.assigned_to.is_some()
        || patch.attachments.is_some()
        || !patch.project_id.is_absent();

    if changes_assigner_owned_fields && !is_assigner {
        return false;
    }
    if patch.status.is_some() && !(is_assigner || is_assignee) {
        return false;
    }
    true
}

/// Whether a caller may delete a todo.
#[must_use]
pub fn can_delete_todo(actor: &VerifiedActor, todo: &Todo) -> bool {
    actor.manages_todos() || actor.actor.principal_id == todo.assigned_by.principal_id
}

/// Whether a caller may append an immutable todo note.
#[must_use]
pub fn can_add_todo_note(actor: &VerifiedActor, todo: &Todo) -> bool {
    actor.manages_todos()
        || actor.actor.principal_id == todo.assigned_by.principal_id
        || actor.actor.principal_id == todo.assigned_to.principal_id
}

/// Whether a caller may mutate one Silicon-managed project.
#[must_use]
pub fn can_mutate_project(actor: &VerifiedActor, project: &Project) -> bool {
    !project.details.private
        || project.created_by.principal_id == actor.actor.principal_id
        || project.has_participant(&actor.actor)
        || project
            .details
            .tags
            .iter()
            .any(|tag| actor.tags.contains(tag))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{can_add_todo_note, can_delete_todo, can_patch_todo};
    use crate::{
        application::ports::{
            CapabilitySet, InboundCredential, OrganizationRole, TrustedIdentity, VerifiedActor,
        },
        domain::{
            Actor, ActorId, ActorType, LimitedText, OrganizationId, PrincipalId,
            PublicOrganizationId, RequiredText, Todo, TodoId, TodoStatus, ValidatedTodoPatch,
        },
    };

    fn actor(id: &str, principal: u128) -> Option<Actor> {
        Some(Actor::new(
            PrincipalId::from_uuid(Uuid::from_u128(principal)),
            ActorType::Carbon,
            ActorId::new(id).ok()?,
        ))
    }

    fn verified(actor: Actor) -> Option<VerifiedActor> {
        let organization_id = OrganizationId::from_uuid(Uuid::from_u128(100));
        let org_id = PublicOrganizationId::new("example").ok()?;
        let identity = TrustedIdentity {
            organization_id,
            org_id: org_id.clone(),
            membership_id: Uuid::from_u128(200),
            actor: actor.clone(),
            organization_role: OrganizationRole::Member,
            capabilities: CapabilitySet::try_from_names(BTreeSet::<String>::new()).ok()?,
        };
        Some(VerifiedActor::new(
            organization_id,
            org_id,
            identity.membership_id,
            actor,
            OrganizationRole::Member,
            identity.capabilities.clone(),
            InboundCredential::trusted(identity),
        ))
    }

    fn todo(assigner: Actor, assignee: Actor) -> Option<Todo> {
        Some(Todo {
            project_id: None,
            id: TodoId::new(),
            organization_id: OrganizationId::from_uuid(Uuid::from_u128(100)),
            org_id: PublicOrganizationId::new("example").ok()?,
            title: RequiredText::new("title", "Task", 500).ok()?,
            description: Some(LimitedText::new("description", "Context", 500).ok()?),
            assigned_to: assignee,
            assigned_by: assigner,
            status: TodoStatus::YetToDo,
            attachments: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        })
    }

    #[test]
    fn assignee_can_change_status_but_not_content_or_delete() {
        let Some(assigner) = actor("assigner", 1) else {
            return;
        };
        let Some(assignee) = actor("assignee", 2) else {
            return;
        };
        let Some(todo) = todo(assigner, assignee.clone()) else {
            return;
        };
        let Some(caller) = verified(assignee) else {
            return;
        };
        let status_patch = ValidatedTodoPatch {
            status: Some(TodoStatus::InProgress),
            ..ValidatedTodoPatch::default()
        };
        let title_patch = ValidatedTodoPatch {
            title: RequiredText::new("title", "Changed", 500).ok(),
            ..ValidatedTodoPatch::default()
        };

        assert!(can_patch_todo(&caller, &todo, &status_patch));
        assert!(!can_patch_todo(&caller, &todo, &title_patch));
        assert!(!can_delete_todo(&caller, &todo));
        assert!(can_add_todo_note(&caller, &todo));
    }
}
