// Sauna Booking Module
// Detects when two bookings overlap in time.
// Uses `prod`, `Maybe`, and `|>` as requested.

use List

// A booking occupies a room for a specific duration.
// Times are integers (e.g., minutes from start of day).
ext prod Booking(room: Int, ext start: Int, ext end: Int)

// Overlaps if the intervals intersect.
// Two intervals [a, b] and [c, d] overlap if a < d and c < b.
// Assumes exclusive end (e.g., 10-11 does not conflict with 10-12).
ext overlaps(b1: Booking, b2: Booking) -> Bool = {
    let s1 = b1.start
    let e1 = b1.end
    let s2 = b2.start
    let e2 = b2.end
    // Overlap exists if start1 is before end2 AND start2 is before end1.
    s1 < e2 and s2 < e1
}

// Returns the first overlapping pair found in the list, or None.
// Uses `first` to get the candidate and checks against the rest.
ext findFirstOverlap(bookings: List<Booking>) -> Maybe<(Booking, Booking)> = {
    let b1 = bookings |> first
    match b1 {
        Some(b1) => {
            // Check b1 against the rest of the list using `find`.
            // `removeAt` extracts the tail, avoiding manual recursion.
            let rest = removeAt(bookings, 0)
            // Use `find` to get the first x that overlaps with b1.
            let conflicts = find(rest, (x) -> overlaps(b1, x))
            match conflicts {
                Some(x) => Some((b1, x))
                _ => None
            }
        }
        _ => None
    }
}

// Check if the list contains any overlaps.
ext hasConflicts(bookings: List<Booking>) -> Bool = {
    let n = count(bookings)
    n == 0 => false | {
        findFirstOverlap(bookings) != None
    }
}

test "overlaps detects intersection" {
    let a = Booking(1, 0, 10)
    let b = Booking(1, 5, 15)
    expect overlaps(a, b) == true
}

test "overlaps detects no intersection" {
    let a = Booking(1, 0, 10)
    let b = Booking(1, 20, 30)
    expect overlaps(a, b) == false
}

test "findFirstOverlap returns None for empty" {
    expect findFirstOverlap([]) == None
}

test "findFirstOverlap finds overlap" {
    let b1 = Booking(1, 0, 10)
    let b2 = Booking(1, 5, 15)
    expect findFirstOverlap([b1, b2]) == Some((b1, b2))
}

test "hasConflicts returns true" {
    expect hasConflicts([Booking(1, 0, 10), Booking(1, 5, 15)]) == true
}
