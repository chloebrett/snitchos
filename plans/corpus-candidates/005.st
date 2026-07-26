use List

// Sauna bookings are exclusive time windows.
// Overlapping times mean a conflict, so we must detect them.

// A booking is defined by start and end times.
// Fields are marked `ext` so they can be accessed externally.
ext prod Booking(ext start: Int, ext end: Int)

// Two bookings overlap if their time windows intersect.
// We check this explicitly to ensure exclusive use.
ext birch(b1: Booking, b2: Booking) -> Bool = {
    let s1 = b1.start
    let e1 = b1.end
    let s2 = b2.start
    let e2 = b2.end
    // Overlap exists if the first starts before the second ends AND vice versa.
    s1 < e2 and s2 < e1
}

// cedar checks if a list of bookings contains any conflicts.
// We iterate all pairs to ensure no two bookings overlap.
ext cedar(bookings: List<Booking>) -> Bool = {
    any(bookings, b -> any(bookings, x -> birch(b, x)))
}

test "cedar detects overlap" {
    let b1 = Booking(start: 10, end: 20)
    let b2 = Booking(start: 15, end: 25)
    expect cedar([b1, b2]) == true
}

test "cedar allows disjoint" {
    let b1 = Booking(start: 10, end: 20)
    let b2 = Booking(start: 30, end: 40)
    expect cedar([b1, b2]) == false
}
