// vm.st — a toy stack-based bytecode interpreter: a small Forth-shaped
// machine (`Push`/`Pop`/`Dup`/`Over`/`Swap`/arithmetic/`Jump`/`Halt`) run by
// a self-tail-recursive `step` function. A little on-the-nose for a
// language whose own runtime is staged tree-walk → bytecode VM (see
// `docs/language-design.md`'s "Execution" section) — but that's exactly
// why it's here: it exercises recursion depth, `Result`-threaded control
// flow, and a `sum`-typed instruction set at once.
//
// Unlike json.st (which needed `at2` — a hand-rolled positional accessor —
// because it kept threading anonymous `(value, cursor)` tuples), this file
// never needs one: `pop` below returns a named `prod Popped(value, rest)`
// instead of a `(Int, List<Int>)` tuple. Once a two-element result is
// *read* more than once, a named product communicates the fields' meaning
// where a tuple + a positional-access shim can't — worth remembering as a
// default over reaching for `at2` again.

use List

// --- the instruction set ---

sum Instr =
    | Push(Int)
    | Pop
    | Dup
    | Over   // duplicate the *second*-from-top element onto the top —
             // Forth's `over`; without it, a loop that both uses and
             // decrements a counter has nowhere to keep a spare copy
    | Swap
    | Add
    | Sub
    | Mul
    | Div
    | Jump(Int)
    | JumpIfZero(Int)
    | Halt

// --- machine state ---

prod VmState(pc: Int, stack: List<Int>)

// What one `exec` step decides: keep going with a new state, or stop with
// the machine's final answer (`Halt` pops one value and returns it — a
// stack-based VM's "return value" is just whatever is left facing up).
sum StepOutcome = Continue(VmState) | Halted(Int)

prod Popped(value: Int, rest: List<Int>)

push(n: Int, stack: List<Int>) -> List<Int> = concat([n], stack)

pop(stack: List<Int>, opName: Str) -> Result<Popped, Str> =
    match first(stack) {
        None => Err(opName + ": stack underflow")
        Some(top) => Ok(Popped(top, List.removeAt(stack, 0)))
    }

// Pop twice for a binary operator: `rhs` is whatever was pushed last (the
// top), `lhs` the one under it — so `Push(a) Push(b) Sub` computes `a - b`,
// matching how you'd write it down (`a`, then `b`, then the operator).
binOp(state: VmState, opName: Str, f) -> Result<StepOutcome, Str> = {
    let p1 = pop(state.stack, opName)?
    let p2 = pop(p1.rest, opName)?
    Ok(Continue(VmState(state.pc + 1, push(f(p2.value, p1.value), p2.rest))))
}

divOp(state: VmState) -> Result<StepOutcome, Str> = {
    let p1 = pop(state.stack, "div")?
    let p2 = pop(p1.rest, "div")?
    p1.value == 0
        => Err("division by zero")
        | Ok(Continue(VmState(state.pc + 1, push(p2.value / p1.value, p2.rest))))
}

// Run one instruction against `state`, returning what to do next.
exec(instr: Instr, state: VmState) -> Result<StepOutcome, Str> =
    match instr {
        Push(n) => Ok(Continue(VmState(state.pc + 1, push(n, state.stack))))
        Pop => {
            let popped = pop(state.stack, "pop")?
            Ok(Continue(VmState(state.pc + 1, popped.rest)))
        }
        Dup => {
            let popped = pop(state.stack, "dup")?
            Ok(Continue(VmState(state.pc + 1, push(popped.value, state.stack))))
        }
        Over => {
            let p1 = pop(state.stack, "over")?
            let p2 = pop(p1.rest, "over")?
            Ok(Continue(VmState(state.pc + 1, push(p2.value, state.stack))))
        }
        Swap => {
            let p1 = pop(state.stack, "swap")?
            let p2 = pop(p1.rest, "swap")?
            Ok(Continue(VmState(state.pc + 1, push(p2.value, push(p1.value, p2.rest)))))
        }
        Add => binOp(state, "add", (lhs, rhs) -> lhs + rhs)
        Sub => binOp(state, "sub", (lhs, rhs) -> lhs - rhs)
        Mul => binOp(state, "mul", (lhs, rhs) -> lhs * rhs)
        Div => divOp(state)
        Jump(target) => Ok(Continue(VmState(target, state.stack)))
        JumpIfZero(target) => {
            let popped = pop(state.stack, "jumpIfZero")?
            popped.value == 0
                => Ok(Continue(VmState(target, popped.rest)))
                | Ok(Continue(VmState(state.pc + 1, popped.rest)))
        }
        Halt => {
            let popped = pop(state.stack, "halt")?
            Ok(Halted(popped.value))
        }
    }

