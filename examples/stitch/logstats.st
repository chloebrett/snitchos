// logstats.st — parse `LEVEL: message` log lines, tally per-level counts,
// and rank each level's most-common words. The batch's observability-
// themed capstone: this is the shape of a program you'd actually want on
// an OS whose whole first-class concern is telemetry — summarizing a log
// stream instead of just emitting one.

use Str
use List
use Map

prod LogEntry(level: Str, message: Str)

findSubstr(s: Str, sub: Str, from: Int) -> Maybe<Int> =
    from + Str.length(sub) > Str.length(s)
        => None
        | (Str.slice(s, from, from + Str.length(sub)) == sub => Some(from) | findSubstr(s, sub, from + 1))

// `None` for a line with no `": "` separator — malformed lines are
// dropped by `parseLog` below rather than erroring the whole file, the
// same "one bad row doesn't sink the batch" stance csv.st's `parseRows`
// takes for a *different* reason (there, an error propagates; here,
// there's no `Result` at the call site to propagate it through, and a
// stray non-log line — a stack trace continuation, blank padding — is
// routine enough in real log output to just skip rather than fail on).
parseLine(line: Str) -> Maybe<LogEntry> =
    match findSubstr(line, ": ", 0) {
        None => None
        Some(i) => Some(LogEntry(Str.slice(line, 0, i), Str.slice(line, i + 2, Str.length(line))))
    }

parseLog(text: Str) -> List<LogEntry> =
    Str.split(text, "\n")
        |> filter(line -> line != "")
        |> flatMap(line -> match parseLine(line) { Some(e) => [e]  None => [] })

// A tally is `Map.update` over a fold: the key's first sighting inserts `1`,
// every later one adds to what's there. `Map` preserves first-seen order, so
// the report below reads levels in the order the log introduced them.
countByLevel(entries: List<LogEntry>) -> Map<Str, Int> =
    fold(entries, [:], (acc, e) -> Map.update(acc, e.level, 1, $ + 1))

tokenizeWords(msg: Str) -> List<Str> = Str.split(msg, " ") |> filter(w -> w != "")

wordCounts(words: List<Str>) -> Map<Str, Int> =
    fold(words, [:], (acc, w) -> Map.update(acc, w, 1, $ + 1))

// The first `n` elements of `xs` — `take`/`toList` want a `Seq`
// (markov.st's entry: `take` is `Seq`-only), and `sortBy` (used just
// below) hands back a `List`, so this is the `List`-native way to say
// "the top few" without a pointless `List -> Seq -> List` round trip.
firstN(xs: List<Str>, n: Int) -> List<Str> =
    enumerate(xs)
        |> filter(pair -> match pair { (i, w) => i < n })
        |> map(pair -> match pair { (i, w) => w })

// The `n` most frequent words in `level`'s messages, most frequent first.
topWords(entries: List<LogEntry>, level: Str, n: Int) -> List<Str> =
    entries
        |> filter(e -> e.level == level)
        |> flatMap(e -> tokenizeWords(e.message))
        |> wordCounts
        |> Map.entries
        |> sortBy(wc -> match wc { (_, count) => 0 - count })
        |> map(wc -> match wc { (word, _) => word })
        |> firstN(n)

// --- tests ---

test "parseLine splits on the first ': ' into level and message" {
    expect parseLine("INFO: server started") == Some(LogEntry("INFO", "server started"))
}

test "parseLine only splits on the first separator, keeping the rest of the message intact" {
    expect parseLine("WARN: retrying: connection reset") == Some(LogEntry("WARN", "retrying: connection reset"))
}

test "parseLine is None for a line with no level separator" {
    expect parseLine("just some text") == None
}

test "parseLog drops malformed lines and blank lines, keeping the rest in order" {
    let text = "INFO: starting up\nmalformed line\nERROR: disk full\n\nINFO: ready"
    expect parseLog(text) == [
        LogEntry("INFO", "starting up"),
        LogEntry("ERROR", "disk full"),
        LogEntry("INFO", "ready"),
    ]
}

test "countByLevel tallies each level, first-seen order" {
    let entries = parseLog("INFO: a\nERROR: b\nINFO: c\nINFO: d\nERROR: e")
    expect countByLevel(entries) == ["INFO": 3, "ERROR": 2]
    // `Map` equality is unordered, so the first-seen order this test's name
    // promises needs saying out loud. The `List<LevelCount>` version asserted
    // it for free, as a side effect of list equality — dropping to a bare
    // `Map` comparison would have quietly retired that coverage.
    expect Map.keys(countByLevel(entries)) == ["INFO", "ERROR"]
}

test "topWords ranks a level's most frequent words, most frequent first" {
    let entries = parseLog(
        "INFO: connection reset by peer\n" +
        "INFO: connection reset again\n" +
        "INFO: connection established\n" +
        "ERROR: disk full disk full disk full"
    )
    expect topWords(entries, "INFO", 2) == ["connection", "reset"]
    expect topWords(entries, "ERROR", 1) == ["disk"]
}

test "topWords on a level with no entries is empty" {
    let entries = parseLog("INFO: hello")
    expect topWords(entries, "DEBUG", 3) == []
}

test "firstN never returns more than n, even if fewer than n exist" {
    expect firstN(["a", "b"], 5) == ["a", "b"]
    expect firstN(["a", "b", "c"], 2) == ["a", "b"]
    expect firstN([], 3) == []
}
