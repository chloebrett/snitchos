//! Lexical environment: an immutable chain of `name → Value` bindings plus a
//! shared, write-once table of top-level (global) definitions.
//!
//! Each `extend` returns a new `Env` that shares its tail (and the globals
//! slot) via `Rc`, so entering a scope — and capturing one in a closure — is
//! cheap and never mutates an existing binding. Lexical lookup walks the chain
//! from the most recent binding (so shadowing falls out for free); a miss falls
//! through to the globals. The globals are an `Rc<OnceCell<…>>` so that the
//! top-level functions, which all capture this env *before* the table is built,
//! end up sharing one fully-populated table — that shared table is what makes
//! recursion and mutual recursion work (letrec at the top level).

use core::str;
use core::cell::{Cell, OnceCell, RefCell};

use alloc::rc::Weak;

use alloc::collections::{BTreeMap, BTreeSet};

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::Method;
use crate::platform::{NullPlatform, Platform};
use crate::source::SourceId;
use crate::telemetry::{RecordingTelemetry, Telemetry};
use crate::value::{ClosureData, Frame, TelemetryEvent, Value};

/// Why an assignment failed — formatted into a message by the interpreter.
pub enum AssignError {
    /// No binding of that name in scope.
    Unbound,
    /// The binding exists but wasn't declared `mut`.
    Immutable,
}

#[derive(Clone)]
pub struct Env {
    locals: Option<Rc<Scope>>,
    globals: Rc<OnceCell<BTreeMap<String, Value>>>,
    methods: Rc<OnceCell<BTreeMap<String, Vec<Method>>>>,
    /// Per-variant field mutability: variant name → field name → is `mut`. The
    /// source of truth for whether a field may be assigned. (Keyed by variant so
    /// each sum variant's fields are tracked independently; for a `prod` the
    /// variant name is the type name.)
    field_mut: Rc<OnceCell<BTreeMap<String, BTreeMap<String, bool>>>>,
    /// Where `emit`/`span` send their telemetry, shared across the whole program
    /// run (every scope and closure points at the same backend). Defaults to
    /// [`RecordingTelemetry`]; the on-target build installs a syscall-backed one
    /// via [`Env::with_telemetry`].
    telemetry: Rc<dyn Telemetry>,
    /// Where console / capability / process / filesystem effects go, shared
    /// across the whole run like [`telemetry`](Self::telemetry). Defaults to
    /// [`NullPlatform`] (discards output, reads nothing); the on-target build
    /// installs a syscall-backed one via [`Env::with_platform`].
    platform: Rc<dyn Platform>,
    /// The capabilities in scope — the authority the running code may exercise
    /// (e.g. `Telemetry` to call `emit`/`span`). A named function's body runs
    /// with exactly its declared `uses` (set via [`Env::with_authority`] at the
    /// call boundary); lambdas and inner scopes inherit through `extend`. The
    /// program entry / REPL prompt is seeded with the process's ambient caps.
    authority: Rc<BTreeSet<String>>,
    /// Remaining evaluation steps — a budget decremented once per `eval` call,
    /// shared across the whole run via `Rc` like the sinks above. The default is
    /// effectively unbounded (`u64::MAX`); [`with_fuel`](Self::with_fuel) sets a
    /// finite budget so a non-terminating program faults instead of hanging (or
    /// overflowing the Rust stack). This is the hook the Stitch mutation tester's
    /// fuel cap rides.
    fuel: Rc<Cell<u64>>,
    /// The active call stack, run-shared like `fuel`. Each closure application
    /// pushes a [`Frame`]; the guard pops it. `frames.len()` is the recursion depth
    /// (the backstop that turns unbounded non-tail recursion into a catchable fault),
    /// and the whole stack is snapshotted onto a fault to form its backtrace.
    frames: Rc<RefCell<Vec<Frame>>>,
    /// The closure currently being applied — set by `apply_values` before
    /// evaluating a closure body so that `eval_tail` can detect self-tail-calls
    /// and signal the trampoline instead of recursing into Rust.
    self_closure: Option<Rc<ClosureData>>,
    /// Which source the code running in this scope came from — set at each closure
    /// boundary (`apply_values`) and at program registration (`build_env`). `eval`
    /// stamps it onto a fault so the fault's span can be resolved to `file:line:col`.
    source: SourceId,
    /// Dynamically-scoped effect handlers: a stack of `(op-name, handler value)`.
    /// `handle op with f { … }` pushes `(op, f)` for the block's extent (via a scoped
    /// env clone). An effect native dispatches to the topmost handler for its op
    /// before falling to the ambient platform. Threaded through calls (not reset at
    /// boundaries) so handlers are dynamic, not lexical.
    handlers: Rc<Vec<(String, Value)>>,
}

