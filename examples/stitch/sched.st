// sched.st — a tiny task-scheduler simulation, comparing round-robin and
// priority strategies through `contract` dispatch — "the only polymorphism"
// (docs/language-design.md). Loosely mirrors the kernel's own scheduler in
// spirit (`kernel_proc::sched::Runqueue`), in userspace toy form: no real
// concurrency, just "who runs this tick" decided by a swappable strategy
// value.
//
// The scenario in the tests below is deliberately **saturated** — three
// tasks with distinct priorities and distinct durations, none of them
// trivial — rather than the easy two-task case. A lightly-loaded scheduler
// makes every strategy look the same (there's nothing to contend over); the
// difference between "fair" and "priority-ordered" only shows up once
// there's real contention, and the thing worth asserting on is *completion
// order over the whole run*, not just which task goes first.

// --- the model ---

prod Task(name: Str, priority: Int, remaining: Int)

// One tick's outcome: who ran, and how much work they had left afterward
// (`0` means this was their last tick).
prod TickResult(ran: Str, remainingAfter: Int)

// `pickNext` is the *entire* strategy — everything else (removing the
// picked task, decrementing its work, deciding whether it's done, and
// requeuing it at the back of the pool if not) is the same regardless of
// strategy, handled once by `step` below. A priority strategy doesn't need
// its own requeue rule: it rescans the whole pool for the max every tick, so
// where a not-yet-done task lands in the list doesn't affect what runs next.
contract Schedulable {
    pickNext(tasks: List<Task>) -> Maybe<Task>
}

prod RoundRobin()

on RoundRobin : Schedulable {
    // The pool is already in run order (see `step`'s requeue-at-the-back
    // rule below) — round-robin's whole strategy is "trust that order".
    pickNext(tasks: List<Task>) -> Maybe<Task> = first(tasks)
}

prod Priority()

on Priority : Schedulable {
    pickNext(tasks: List<Task>) -> Maybe<Task> = highestPriority(tasks)
}

// The first task with the maximum `priority` — ties broken by pool order,
// same as `RoundRobin` would pick among equals.
highestPriority(tasks: List<Task>) -> Maybe<Task> =
    fold(tasks, None, (best, t) ->
        match best {
            None => Some(t)
            Some(b) => t.priority > b.priority => Some(t) | best
        })

// --- running the simulation ---

removeByName(tasks: List<Task>, name: Str) -> List<Task> = filter(tasks, t -> t.name != name)

prod StepOutcome(log: TickResult, pool: List<Task>)

// One tick: ask the strategy who runs, burn one unit of their work, and
// either drop them (done) or requeue them at the back. `strategy` is
// untyped here on purpose — `contract`-typed dynamic dispatch means `step`
// doesn't need to know or care *which* `Schedulable` it was handed; that's
// the whole payoff of "the only polymorphism."
step(strategy, pool: List<Task>) -> Maybe<StepOutcome> =
    match strategy.pickNext(pool) {
        None => None
        Some(task) => {
            let rest = removeByName(pool, task.name)
            let after = task.remaining - 1
            let nextPool = after <= 0 => rest | concat(rest, [Task(task.name, task.priority, after)])
            Some(StepOutcome(TickResult(task.name, after), nextPool))
        }
    }

runAll(strategy, pool: List<Task>) -> List<TickResult> = runLoop(strategy, pool, [])

runLoop(strategy, pool: List<Task>, log: List<TickResult>) -> List<TickResult> =
    match step(strategy, pool) {
        None => reverse(log)
        Some(outcome) => runLoop(strategy, outcome.pool, concat([outcome.log], log))
    }

// --- reporting ---
//
// Per-task total CPU time, in the order each task first ran — the
// measurement project memory settled on for this exact kind of comparison:
// *how many ticks each task actually got*, not just which one ran first
// (a single early tick tells you nothing about whether a strategy kept
// starving someone).

prod CpuTime(name: Str, ticks: Int)

cpuTimeByTask(log: List<TickResult>) -> List<CpuTime> =
    fold(log, [], (acc, t) ->
        match find(acc, c -> c.name == t.ran) {
            None => concat(acc, [CpuTime(t.ran, 1)])
            Some(_) => map(acc, c -> c.name == t.ran => CpuTime(c.name, c.ticks + 1) | c)
        })

// --- tests ---

test "round-robin interleaves fairly across ready tasks" {
    let pool = [Task("t", 1, 3), Task("u", 5, 2), Task("v", 3, 2)]
    let names = runAll(RoundRobin(), pool) |> map(r -> r.ran)
    expect names == ["t", "u", "v", "t", "u", "v", "t"]
}

test "priority drains the highest-priority ready task to completion first" {
    // Same pool, same total work (7 ticks) — priority ignores fairness
    // entirely and finishes "u" (priority 5), then "v" (3), then "t" (1),
    // each one run to completion before the next starts.
    let pool = [Task("t", 1, 3), Task("u", 5, 2), Task("v", 3, 2)]
    let names = runAll(Priority(), pool) |> map(r -> r.ran)
    expect names == ["u", "u", "v", "v", "t", "t", "t"]
}

test "both strategies finish the same total work, just in a different order" {
    let pool = [Task("t", 1, 3), Task("u", 5, 2), Task("v", 3, 2)]
    expect count(runAll(RoundRobin(), pool)) == 7
    expect count(runAll(Priority(), pool)) == 7
}

test "cpuTimeByTask sums each task's ticks, first-run order" {
    let pool = [Task("t", 1, 3), Task("u", 5, 2), Task("v", 3, 2)]
    expect cpuTimeByTask(runAll(RoundRobin(), pool)) == [CpuTime("t", 3), CpuTime("u", 2), CpuTime("v", 2)]
    expect cpuTimeByTask(runAll(Priority(), pool)) == [CpuTime("u", 2), CpuTime("v", 2), CpuTime("t", 3)]
}

test "priority ties break by pool order, same as round-robin among equals" {
    let pool = [Task("a", 5, 1), Task("b", 5, 1)]
    expect runAll(Priority(), pool) |> map(r -> r.ran) == ["a", "b"]
    expect runAll(RoundRobin(), pool) |> map(r -> r.ran) == ["a", "b"]
}

test "an empty pool runs zero ticks under either strategy" {
    expect runAll(RoundRobin(), []) == []
    expect runAll(Priority(), []) == []
}

test "a single task just runs out its own remaining work" {
    let pool = [Task("solo", 1, 3)]
    expect runAll(RoundRobin(), pool) == [
        TickResult("solo", 2),
        TickResult("solo", 1),
        TickResult("solo", 0),
    ]
}

test "step reports the exact remaining count, zero on the last tick" {
    match step(RoundRobin(), [Task("only", 1, 1)]) {
        None => expect false
        Some(outcome) => {
            expect outcome.log == TickResult("only", 0)
            expect outcome.pool == []
        }
    }
}
