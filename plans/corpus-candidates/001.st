use Str

// --- booking shapes ---

prod Booking(start: Int, end: Int) = // start and end in minutes since epoch
    // end is exclusive — booking lasts until but not including end
    // so [10, 20] means 10-19 minutes are booked, not 10-20

// --- room types ---

prod Room(mut bookings: List<Booking>) = // bookings are strictly ordered

// --- room operations ---

on Room {
    // Check if a new booking overlaps with any existing one
    // If it does, return None; if not, return Some(new booking)
    // Note: bookings are strictly ordered, so we can do a single scan
    mut book(start: Int, end: Int) -> Maybe<Booking> =
        let newBooking = Booking(start, end)
        let overlaps = find(@bookings, b -> start < @b.end and end > @b.start)
        overlaps == None
            => Some(newBooking)
            | {
                // No overlap — add to list
                @bookings = concat(@bookings, [newBooking])
                Some(newBooking)
            }
        // TRUNCATED: ~1300 further tokens of one comment repeated verbatim.
