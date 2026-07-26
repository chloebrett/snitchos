// saunas.st — a small module for managing exclusive room bookings.
// It tracks time windows and detects conflicts where two bookings overlap.

use List
use Maybe
use text.{pad}

// The type of a single booking.
// Fields are immutable; a caller must return a new Product with updated values.
prod Booking(
    ext id: Int,
    ext roomName: Str,
    ext start: Int,   // minute of day (0..1439)
    ext end: Int     // minute of day (> start)
)

// A list of bookings.
prod Bookings(List<Booking>)

// Create a new booking with the given parameters.
// Validates that start < end; otherwise returns None.
ext makeBooking(id: Int, roomName: Str, start: Int, end: Int) -> Maybe<Booking> = {
    match {
        start < end => Some(Booking(id: id, roomName: roomName, start: start, end: end))
        _ => None
    }
}

test "makeBooking accepts valid time windows" {
    expect (makeBooking(1, "Main", 100, 200)) == Some(Booking(id: 1, roomName: "Main", start: 100, end: 200))
}

test "makeBooking rejects invalid windows where start >= end" {
    expect makeBooking(1, "Main", 200, 100) == None
}

// Check if two bookings overlap.
// Overlap occurs if one starts before the other ends AND ends after the other starts.
ext overlaps(a: Booking, b: Booking) -> Bool = {
    // a and b overlap if: a.start < b.end AND b.start < a.end
    a.start < b.end and b.start < a.end
}

test "overlaps detects concurrent bookings" {
    let b1 = Booking(id: 1, roomName: "A", start: 100, end: 200)
    let b2 = Booking(id: 2, roomName: "B", start: 150, end: 250)
    expect overlaps(b1, b2) == true
}

test "overlaps does not detect non-overlapping bookings" {
    let b1 = Booking(id: 1, roomName: "A", start: 100, end: 120)
    let b2 = Booking(id: 2, roomName: "B", start: 120, end: 140) // Touching but not overlapping
    expect overlaps(b1, b2) == false
}

// Find all pairs of bookings in a list that overlap.
// Returns a list of tuples (b1, b2) where b1.id < b2.id to avoid duplicates.
ext findConflicts(bookings: List<Booking>) -> List<(Booking, Booking)> = {
    let pairs = bookings |> map(b1 -> bookings |> map(b2 -> (b1, b2)))
    // Filter out self-comparisons and duplicates (where b1.id == b2.id or b1.id > b2.id)
    // and keep only those that actually overlap.
    pairs |> filter((pair) -> {
        let (b1, b2) = pair
        // Ensure unique ordering: only keep if b1 comes before b2 by ID
        b1.id < b2.id and overlaps(b1, b2)
    })
}

test "findConflicts finds overlapping pairs" {
    let b1 = Booking(id: 1, roomName: "A", start: 100, end: 200)
    let b2 = Booking(id: 2, roomName: "B", start: 150, end: 250)
    let b3 = Booking(id: 3, roomName: "C", start: 300, end: 400)
    let bookings = [b1, b2, b3]
    let conflicts = findConflicts(bookings)
    // Only (b1, b2) should be in the list
    expect count(conflicts) == 1
    expect (conflicts |> head |> unwrapOr(None)) == Some((b1, b2))
}

test "findConflicts returns empty list for non-overlapping bookings" {
    let b1 = Booking(id: 1, roomName: "A", start: 0, end: 60)
    let b2 = Booking(id: 2, roomName: "B", start: 120, end: 180)
    let bookings = [b1, b2]
    expect findConflicts(bookings) == []
}

// Check if a specific booking conflicts with any existing bookings.
ext hasConflict(booking: Booking, others: List<Booking>) -> Bool = {
    any(others, o -> overlaps(booking, o))
}

test "hasConflict detects a conflicting booking" {
    let b1 = Booking(id: 1, roomName: "A", start: 100, end: 200)
    let b2 = Booking(id: 2, roomName: "B", start: 150, end: 250)
    let others = [b2]
    expect hasConflict(b1, others) == true
}

test "hasConflict returns false for non-conflicting booking" {
    let b1 = Booking(id: 1, roomName: "A", start: 0, end: 60)
    let others = [Booking(id: 2, roomName: "B", start: 120, end: 180)]
    expect hasConflict(b1, others) == false
}

// Attempt to add a booking to a list if it doesn't conflict.
// Returns the new list with the booking appended, or the original list if it conflicts.
ext tryAdd(booking: Booking, existing: List<Booking>) -> List<Booking> = {
    match hasConflict(booking, existing) {
        false => concat(existing, [booking])
        true => existing
    }
}

test "tryAdd appends a non-conflicting booking" {
    let b1 = Booking(id: 1, roomName: "A", start: 0, end: 60)
    let b2 = Booking(id: 2, roomName: "B", start: 120, end: 180)
    let existing = [b1]
    let result = tryAdd(b2, existing)
    expect count(result) == 2
}

test "tryAdd rejects a conflicting booking" {
    let b1 = Booking(id: 1, roomName: "A", start: 0, end: 200)
    let b2 = Booking(id: 2, roomName: "B", start: 100, end: 150)
    let existing = [b1]
    let result = tryAdd(b2, existing)
    expect count(result) == 1
}

// Generate a formatted string of all booking conflicts.
ext reportConflicts(bookings: List<Booking>) -> Str = {
    let conflicts = findConflicts(bookings)
    if count(conflicts) == 0 {
        "No conflicts found."
    } else {
        let formatted = conflicts |> map(pair -> {
            let (b1, b2) = pair
            // Format: "Booking 1 overlaps with Booking 2"
            pad("Conflict: " + b1.id + " (" + b1.roomName + ") overlaps with " + b2.id + " (" + b2.roomName + ")", 80)
        })
        Str.join(formatted, "\n") + "\n"
    }
}

test "reportConflicts formats empty list correctly" {
    let bookings = []
    expect reportConflicts(bookings) == "No conflicts found."
}

test "reportConflicts formats single conflict correctly" {
    let b1 = Booking(id: 1, roomName: "Alpha", start: 0, end: 60)
    let b2 = Booking(id: 2, roomName: "Beta", start: 30, end: 90)
    let bookings = [b1, b2]
    expect reportConflicts(bookings).contains("Conflict: 1") == true
    expect reportConflicts(bookings).contains("Alpha overlaps with Beta") == true
}
