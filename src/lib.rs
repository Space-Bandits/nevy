use bevy::{
    ecs::{
        component::{ComponentHook, HookContext, StorageType},
        relationship::Relationship,
        world::DeferredWorld,
    },
    prelude::*,
};

#[derive(Component, Default)]
#[relationship_target(relationship = ConnectionOf)]
pub struct EndpointOf(Vec<Entity>);

#[derive(Deref)]
pub struct ConnectionOf(pub Entity);

impl Default for ConnectionOf {
    fn default() -> Self {
        ConnectionOf(Entity::PLACEHOLDER)
    }
}

impl Component for ConnectionOf {
    const STORAGE_TYPE: StorageType = StorageType::SparseSet;

    type Mutability = bevy::ecs::component::Immutable;

    fn on_insert() -> Option<ComponentHook> {
        Some(|mut world: DeferredWorld, hook_context: HookContext| {
            <Self as Relationship>::on_insert(world.reborrow(), hook_context);

            let target_entity = world.entity(hook_context.entity).get::<Self>().unwrap().0;

            world.trigger_targets(NewConnectionOf(hook_context.entity), target_entity);
        })
    }

    fn on_replace() -> Option<ComponentHook> {
        Some(<Self as Relationship>::on_replace)
    }

    fn on_remove() -> Option<ComponentHook> {
        Some(|mut world: DeferredWorld, hook_context: HookContext| {
            let target_entity = world.entity(hook_context.entity).get::<Self>().unwrap().0;

            world.trigger_targets(RemovedConnectionOf(hook_context.entity), target_entity);
        })
    }
}

impl Relationship for ConnectionOf {
    type RelationshipTarget = EndpointOf;

    fn get(&self) -> Entity {
        self.0
    }

    fn from(entity: Entity) -> Self {
        ConnectionOf(entity)
    }
}

/// Event that is triggered when a new [ConnectionOf] component is inserted.
/// The target is the associated endpoint.
#[derive(Event)]
pub struct NewConnectionOf(pub Entity);

/// Event that is triggered when a [ConnectionOf] component is removed.
/// The target is the associated endpoint.
#[derive(Event)]
pub struct RemovedConnectionOf(pub Entity);
