use List

// Time is represented as minutes from midnight to allow simple integer comparison.
ext prod TimeWindow(ext start: Int, ext end: Int)

// A reservation for a specific room slot.
ext prod Booking(ext id: Str, ext window: TimeWindow)

// Exclusive use requires non-overlapping intervals; this checks the intersection condition.
ext overlaps(a: Booking, b: Booking) -> Bool = {
    let s1 = a.window.start
    let e1 = a.window.end
    let s2 = b.window.start
    let e2 = b.window.end
    // Intervals [s1, e1) and [s2, e2) overlap if start of one is before end of other.
    s1 < e2 and s2 < e1
}

// We need to know *which* booking blocks a request to report errors accurately.
ext findConflict(schedule: List<Booking>, candidate: Booking) -> Maybe<Booking> =
    schedule |> find(b -> overlaps(candidate, b))

test "bookings overlap when windows intersect" {
    let a = Booking(id: "1", window: TimeWindow(start: 0, end: 60))
    let b = Booking(id: "2", window: TimeWindow(start: 30, end: 90))
    expect overlaps(a, b) == true
}

test "bookings do not overlap when one ends before the other starts" {
    let a = Booking(id: "1", window: TimeWindow(start: 0, end: 60))
    let b = Booking(id: "2", window: TimeWindow(start: 60, end: 90))
    expect overlaps(a, b) == false
}

test "findConflict returns the blocker if any exists" {
    let schedule = [
        Booking(id: "1", window: TimeWindow(start: 0, end: 60))
    ]
    let candidate = Booking(id: "2", window: TimeWindow(start: 30, end: 90))
    let conflict = findConflict(schedule, candidate)
    expect isSome(conflict) == true
}

test "findConflict returns None when schedule is compatible" {
    let schedule = [
        Booking(id: "1", window: TimeWindow(start: 0, end: 60))
    ]
    let candidate = Booking(id: "2", window: TimeWindow(start: 60, end: 90))
    let conflict = findConflict(schedule, candidate)
    expect isNone(conflict) == true
}
