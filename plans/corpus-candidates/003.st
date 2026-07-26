use List

// Rooms are distinct entities (cedar or birch) to identify conflict zones
// This helps log which specific sauna zone was double-booked
ext Room(id: Int, name: Str) = prod Room(id: Int, name: Str)

// A booking occupies a room for an exclusive time window
// Overlap is strictly inside the interval boundaries
ext Booking(roomId: Int, start: Int, end: Int) = prod Booking(roomId: Int, start: Int, end: Int)

// Overlap occurs when rooms match and intervals intersect strictly
// This ensures exclusive use without touching edges (e.g. 10-12 vs 11-13)
ext overlaps(b1: Booking, b2: Booking) -> Bool = {
    let sameRoom = b1.roomId == b2.roomId
    b1.start < b2.end && b2.start < b1.end && sameRoom
}

// Conflict record stores which rooms clashed and required buffer
// cooldown is set to 0 when overlap detected
ext Conflict(idA: Int, idB: Int, cooldown: Int) = prod Conflict(idA: Int, idB: Int, cooldown: Int)

// Detect if any conflict exists between two bookings
// Returns Some([conflict]) if overlap found, None otherwise
ext findConflict(b1: Booking, b2: Booking) -> Maybe<Conflict> = {
    overlaps(b1, b2) => {
        let c = Conflict(b1.roomId, b2.roomId, 0)
        Some(c)
    } | None
}

// Check if a list of bookings contains any overlapping pairs
// We use fold to iterate and check all pairs against each other
ext findConflicts(bookings: List<Booking>) -> Maybe<List<Conflict>> = {
    let len = count(bookings)
    len == 0 => None | {
        let acc = []
        bookings |> fold([], [], (acc, b) -> {
            // Check if current booking overlaps any previously processed one
            // If so, record the conflict and add to accumulator
            let conflicts = acc |> all(a -> overlaps(a, b))
            conflicts => {
                let c = Conflict(b.roomId, b.roomId, 0)
                acc + [c]
            } | acc
        })
    }
}

test "cedar and birch cannot overlap in the same room" {
    let b1 = Booking(1, 10, 12)
    let b2 = Booking(1, 11, 13)
    expect overlaps(b1, b2) == true
}

test "birch rooms overlap differently" {
    let b1 = Booking(1, 10, 12)
    let b2 = Booking(2, 11, 13)
    expect overlaps(b1, b2) == false
}

test "no conflicts in empty list" {
    expect findConflicts([]) == None
}
