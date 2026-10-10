//! Account and custodian authorization policy.
//!
//! Commit has no organizations. Work belongs to the account that created it,
//! is shared explicitly (assignment, project membership), and is visible to
//! the custodian circle as the database functions `commit.todo_access`,
//! `commit.project_access` and `commit.project_writable` define. The rules
//! here decide what an already-visible caller may change; every one of them
//! applies the custodian rule through [`VerifiedActor::acts_for`]: a Carbon
//! manages its Silicons' work in Commit, always as itself.

use crate::{
    application::ports::VerifiedActor,
    domain::{Todo, ValidatedTodoPatch},
};

/// Whether a caller may apply every field in one todo patch.
///
/// Content (title, description, assignee, attachments, project link) belongs
/// to the owner; status may also be moved by the assignee. Custodians stand in
/// for their Silicons on both sides.
#[must_use]
pub fn can_patch_todo(actor: &VerifiedActor, todo: &Todo, patch: &ValidatedTodoPatch) -> bool {
    let for_owner = actor.acts_for(&todo.assigned_by.uuid);
    let for_assignee = actor.acts_for(&todo.assigned_to.uuid);
    let changes_owner_fields = patch.title.is_some()
        || !patch.description.is_absent()
        || patch.assigned_to.is_some()
        || patch.attachments.is_some()
        || !patch.project_id.is_absent();

    if changes_owner_fields && !for_owner {
        return false;
    }
    if patch.status.is_some() && !(for_owner || for_assignee) {
        return false;
    }
    true
}

/// Whether a caller may delete a todo: its owner, or the owner's custodian.
#[must_use]
pub fn can_delete_todo(actor: &VerifiedActor, todo: &Todo) -> bool {
    actor.acts_for(&todo.assigned_by.uuid)
}

/// Whether a caller may append a note: owner, assignee, or a custodian of either.
#[must_use]
pub fn can_add_todo_note(actor: &VerifiedActor, todo: &Todo) -> bool {
    actor.acts_for(&todo.assigned_by.uuid) || actor.acts_for(&todo.assigned_to.uuid)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::OffsetDateTime;

    use super::{can_add_todo_note, can_delete_todo, can_patch_todo};
    use crate::{
        application::ports::{Grant, VerifiedActor},
        domain::{
            AccountUuid, Actor, ActorId, ActorType, LimitedText, RequiredText, Todo, TodoId,
            TodoStatus, ValidatedTodoPatch,
        },
    };

    fn actor(uuid: &str, id: &str, kind: ActorType) -> Actor {
        Actor::new(
            AccountUuid::new(uuid).unwrap_or_else(|error| panic!("{error}")),
            kind,
            ActorId::new(id).unwrap_or_else(|error| panic!("{error}")),
        )
    }

    fn verified(actor: Actor, managed: &[&str]) -> VerifiedActor {
        VerifiedActor::new(
            actor,
            Grant::Bearer {
                issued_at: None,
                family: None,
            },
        )
        .with_managed_silicons(
            managed
                .iter()
                .map(|uuid| AccountUuid::new(*uuid).unwrap_or_else(|error| panic!("{error}")))
                .collect::<BTreeSet<_>>(),
        )
    }

    fn todo(owner: Actor, assignee: Actor) -> Todo {
        Todo {
            project_id: None,
            id: TodoId::new(),
            title: RequiredText::new("title", "Task", 500).unwrap_or_else(|e| panic!("{e}")),
            description: LimitedText::new("description", "Context", 500).ok(),
            assigned_to: assignee,
            assigned_by: owner,
            status: TodoStatus::YetToDo,
            attachments: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn status_patch() -> ValidatedTodoPatch {
        ValidatedTodoPatch {
            status: Some(TodoStatus::InProgress),
            ..ValidatedTodoPatch::default()
        }
    }

    fn title_patch() -> ValidatedTodoPatch {
        ValidatedTodoPatch {
            title: RequiredText::new("title", "Changed", 500).ok(),
            ..ValidatedTodoPatch::default()
        }
    }

    #[test]
    fn assignee_can_change_status_but_not_content_or_delete() {
        let owner = actor("Own", "c:owner", ActorType::Carbon);
        let assignee = actor("Asg", "si:worker", ActorType::Silicon);
        let todo = todo(owner, assignee.clone());
        let caller = verified(assignee, &[]);

        assert!(can_patch_todo(&caller, &todo, &status_patch()));
        assert!(!can_patch_todo(&caller, &todo, &title_patch()));
        assert!(!can_delete_todo(&caller, &todo));
        assert!(can_add_todo_note(&caller, &todo));
    }

    #[test]
    fn a_custodian_manages_its_silicons_todos_as_itself() {
        let silicon = actor("Sil", "si:scout", ActorType::Silicon);
        let stranger = actor("Str", "c:stranger", ActorType::Carbon);
        let todo = todo(silicon.clone(), stranger);
        let custodian = verified(actor("Cus", "c:ada", ActorType::Carbon), &["Sil"]);

        assert!(can_patch_todo(&custodian, &todo, &title_patch()));
        assert!(can_delete_todo(&custodian, &todo));
        assert!(can_add_todo_note(&custodian, &todo));
    }

    #[test]
    fn outsiders_cannot_change_anything() {
        let todo = todo(
            actor("Own", "c:owner", ActorType::Carbon),
            actor("Asg", "c:assignee", ActorType::Carbon),
        );
        let outsider = verified(actor("Out", "c:outsider", ActorType::Carbon), &["Other"]);

        assert!(!can_patch_todo(&outsider, &todo, &status_patch()));
        assert!(!can_patch_todo(&outsider, &todo, &title_patch()));
        assert!(!can_delete_todo(&outsider, &todo));
        assert!(!can_add_todo_note(&outsider, &todo));
    }
}
