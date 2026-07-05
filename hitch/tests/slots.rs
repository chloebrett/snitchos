//! `resolve_slot` maps a role name (from a program's `#[entry(needs = […])]` slot
//! table) to its handle index, checking the requested object kind matches the
//! declared one. The runtime's `bootstrap().get::<T>(name)` is a thin wrapper over
//! this: index → `delegated_handle(index)`.

use hitch::{resolve_slot, SlotError};
use snitchos_abi::object_kind;

#[test]
fn resolve_slot_finds_a_declared_role_at_its_index() {
    let slots = &[
        ("fs", object_kind::ENDPOINT as u8),
        ("log", object_kind::ENDPOINT as u8),
    ];
    // Declaration order is handle order: `log` is the second slot → index 1.
    assert_eq!(resolve_slot(slots, "log", object_kind::ENDPOINT as u8), Ok(1));
}

#[test]
fn resolve_slot_missing_role_is_not_found() {
    let slots = &[("fs", object_kind::ENDPOINT as u8)];
    assert_eq!(
        resolve_slot(slots, "nope", object_kind::ENDPOINT as u8),
        Err(SlotError::NotFound),
    );
}

#[test]
fn resolve_slot_wrong_object_is_rejected() {
    // The role exists, but asked for as the wrong capability type — reject it, don't
    // hand back a handle the program would misuse.
    let slots = &[("fs", object_kind::ENDPOINT as u8)];
    assert_eq!(
        resolve_slot(slots, "fs", object_kind::NOTIFICATION as u8),
        Err(SlotError::WrongObject {
            declared: object_kind::ENDPOINT as u8,
            wanted: object_kind::NOTIFICATION as u8,
        }),
    );
}
