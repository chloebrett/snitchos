//! `satisfy` matches a child's declared `needs` (manifest `Slot`s) against the
//! capabilities a satisfier (init, the shell, a supervisor) already holds,
//! producing a per-slot grant plan in slot order — or the first slot it can't
//! satisfy (all-or-nothing). Pure: the host-testable core of the userspace
//! satisfier. Attenuation (a held cap wider than the slot asks) plans a mint.

use hitch::{satisfy, CapView, Grant, Slot, Unsatisfied};
use snitchos_abi::{object_kind, rights};

fn slot(name: &str, object: u32, r: u32) -> Slot {
    Slot { name: name.into(), object: object as u8, rights: r }
}

#[test]
fn exact_match_grants_the_held_handle() {
    // The satisfier holds exactly the slot's cap → delegate its handle as-is.
    let needs = [slot("fs", object_kind::ENDPOINT, rights::SEND)];
    let have = [CapView { object: object_kind::ENDPOINT as u8, rights: rights::SEND, handle: 7 }];
    assert_eq!(satisfy(&needs, &have), Ok(vec![Grant::Use { handle: 7 }]));
}

#[test]
fn unmatched_required_slot_is_unsatisfied() {
    let needs = [slot("fs", object_kind::ENDPOINT, rights::SEND)];
    let have: [CapView; 0] = [];
    assert_eq!(satisfy(&needs, &have), Err(Unsatisfied { slot: 0 }));
}

#[test]
fn a_wider_held_cap_plans_an_attenuated_mint() {
    // Hold SEND|RECV|MINT; the slot asks only SEND → mint an attenuated child
    // carrying exactly SEND (only ever attenuate, never amplify).
    let needs = [slot("fs", object_kind::ENDPOINT, rights::SEND)];
    let have = [CapView {
        object: object_kind::ENDPOINT as u8,
        rights: rights::SEND | rights::RECV | rights::MINT,
        handle: 3,
    }];
    assert_eq!(
        satisfy(&needs, &have),
        Ok(vec![Grant::Mint { from: 3, rights: rights::SEND }]),
    );
}

#[test]
fn plan_follows_slot_order() {
    let needs = [
        slot("a", object_kind::ENDPOINT, rights::SEND),
        slot("b", object_kind::ENDPOINT, rights::RECV),
    ];
    let have = [
        CapView { object: object_kind::ENDPOINT as u8, rights: rights::SEND, handle: 1 },
        CapView { object: object_kind::ENDPOINT as u8, rights: rights::RECV, handle: 2 },
    ];
    assert_eq!(
        satisfy(&needs, &have),
        Ok(vec![Grant::Use { handle: 1 }, Grant::Use { handle: 2 }]),
    );
}

#[test]
fn object_kind_mismatch_is_not_a_match() {
    // Right rights, wrong object kind → no match → unsatisfied.
    let needs = [slot("fs", object_kind::ENDPOINT, rights::SEND)];
    let have = [CapView { object: object_kind::NOTIFICATION as u8, rights: rights::SEND, handle: 9 }];
    assert_eq!(satisfy(&needs, &have), Err(Unsatisfied { slot: 0 }));
}