// The driver loop. Self-tail-recursive: the call to `step` in the
// `Continue` arm is this function's own last action, so a long-running
// program (the loop test below executes dozens of steps) runs in bounded
// Rust stack rather than growing one frame per instruction — the same
// trampoline `interp.rs`'s own `self_tail_recursive_function_via_match_*`
// tests prove holds through a `match` arm, not just a bare `if`.
step(prog: List<Instr>, state: VmState) -> Result<Int, Str> =
    match List.at(prog, state.pc) {
        None => Err("pc " + toStr(state.pc) + " ran off the end of the program (missing Halt?)")
        Some(instr) => match exec(instr, state) {
            Err(e) => Err(e)
            Ok(Halted(result)) => Ok(result)
            Ok(Continue(next)) => step(prog, next)
        }
    }

run(prog: List<Instr>) -> Result<Int, Str> = step(prog, VmState(0, []))

// Assemble "sum the integers 1..=n" as a program. Loop invariant: the
// stack is always `[acc, counter]` on entry to the loop head (pc 2) —
// worked out by hand once (see plans/stitch-examples-findings.md), then
// this generator is what makes it reusable for any `n`.
sumTo(n: Int) -> List<Instr> = [
    Push(n),           // 0: counter
    Push(0),           // 1: acc            stack: [acc, counter]
    Over,               // 2: LOOP           [counter, acc, counter]
    JumpIfZero(11),     // 3: exit when counter == 0
    Over,               // 4                 [counter, acc, counter]
    Add,                // 5: acc += counter [acc', counter]
    Swap,               // 6                 [counter, acc']
    Push(1),            // 7
    Sub,                // 8: counter -= 1   [counter', acc']
    Swap,               // 9                 [acc', counter']
    Jump(2),            // 10: back to LOOP
    // Exit lands with the spent (zero) counter on *top* — the loop
    // invariant is [acc, counter], and `Over`+`JumpIfZero` only add and then
    // remove a temporary copy, so the exit stack is exactly that pair
    // unchanged. Getting this backwards (`Pop` straight away, dropping the
    // accumulator instead) was this program's first bug — caught by
    // simulating the instruction list by hand outside Stitch, not by the
    // Stitch tests, which only ever saw the wrong-but-plausible `Ok(0)`.
    Swap,               // 11: bring the counter to the top
    Pop,                // 12: drop it, leaving the accumulator
    Halt,               // 13
]

// --- tests ---

test "push then halt returns the pushed value" {
    expect run([Push(42), Halt]) == Ok(42)
}

test "add pops two and pushes their sum, in the order written" {
    expect run([Push(10), Push(3), Add, Halt]) == Ok(13)
}

test "sub computes the first-pushed minus the second-pushed" {
    expect run([Push(10), Push(3), Sub, Halt]) == Ok(7)
}

test "mul multiplies" {
    expect run([Push(6), Push(7), Mul, Halt]) == Ok(42)
}

test "div divides and division by zero is a named error" {
    expect run([Push(20), Push(4), Div, Halt]) == Ok(5)
    expect run([Push(20), Push(0), Div, Halt]) == Err("division by zero")
}

test "dup duplicates the top of the stack" {
    expect run([Push(5), Dup, Add, Halt]) == Ok(10)
}

test "over copies the second-from-top element to the top" {
    // [3, 5] -> Over -> [5, 3, 5] -> Add(5+5) consumed with 3 left under it
    // -> [10, 3] -> Add(3+10) -> [13]
    expect run([Push(5), Push(3), Over, Add, Add, Halt]) == Ok(13)
}

test "swap exchanges the top two elements" {
    // Sub is lhs-minus-rhs on (first pushed, second pushed); swapping the
    // push order changes the answer, proving swap actually reordered them.
    expect run([Push(10), Push(3), Swap, Sub, Halt]) == Ok(0 - 7)
}

test "pop discards the top without using it" {
    expect run([Push(1), Push(2), Pop, Halt]) == Ok(1)
}

test "popping an empty stack is a named underflow error, not a fault" {
    expect run([Pop, Halt]) == Err("pop: stack underflow")
    expect run([Add, Halt]) == Err("add: stack underflow")
    expect run([Halt]) == Err("halt: stack underflow")
}

test "a program that runs off the end without Halt is an error" {
    match run([Push(1), Push(2)]) {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "jump skips forward unconditionally" {
    // Jump(3) skips the Push(99) at index 2 entirely.
    expect run([Push(1), Jump(3), Push(99), Push(2), Add, Halt]) == Ok(3)
}

test "jumpIfZero falls through when the top is nonzero" {
    expect run([Push(1), JumpIfZero(5), Push(10), Push(20), Add, Halt]) == Ok(30)
}

test "jumpIfZero jumps when the top is zero" {
    expect run([Push(0), JumpIfZero(4), Push(999), Halt, Push(0 - 1), Halt]) == Ok(0 - 1)
}

test "sumTo assembles a loop that sums 1..=n" {
    expect run(sumTo(5)) == Ok(15)
    expect run(sumTo(1)) == Ok(1)
    expect run(sumTo(0)) == Ok(0)
}

test "exec runs a single instruction without a whole program" {
    let state = VmState(0, [7, 3])
    match exec(Add, state) {
        Ok(Continue(next)) => {
            expect next.pc == 1
            expect next.stack == [10]
        }
        other => expect false
    }
}