/// Pops [`Env::enter_call`]'s pushed frame when dropped — so the call stack is
/// unwound on *every* exit from a call, error paths (`?`) included.
pub struct CallGuard {
    frames: Rc<RefCell<Vec<Frame>>>,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.frames.borrow_mut().pop();
    }
}

impl Default for Env {
    fn default() -> Self {
        Env::with_telemetry(Rc::new(RecordingTelemetry::default()))
    }
}

struct Scope {
    name: String,
    /// The binding's value lives in a shared cell, so a `mut` binding reassigned
    /// here is visible through every clone of this scope — including closures
    /// that captured it (capture-by-reference). Immutable bindings use a cell
    /// too, but `assign` refuses them, so it never changes.
    value: Rc<RefCell<Value>>,
    mutable: bool,
    parent: Option<Rc<Scope>>,
}

impl Env {
    /// The empty environment, recording telemetry in memory.
    pub fn new() -> Self {
        Env::default()
    }

    /// An empty environment whose `emit`/`span` route to `telemetry`. The seam
    /// for swapping the in-memory recorder for the on-target, syscall-backed
    /// backend without the interpreter knowing which it has.
    #[must_use]
    pub fn with_telemetry(telemetry: Rc<dyn Telemetry>) -> Env {
        Env {
            locals: None,
            globals: Rc::new(OnceCell::new()),
            methods: Rc::new(OnceCell::new()),
            field_mut: Rc::new(OnceCell::new()),
            telemetry,
            platform: Rc::new(NullPlatform),
            authority: Rc::new(BTreeSet::new()),
            fuel: Rc::new(Cell::new(u64::MAX)),
            frames: Rc::new(RefCell::new(Vec::new())),
            self_closure: None,
            source: SourceId::default(),
            handlers: Rc::new(Vec::new()),
        }
    }

    /// A clone of this environment with a finite evaluation budget of `steps` —
    /// seed it *before* building the program env so every closure captures the
    /// same budgeted counter. Consumed one step per `eval` call; on exhaustion
    /// [`take_fuel`](Self::take_fuel) reports empty and the interpreter faults.
    #[must_use]
    pub fn with_fuel(self, steps: u64) -> Env {
        Env { fuel: Rc::new(Cell::new(steps)), ..self }
    }

    /// Consume one unit of evaluation fuel. Returns `false` when the budget is
    /// exhausted (the caller should fault), `true` otherwise. Decrements the
    /// run-shared counter in place.
    #[must_use]
    pub fn take_fuel(&self) -> bool {
        let remaining = self.fuel.get();
        if remaining == 0 {
            return false;
        }
        self.fuel.set(remaining - 1);
        true
    }

    /// The maximum nested-call depth before a fault — a backstop that converts a
    /// Rust-stack overflow (unbounded non-tail recursion) into a catchable error.
    /// Conservative and target-dependent: on-target stacks are far smaller than
    /// the host's, and the real fix for *tail* recursion is the trampoline. Tune
    /// here.
    const MAX_CALL_DEPTH: u32 = 48;

