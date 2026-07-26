// Sauna booking module
// Core logic: detecting overlapping time windows for exclusive room use

use List

// Types: Time and Booking are products (records)
ext prod Time(ext start: Int, ext end: Int)
ext prod Booking(id: Int, time: Time)

// Overlap logic: two bookings overlap if one starts before the other ends
// and vice versa. This is the core constraint for exclusive use.
ext overlap(b1: Booking, b2: Booking) -> Bool = {
    let t1 = b1.time
    let t2 = b2.time
    t1.start < t2.end and t2.start < t1.end
}

// Check if a new booking conflicts with any existing bookings in the list.
// Uses pipes to compose the fold operation cleanly.
ext isAvailable(bookings: List<Booking>, time: Time) -> Bool = {
    bookings |> fold(true, (avail, b) -> avail and not(overlap(b.time, time)))
}

// Attempt to book a room by checking availability.
// Returns Maybe<Bool>: Some(true) if free, None if conflict.
ext tryBook(existingBookings: List<Booking>, time: Time) -> Maybe<Bool> = {
    let result = isAvailable(existingBookings, time)
    match result {
        true => Some(true)
        false => None
    }
}

// Test suite covering overlap and availability logic
test "overlap detects exact same time" {
    let b1 = Booking(id: 1, time: Time(start: 10, end: 20))
    let b2 = Booking(id: 2, time: Time(start: 10, end: 20))
    expect overlap(b1, b2) == true
}

test "overlap detects partial overlap" {
    let b1 = Booking(id: 1, time: Time(start: 10, end: 20))
    let b2 = Booking(id: 2, time: Time(start: 15, end: 30))
    expect overlap(b1, b2) == true
}

test "overlap detects no overlap" {
    let b1 = Booking(id: 1, time: Time(start: 10, end: 20))
    let b2 = Booking(id: 2, time: Time(start: 20, end: 30))
    expect overlap(b1, b2) == false
}

test "availability finds conflict" {
    let existing = [Booking(id: 1, time: Time(start: 10, end: 20))]
    let available = isAvailable(existing, Time(start: 5, end: 15))
    expect available == false
}

test "availability finds no conflict" {
    let existing = [Booking(id: 1, time: Time(start: 10, end: 20))]
    let available = isAvailable(existing, Time(start: 25, end: 30))
    expect available == true
}

test "tryBook returns success on free room" {
    let existing = []
    let result = tryBook(existing, Time(start: 10, end: 20))
    expect result == Some(true)
}

test "tryBook fails on conflict" {
    let existing = [Booking(id: 1, time: Time(start: 10, end: 20))]
    let result = tryBook(existing, Time(start: 10, end: 20))
    expect result == None
}
