// tokenbucket.st — a token-bucket rate limiter: `capacity` tokens, refilled
// at `refillRate` per tick, each request costing however many tokens it
// asks for. Systems-flavored and small on purpose (on-theme for an
// observability OS — this is the shape of the request throttling a real
// service would put in front of an expensive endpoint), and every request
// opens its own `uses Telemetry` span the way a real rate limiter would
// want visible in Grafana: allowed vs. denied, over time.
//
// No wall clock: `now` is an explicit `Int` tick the caller supplies —
// consistent with every other program in this batch that needed
// "current time" (stim.st's driver-supplied ticks are the shipped
// precedent), and it's what makes the tests below deterministic without
// a fake clock.

prod Bucket(capacity: Int, mut tokens: Int, refillRate: Int, mut lastRefillTick: Int)

newBucket(capacity: Int, refillRate: Int) -> Bucket = Bucket(capacity, capacity, refillRate, 0)

min2(a: Int, b: Int) -> Int = a < b => a | b

on Bucket {
    // Adds `elapsed * refillRate` tokens (clamped to `capacity`) for
    // whatever time has passed since the last refill, then bumps the
    // refill clock. A no-op if `now` hasn't advanced — calling `tryTake`
    // twice at the same tick doesn't double-refill.
    mut refill(now: Int) {
        let elapsed = now - @lastRefillTick
        elapsed > 0
            => {
                @tokens = min2(@capacity, @tokens + elapsed * @refillRate)
                @lastRefillTick = now
            }
            | ()
    }

    // Refills up to `now`, then spends `amount` tokens if there are enough
    // — the whole request is atomic (never partially charged). Emits which
    // way it went either way: a rate limiter's denials are exactly the
    // interesting telemetry, not just its successes.
    mut tryTake(now: Int, amount: Int) -> Bool uses Telemetry {
        @refill(now)
        @tokens >= amount
            => {
                @tokens = @tokens - amount
                emit("tokenbucket.allowed", amount)
                true
            }
            | {
                emit("tokenbucket.denied", amount)
                false
            }
    }
}

// --- tests ---

test "a fresh bucket starts full" {
    expect newBucket(10, 1).tokens == 10
}

test "tryTake spends tokens on success" uses Telemetry {
    let mut bucket = newBucket(10, 1)
    expect bucket.tryTake(0, 4) == true
    expect bucket.tokens == 6
}

test "tryTake denies and leaves the bucket unchanged when tokens are insufficient" uses Telemetry {
    let mut bucket = newBucket(5, 1)
    expect bucket.tryTake(0, 10) == false
    expect bucket.tokens == 5
}

test "repeated requests exhaust the bucket, then every further request is denied" uses Telemetry {
    let mut bucket = newBucket(3, 1)
    expect bucket.tryTake(0, 1) == true
    expect bucket.tryTake(0, 1) == true
    expect bucket.tryTake(0, 1) == true
    expect bucket.tryTake(0, 1) == false
    expect bucket.tokens == 0
}

test "refill restores tokens proportional to elapsed ticks, capped at capacity" uses Telemetry {
    let mut bucket = newBucket(10, 2)
    expect bucket.tryTake(0, 10) == true
    expect bucket.tokens == 0
    // 3 ticks pass, refilling at 2/tick = 6 tokens, but never above capacity.
    bucket.refill(3)
    expect bucket.tokens == 6
    bucket.refill(20)
    expect bucket.tokens == 10
}

test "tryTake at the same tick twice in a row doesn't double-refill in between" uses Telemetry {
    let mut bucket = newBucket(10, 5)
    expect bucket.tryTake(0, 10) == true
    expect bucket.tryTake(0, 1) == false
    expect bucket.tokens == 0
}

test "a request larger than the bucket's own capacity can never succeed, even fully refilled" uses Telemetry {
    let mut bucket = newBucket(5, 100)
    bucket.refill(1000)
    expect bucket.tryTake(1000, 6) == false
    expect bucket.tokens == 5
}