    /// Mark `closure` as the function currently being applied — so that
    /// `eval_tail` can recognise a self-tail-call by pointer identity and signal
    /// the trampoline instead of recursing into Rust.
    #[must_use]
    pub fn with_self_closure(self, closure: Rc<ClosureData>) -> Env {
        Env { self_closure: Some(closure), ..self }
    }

    /// Returns `true` if `c` is pointer-equal to the closure this environment
    /// was set up for (i.e., we're about to make a self-tail-call).
    pub fn is_self_closure(&self, c: &Rc<ClosureData>) -> bool {
        self.self_closure.as_ref().is_some_and(|s| Rc::ptr_eq(s, c))
    }

    /// Enter a nested call: push a [`Frame`] for the function named `name` onto the
    /// run-shared call stack and hand back a [`CallGuard`] that pops it on drop (so
    /// the stack tracks *current* nesting on every exit path). `None` if entering
    /// would exceed [`MAX_CALL_DEPTH`] — the caller should fault rather than recurse
    /// into an overflow.
    #[must_use]
    pub fn enter_call(&self, name: Option<String>) -> Option<CallGuard> {
        let mut frames = self.frames.borrow_mut();
        if frames.len() as u32 >= Self::MAX_CALL_DEPTH {
            return None;
        }
        frames.push(Frame { name });
        drop(frames);
        Some(CallGuard { frames: Rc::clone(&self.frames) })
    }

    /// A snapshot of the current call stack (innermost-last) — captured onto a fault
    /// at the raise point to form its backtrace.
    #[must_use]
    pub fn frames_snapshot(&self) -> Vec<Frame> {
        self.frames.borrow().clone()
    }

    /// A clone of this environment whose console / capability / process / FS
    /// effects route to `platform` — the seam for swapping the no-op default for
    /// the on-target, syscall-backed backend (or a host fake) without the
    /// interpreter knowing which it has. Inner scopes inherit it.
    #[must_use]
    pub fn with_platform(self, platform: Rc<dyn Platform>) -> Env {
        Env { platform, ..self }
    }

    /// The installed effect backend — what the console / cap / proc / FS natives
    /// call through.
    #[must_use]
    pub fn platform(&self) -> &dyn Platform {
        self.platform.as_ref()
    }

    /// A shared handle to the platform backend — for building a *fresh* env (a
    /// separately-namespaced `~>` stage) that still routes effects to the same
    /// place. Prefer [`platform`](Self::platform) for a one-off call.
    #[must_use]
    pub fn platform_rc(&self) -> Rc<dyn Platform> {
        Rc::clone(&self.platform)
    }

    /// A shared handle to the telemetry backend — the twin of
    /// [`platform_rc`](Self::platform_rc) for building a stage env whose
    /// `emit`/`span` reach the caller's sink.
    #[must_use]
    pub fn telemetry_rc(&self) -> Rc<dyn Telemetry> {
        Rc::clone(&self.telemetry)
    }

    /// A clone of this environment carrying `authority` as its capability set —
    /// the call-boundary primitive for a named function (its body runs with
    /// exactly its declared `uses`, replacing the caller's). Inner scopes inherit
    /// it via [`extend`](Self::extend).
    #[must_use]
    pub fn with_authority(self, authority: BTreeSet<String>) -> Env {
        Env { authority: Rc::new(authority), ..self }
    }

    /// A clone of this environment tagged with `source` — the source the code
    /// running in it came from. Set at each closure boundary (`apply_values`) and at
    /// program registration (`build_env`); read by `eval` when stamping a fault.
    #[must_use]
    pub fn with_source(self, source: SourceId) -> Env {
        Env { source, ..self }
    }

    /// The source the code in this scope came from.
    #[must_use]
    pub fn source(&self) -> SourceId {
        self.source
    }

