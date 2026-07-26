// shell.st — a tiny command dispatcher: parse a line into a `sum Command`,
// then run it. The batch's tenth and last program in this round, so it
// deliberately pulls together threads from the ones before it rather than
// opening a new one: `use <-` (markov.st, inventory.st, life.st), capability
// rows that vary per command (bank.st), and — new here — `?.` safe
// navigation, which nothing earlier in the batch needed a `Maybe`-of-a-
// `prod` to exercise.

use Str
use List

// --- commands ---

sum Command =
    | Help
    | Echo(Str)
    | Add(Int, Int)
    | Whoami
    | Ls
    | Unknown(Str)

tokenize(line: Str) -> List<Str> = Str.split(line, " ") |> filter(w -> w != "")

parseCommand(line: Str) -> Command = {
    let words = tokenize(line)
    match first(words) {
        None => Unknown(line)
        Some(name) => dispatch(name, List.removeAt(words, 0), line)
    }
}

dispatch(name: Str, args: List<Str>, original: Str) -> Command =
    match name {
        "help" => Help
        "echo" => Echo(Str.join(args, " "))
        "whoami" => Whoami
        "ls" => Ls
        "add" => parseAdd(args, original)
        _ => Unknown(original)
    }

// `add` needs exactly two integer arguments; anything else (wrong count,
// non-numeric args) falls back to `Unknown(original)` rather than a
// dedicated "bad add" variant — from the user's side, "add x y" and
// "addd 1 2" are the same kind of mistake, so they get the same answer.
parseAdd(args: List<Str>, original: Str) -> Command =
    count(args) != 2
        => Unknown(original)
        | match (Str.parseInt(unwrapOr(List.at(args, 0), "")), Str.parseInt(unwrapOr(List.at(args, 1), ""))) {
            (Some(a), Some(b)) => Add(a, b)
            _ => Unknown(original)
        }

// --- who's running the shell ---

prod Profile(name: Str, home: Str)
prod Session(user: Maybe<Profile>)

guest() -> Session = Session(None)
loggedIn(name: Str, home: Str) -> Session = Session(Some(Profile(name, home)))

// --- running a command ---

// `Ls` is the only command that touches the filesystem, gated by `runLs`'s
// own `uses FsRead` — but unlike bank.st's `transferAudited` (which calls
// `@transfer` as a *method* and doesn't need to hold `Telemetry` itself),
// `runCommand` calls `runLs()` as a *plain function*, and the static
// checker propagates a called function's `uses` up to *every* caller
// (`check.rs::walk_effects`, case C2) — unlike the runtime, which resets
// authority at every call boundary regardless of how it's called. So
// `runCommand` has to declare `FsRead` too, even though nothing in its own
// body performs an FsRead effect directly. See
// plans/stitch-examples-findings.md for the fuller writeup (and why method
// calls don't have this requirement).
runCommand(cmd: Command, session: Session) -> Result<Str, Str> uses Telemetry, FsRead {
    use <- span("shell.run")
    match cmd {
        Help => Ok("commands: help, echo <text>, add <a> <b>, whoami, ls")
        Echo(text) => Ok(text)
        Add(a, b) => Ok(toStr(a + b))
        // `session.user` is `Maybe<Profile>`; `?.name` reaches into the
        // `Profile` only if there is one, staying `None` (short-circuiting
        // to "stranger" below) otherwise — the "user?.address?.zip" shape
        // from docs/language-design.md, just one level deep.
        Whoami => Ok("hello, " + (session.user?.name |> unwrapOr("stranger")))
        Ls => runLs()
        Unknown(text) => Err("unknown command: " + text)
    }
}

// A real shell would list a directory; this one reads a fixed manifest
// file, since that's what the interpreter's `readFile` actually offers
// (no directory-listing native — see `natives.rs::native_read_file`). A
// missing manifest reads as "no files" rather than an error: an empty
// listing and aListing-that-doesn't-exist-yet are the same answer to
// "what's here".
runLs() -> Result<Str, Str> uses FsRead =
    match readFile("manifest") {
        None => Ok("(no files)")
        Some(contents) => Ok(contents)
    }

runLine(line: Str, session: Session) -> Result<Str, Str> uses Telemetry, FsRead = runCommand(parseCommand(line), session)

// --- tests ---

test "parseCommand recognizes every known command" {
    expect parseCommand("help") == Help
    expect parseCommand("echo hi there") == Echo("hi there")
    expect parseCommand("add 2 3") == Add(2, 3)
    expect parseCommand("whoami") == Whoami
    expect parseCommand("ls") == Ls
}

test "an empty or unrecognized line is Unknown, carrying the original text" {
    expect parseCommand("") == Unknown("")
    expect parseCommand("bogus arg") == Unknown("bogus arg")
}

test "add with the wrong argument count or non-numeric args is Unknown" {
    expect parseCommand("add 2") == Unknown("add 2")
    expect parseCommand("add 1 2 3") == Unknown("add 1 2 3")
    expect parseCommand("add x y") == Unknown("add x y")
}

test "tokenize collapses repeated spaces and drops empties" {
    expect tokenize("echo   hi   there") |> count == 3
}

test "runCommand answers help, echo, and add directly" uses Telemetry, FsRead {
    expect runCommand(Help, guest()) == Ok("commands: help, echo <text>, add <a> <b>, whoami, ls")
    expect runCommand(Echo("hi"), guest()) == Ok("hi")
    expect runCommand(Add(2, 3), guest()) == Ok("5")
}

test "whoami greets a logged-in user by name and a guest generically" uses Telemetry, FsRead {
    expect runCommand(Whoami, loggedIn("ava", "/home/ava")) == Ok("hello, ava")
    expect runCommand(Whoami, guest()) == Ok("hello, stranger")
}

test "ls reports no files when there is no filesystem to read" uses Telemetry, FsRead {
    expect runCommand(Ls, guest()) == Ok("(no files)")
}

test "an unknown command is a named error, not a fault" uses Telemetry, FsRead {
    expect runCommand(Unknown("frobnicate"), guest()) == Err("unknown command: frobnicate")
}

test "runLine parses and runs in one step" uses Telemetry, FsRead {
    expect runLine("add 10 20", guest()) == Ok("30")
    expect runLine("whoami", loggedIn("bo", "/home/bo")) == Ok("hello, bo")
    expect runLine("nope", guest()) == Err("unknown command: nope")
}
