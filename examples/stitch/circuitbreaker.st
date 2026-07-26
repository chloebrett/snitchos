// circuitbreaker.st — the classic three-state circuit breaker
// (`Closed -> Open -> HalfOpen -> Closed`/`Open`), distinct from stim.st's
// mode machine (`sum Mode`) in shape: stim's modes are driven by discrete
// key events, this one's transitions are driven by *counting* (failures
// against a threshold) and *time* (a reset timeout), which is the more
// common shape for an operational state machine.

contract Show {
    show() -> Str
}

sum State = Closed | Open | HalfOpen

on State : Show {
    show() -> Str = match @ { Closed => "closed"  Open => "open"  HalfOpen => "half-open" }
}

prod Breaker(
    mut state: State,
    mut failureCount: Int,
    failureThreshold: Int,
    mut openedAtTick: Int,
    resetTimeoutTicks: Int,
)

newBreaker(failureThreshold: Int, resetTimeoutTicks: Int) -> Breaker =
    Breaker(Closed, 0, failureThreshold, 0, resetTimeoutTicks)

on Breaker {
    // Whether a call may proceed right now. The one transition that's
    // time-driven rather than event-driven: `Open` becomes `HalfOpen` (and
    // allows exactly this one trial call through) once the reset timeout
    // has elapsed, without anyone having to call anything else first.
    mut allow(now: Int) -> Bool =
        match @state {
            Closed => true
            HalfOpen => true
            Open =>
                now - @openedAtTick >= @resetTimeoutTicks
                    => { @state = HalfOpen  true }
                    | false
        }

    mut recordSuccess() {
        match @state {
            HalfOpen => { @state = Closed  @failureCount = 0 }
            Closed => { @failureCount = 0 }
            Open => ()
        }
    }

    mut recordFailure(now: Int) {
        match @state {
            // A failed trial call while half-open immediately reopens —
            // no second chance, and the reset clock restarts from `now`.
            HalfOpen => { @state = Open  @openedAtTick = now  @failureCount = @failureCount + 1 }
            Closed => {
                @failureCount = @failureCount + 1
                @failureCount >= @failureThreshold
                    => { @state = Open  @openedAtTick = now }
                    | ()
            }
            Open => ()
        }
    }

    // The convenience wrapper a real caller would actually use: check
    // `allow`, run `op` only if allowed, and record the outcome either way
    // — the caller never has to remember the record-after-call step.
    mut call(now: Int, op) -> Result<Int, Str> =
        @allow(now)
            => match op() {
                Ok(v) => { @recordSuccess()  Ok(v) }
                Err(e) => { @recordFailure(now)  Err(e) }
            }
            | Err("circuit open")
}

// --- tests ---

test "a fresh breaker starts closed and allows calls" {
    let mut b = newBreaker(3, 100)
    expect b.state == Closed
    expect b.allow(0) == true
}

test "failures below the threshold don't trip the breaker" {
    let mut b = newBreaker(3, 100)
    b.recordFailure(0)
    b.recordFailure(1)
    expect b.state == Closed
    expect b.allow(2) == true
}

test "reaching the failure threshold opens the breaker" {
    let mut b = newBreaker(3, 100)
    b.recordFailure(0)
    b.recordFailure(1)
    b.recordFailure(2)
    expect b.state == Open
    expect b.allow(3) == false
}

test "an open breaker refuses calls until the reset timeout elapses" {
    let mut b = newBreaker(1, 50)
    b.recordFailure(0)
    expect b.state == Open
    expect b.allow(10) == false
    expect b.allow(49) == false
    expect b.allow(50) == true
    expect b.state == HalfOpen
}

test "a success while half-open closes the breaker and resets the count" {
    let mut b = newBreaker(1, 50)
    b.recordFailure(0)
    b.allow(50)
    expect b.state == HalfOpen
    b.recordSuccess()
    expect b.state == Closed
    expect b.failureCount == 0
}

test "a failure while half-open reopens immediately, resetting the clock" {
    let mut b = newBreaker(1, 50)
    b.recordFailure(0)
    b.allow(50)
    expect b.state == HalfOpen
    b.recordFailure(75)
    expect b.state == Open
    expect b.allow(100) == false
    expect b.allow(125) == true
}

test "show renders each state as its lowercase name" {
    expect Closed.show() == "closed"
    expect Open.show() == "open"
    expect HalfOpen.show() == "half-open"
}

test "call runs the operation only when allowed, and records the outcome" {
    let mut b = newBreaker(1, 100)
    expect b.call(0, () -> Ok(42)) == Ok(42)
    expect b.state == Closed
    expect b.call(1, () -> Err("boom")) == Err("boom")
    expect b.state == Open
    expect b.call(2, () -> Ok(1)) == Err("circuit open")
}