    /// A clone of this environment with `handler` installed for effect op `op`,
    /// pushed on top of the dynamically-scoped handler stack. The returned env's
    /// dynamic extent (the block it evaluates) is where the handler is active.
    #[must_use]
    pub fn with_handler(self, op: String, handler: Value) -> Env {
        let mut handlers = (*self.handlers).clone();
        handlers.push((op, handler));
        Env { handlers: Rc::new(handlers), ..self }
    }

    /// The topmost handler installed for effect op `op`, if any — what an effect
    /// native dispatches to before the ambient platform.
    #[must_use]
    pub fn handler_for(&self, op: &str) -> Option<Value> {
        self.handlers
            .iter()
            .rev()
            .find(|(name, _)| name == op)
            .map(|(_, handler)| handler.clone())
    }

    /// A clone with the topmost handler for `op` removed — the env a handler runs
    /// in, so performing `op` again forwards to the next handler down (or ambient)
    /// instead of re-entering the same handler (shallow semantics).
    #[must_use]
    pub fn without_top_handler(&self, op: &str) -> Env {
        let mut handlers = (*self.handlers).clone();
        if let Some(pos) = handlers.iter().rposition(|(name, _)| name == op) {
            handlers.remove(pos);
        }
        Env { handlers: Rc::new(handlers), ..self.clone() }
    }

    /// Whether capability `cap` is in scope — the gate `emit`/`span` consult.
    #[must_use]
    pub fn has_authority(&self, cap: &str) -> bool {
        self.authority.contains(cap)
    }

    /// An environment sharing this one's globals, methods, and telemetry sink
    /// but with **no locals**. Used to run a top-level definition's body (a
    /// method, say) in global scope rather than the caller's lexical scope — the
    /// same hygiene a closure gets by capturing its own defining env instead of
    /// the caller's. Globals/methods stay reachable; the caller's locals don't
    /// leak in.
    #[must_use]
    pub fn globals_only(&self) -> Env {
        Env {
            locals: None,
            globals: Rc::clone(&self.globals),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            telemetry: Rc::clone(&self.telemetry),
            platform: Rc::clone(&self.platform),
            authority: Rc::clone(&self.authority),
            fuel: Rc::clone(&self.fuel),
            frames: Rc::clone(&self.frames),
            self_closure: None,
            source: self.source,
            handlers: Rc::clone(&self.handlers),
        }
    }

    /// A sibling environment for another module: its own (fresh, not-yet-set)
    /// globals slot, but **sharing** this one's method/field-mutability tables and
    /// telemetry sink. Method dispatch is by runtime type name into one
    /// program-wide table, and telemetry is program-wide, so those are shared
    /// across every module; only the value namespace (`globals`) is per-module.
    #[must_use]
    pub fn sibling_module(&self) -> Env {
        Env {
            locals: None,
            globals: Rc::new(OnceCell::new()),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            telemetry: Rc::clone(&self.telemetry),
            platform: Rc::clone(&self.platform),
            authority: Rc::clone(&self.authority),
            fuel: Rc::clone(&self.fuel),
            frames: Rc::clone(&self.frames),
            self_closure: None,
            source: self.source,
            handlers: Rc::clone(&self.handlers),
        }
    }

    /// A new environment with an immutable `name` binding, shadowing any
    /// existing binding and sharing the same globals + sink.
    #[must_use]
    pub fn extend(&self, name: String, value: Value) -> Env {
        self.bind(name, value, false)
    }

    /// As [`extend`](Self::extend), but the binding is `mut` (assignable).
    #[must_use]
    pub fn extend_mut(&self, name: String, value: Value) -> Env {
        self.bind(name, value, true)
    }

    fn bind(&self, name: String, value: Value, mutable: bool) -> Env {
        Env {
            locals: Some(Rc::new(Scope {
                name,
                value: Rc::new(RefCell::new(value)),
                mutable,
                parent: self.locals.clone(),
            })),
            globals: Rc::clone(&self.globals),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            telemetry: Rc::clone(&self.telemetry),
            platform: Rc::clone(&self.platform),
            authority: Rc::clone(&self.authority),
            fuel: Rc::clone(&self.fuel),
            frames: Rc::clone(&self.frames),
            self_closure: None,
            source: self.source,
            handlers: Rc::clone(&self.handlers),
        }
    }

    /// Reassign an existing `mut` binding in place (mutating its shared cell, so
    /// the change is visible through every holder of this scope).
    ///
    /// # Errors
    /// `Unbound` if no such binding; `Immutable` if it isn't `mut`.
    pub fn assign(&self, name: &str, value: Value) -> Result<(), AssignError> {
        let mut current = &self.locals;
        while let Some(scope) = current {
            if scope.name == name {
                if !scope.mutable {
                    return Err(AssignError::Immutable);
                }
                *scope.value.borrow_mut() = value;
                return Ok(());
            }
            current = &scope.parent;
        }
        Err(AssignError::Unbound)
    }

    /// Open a span on the installed telemetry backend.
    pub fn span_open(&self, name: &str) {
        self.telemetry.span_open(name);
    }

    /// Close the most recently opened span on the installed backend.
    pub fn span_close(&self, name: &str) {
        self.telemetry.span_close(name);
    }

    /// Emit a metric sample on the installed backend.
    pub fn emit_metric(&self, name: &str, value: &Value) {
        self.telemetry.emit(name, value);
    }

    /// A snapshot of all telemetry recorded so far (empty for non-recording
    /// backends).
    pub fn telemetry(&self) -> Vec<TelemetryEvent> {
        self.telemetry.snapshot()
    }

    /// Drain the recorded telemetry: return everything and clear it. Lets a
    /// long-lived REPL env render just *this line's* events without the previous
    /// lines' accumulating. Empty for non-recording backends.
    pub fn take_telemetry(&self) -> Vec<TelemetryEvent> {
        self.telemetry.drain()
    }

    /// The value of the nearest local binding of `name`, else a global of that
    /// name, else `None`.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        let mut current = &self.locals;
        while let Some(scope) = current {
            if scope.name == name {
                return Some(scope.value.borrow().clone());
            }
            current = &scope.parent;
        }
        self.globals
            .get()
            .and_then(|globals| globals.get(name).cloned())
    }

    /// Look up a name in the *local* scope chain (not globals) and return its
    /// shared cell and mutability flag — for building closure upvalues without
    /// capturing the full env.
    pub fn lookup_local_cell(&self, name: &str) -> Option<(Rc<RefCell<Value>>, bool)> {
        let mut current = &self.locals;
        while let Some(scope) = current {
            if scope.name == name {
                return Some((Rc::clone(&scope.value), scope.mutable));
            }
            current = &scope.parent;
        }
        None
    }

    /// Bind `name` to an *existing* cell (shared `Rc<RefCell<Value>>`), rather
    /// than creating a new one. Used when restoring upvalues at closure call time
    /// so mutable captures see mutations made after the closure was created.
    #[must_use]
    pub fn extend_cell(&self, name: String, cell: Rc<RefCell<Value>>, mutable: bool) -> Env {
        Env {
            locals: Some(Rc::new(Scope { name, value: cell, mutable, parent: self.locals.clone() })),
            globals: Rc::clone(&self.globals),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            telemetry: Rc::clone(&self.telemetry),
            platform: Rc::clone(&self.platform),
            authority: Rc::clone(&self.authority),
            fuel: Rc::clone(&self.fuel),
            frames: Rc::clone(&self.frames),
            self_closure: None,
            source: self.source,
            handlers: Rc::clone(&self.handlers),
        }
    }

    /// The current authority set, as a shared `Rc` — for capturing at closure
    /// creation time without cloning the `BTreeSet`.
    pub fn authority_rc(&self) -> Rc<BTreeSet<String>> {
        Rc::clone(&self.authority)
    }

    /// A `Weak` reference to the globals cell — for storing in `ClosureData` to
    /// break the `Rc<OnceCell> → map → Closure → Rc<OnceCell>` cycle.
    /// The weak ref does not keep the globals alive: when the env (and any env
    /// clones) are dropped, the strong count reaches 0 and the map is freed even
    /// though closures still hold weak refs to it. During a single `eval_program`
    /// call the env is always alive on the Rust stack, so upgrades always succeed.
    pub fn home_globals_weak(&self) -> Weak<OnceCell<BTreeMap<String, Value>>> {
        Rc::downgrade(&self.globals)
    }

    /// Replace the globals slot of this env with `globals`. Used in `apply_values`
    /// to seed the call env from the closure's home globals rather than the
    /// call-site globals — so module functions can see their own siblings.
    #[must_use]
    pub fn with_home_globals(self, globals: Rc<OnceCell<BTreeMap<String, Value>>>) -> Env {
        Env { globals, ..self }
    }

    pub fn lookup_method(&self, type_name: &str, method_name: &str) -> Option<Method> {
        self.methods
            .get()
            .and_then(|methods| {
                methods
                    .get(type_name)
                    .and_then(|for_type| for_type.iter().find(|method| method.name == method_name))
            })
            .cloned()
    }

    /// Whether any methods are registered for `type_name` — i.e. the name refers
    /// to a type with an `on` block. Lets a bare type name be recognised as a
    /// type-path receiver (`SumType.free_method()`) without it being a value.
    pub fn has_methods(&self, type_name: &str) -> bool {
        self.methods
            .get()
            .is_some_and(|methods| methods.contains_key(type_name))
    }

    /// Whether `field` of `variant` is declared `mut` — `None` if the variant
    /// has no such field. The source of truth for field-assignment legality.
    pub fn field_mutability(&self, variant: &str, field: &str) -> Option<bool> {
        self.field_mut
            .get()
            .and_then(|table| table.get(variant))
            .and_then(|fields| fields.get(field))
            .copied()
    }

    /// Install the program's top-level definitions into the shared table. Call
    /// exactly once, after building the closures that capture this env — they
    /// share the table, so each then sees every top-level definition.
    pub fn set_globals(&self, globals: BTreeMap<String, Value>) {
        assert!(
            self.globals.set(globals).is_ok(),
            "globals must be installed exactly once"
        );
    }

    pub fn set_methods(&self, methods: BTreeMap<String, Vec<Method>>) {
        assert!(
            self.methods.set(methods).is_ok(),
            "methods must be installed exactly once"
        );
    }

    pub fn set_field_mut(&self, field_mut: BTreeMap<String, BTreeMap<String, bool>>) {
        assert!(
            self.field_mut.set(field_mut).is_ok(),
            "field mutability must be installed exactly once"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Env;

    #[test]
    fn enter_call_allows_exactly_the_depth_limit_then_refuses() {
        let env = Env::new();
        // Hold each guard so depth stays incremented: exactly MAX_CALL_DEPTH
        // enters succeed, and the next is refused. Pins the boundary exactly (the
        // eval-level tests only check "deep faults / shallow doesn't").
        let mut guards = Vec::new();
        for _ in 0..Env::MAX_CALL_DEPTH {
            guards.push(env.enter_call(None).expect("under the limit succeeds"));
        }
        assert_eq!(guards.len() as u32, Env::MAX_CALL_DEPTH);
        assert!(env.enter_call(None).is_none(), "at the limit, a further call is refused");
    }

    #[test]
    fn dropping_call_guards_frees_the_depth() {
        let env = Env::new();
        {
            let mut guards = Vec::new();
            for _ in 0..Env::MAX_CALL_DEPTH {
                guards.push(env.enter_call(None).expect("under the limit"));
            }
            assert!(env.enter_call(None).is_none(), "exhausted at the limit");
        }
        // Guards dropped ⇒ depth freed ⇒ we can enter again.
        assert!(env.enter_call(None).is_some(), "a dropped guard must decrement depth");
    }
}
