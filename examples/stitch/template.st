// template.st — a mustache-like renderer: `{{var}}` interpolates a string
// binding, `{{#each items}}…{{.}}…{{/each}}` repeats the enclosed text once
// per element of a list binding, `.` inside an each-block is the current
// element.
//
// The context is a `Map<Str, Value>` — it was an association list until
// `Map` became buildable. `Value`'s two variants are named `VStr`/`VList`,
// not `Str`/`List`, because those
// names are already taken by the built-in `Str`/`List` *modules* this file
// also imports; a `sum` variant and a module living in the same namespace
// would collide.
//
// Scope cut, stated up front rather than discovered by a failing test:
// **no nested `{{#each}}`.** `splitEachBlock` finds the *first* `{{/each}}`
// after a `{{#each}}`, so a nested each-block's own `{{/each}}` would close
// the outer one early. A real template engine needs a depth counter; this
// one doesn't, because demonstrating the interpolate/iterate mechanism
// doesn't require it.
//
// **The one bug this file guaranteed it would hit, and did**: Stitch string
// interpolation escapes a literal brace as `{{`/`}}` (json.st's entry in
// plans/stitch-examples-findings.md already flagged this as a papercut for
// any JSON-shaped string; a template *language whose own syntax is
// `{{`/`}}`* is the worst possible file to write it in). Every `"{{"` /
// `"}}"` below that means the literal two-character delimiter — not an
// interpolation escape — has to be written **doubled again**: source
// `"{{{{"` decodes to the value `"{{"`. First draft used undoubled
// `"{{"`/`"}}"` throughout (looking exactly right to the eye, since `{{`
// visually *is* the delimiter), and the whole file quietly parsed as if
// every tag were single-braced (`Str.split(template, "{")`, effectively) —
// caught by "Hello, {{name}}!" rendering as "Hello, Ada" with the `!`
// silently eaten, and an unrelated `"no match arm matched"` fault on the
// each-block tests, neither of which points anywhere near the real cause.

use Str
use List
use Map

sum Value = VStr(Str) | VList(List<Value>)

sum Segment = Text(Str) | Var(Str) | EachStart(Str) | EachEnd

isEmptyStr(s: Str) -> Bool = Str.length(s) == 0

// A generic substring search — `Str` has no `indexOf`, so finding `"}}"`
// (the tag closer) has to be hand-rolled, self-tail-recursive over the
// starting position.
findSubstr(s: Str, sub: Str, from: Int) -> Maybe<Int> =
    from + Str.length(sub) > Str.length(s)
        => None
        | (Str.slice(s, from, from + Str.length(sub)) == sub => Some(from) | findSubstr(s, sub, from + 1))

prod TaggedPart(tag: Str, text: Str)

// `part` is everything after one `{{` split point: the tag content, `}}`,
// then plain text up to the next `{{` (or the end of the template).
splitTag(part: Str) -> TaggedPart =
    match findSubstr(part, "}}}}", 0) {
        None => TaggedPart(Str.trim(part), "")
        Some(idx) => TaggedPart(Str.trim(Str.slice(part, 0, idx)), Str.slice(part, idx + 2, Str.length(part)))
    }

classifyTag(tag: Str) -> Segment =
    tag == "/each"
        => EachEnd
        | (Str.startsWith(tag, "#each ")
            => EachStart(Str.trim(Str.slice(tag, 6, Str.length(tag))))
            | Var(tag))

tagSegments(part: Str) -> List<Segment> = {
    let tp = splitTag(part)
    concat([classifyTag(tp.tag)], isEmptyStr(tp.text) => [] | [Text(tp.text)])
}

segmentsOf(template: Str) -> List<Segment> = {
    let parts = Str.split(template, "{{{{")
    let head = List.at(parts, 0) |> unwrapOr("")
    concat(isEmptyStr(head) => [] | [Text(head)], flatMap(List.removeAt(parts, 0), tagSegments))
}

prod EachSplit(body: List<Segment>, after: List<Segment>)

splitEachBlock(segments: List<Segment>) -> Result<EachSplit, Str> = splitEachLoop(segments, [])

splitEachLoop(segments: List<Segment>, body: List<Segment>) -> Result<EachSplit, Str> =
    match first(segments) {
        None => Err("unterminated {{{{#each}}}}")
        Some(EachEnd) => Ok(EachSplit(reverse(body), List.removeAt(segments, 0)))
        Some(seg) => splitEachLoop(List.removeAt(segments, 0), concat([seg], body))
    }

lookupVar(context: Map<Str, Value>, name: Str) -> Result<Str, Str> =
    match Map.get(context, name) {
        None => Err("unknown variable: " + name)
        Some(v) => match v { VStr(s) => Ok(s)  VList(_) => Err(name + " is a list, not a value") }
    }

lookupList(context: Map<Str, Value>, name: Str) -> Result<List<Value>, Str> =
    match Map.get(context, name) {
        None => Err("unknown variable: " + name)
        Some(v) => match v { VList(items) => Ok(items)  VStr(_) => Err(name + " is a value, not a list") }
    }

render(template: Str, context: Map<Str, Value>) -> Result<Str, Str> = renderSegments(segmentsOf(template), context)

renderSegments(segments: List<Segment>, context: Map<Str, Value>) -> Result<Str, Str> =
    match first(segments) {
        None => Ok("")
        Some(Text(t)) => {
            let rest = renderSegments(List.removeAt(segments, 0), context)?
            Ok(t + rest)
        }
        Some(Var(name)) => {
            let v = lookupVar(context, name)?
            let rest = renderSegments(List.removeAt(segments, 0), context)?
            Ok(v + rest)
        }
        Some(EachStart(name)) => {
            let split = splitEachBlock(List.removeAt(segments, 0))?
            let items = lookupList(context, name)?
            let rendered = renderEach(split.body, items)?
            let rest = renderSegments(split.after, context)?
            Ok(rendered + rest)
        }
        Some(EachEnd) => Err("unexpected {{{{/each}}}} with no matching {{{{#each}}}}")
    }

renderEach(body: List<Segment>, items: List<Value>) -> Result<Str, Str> =
    match first(items) {
        None => Ok("")
        Some(item) => {
            let rendered = renderSegments(body, Map.fromList([(".", item)]))?
            let rest = renderEach(body, List.removeAt(items, 0))?
            Ok(rendered + rest)
        }
    }

// --- tests ---

test "plain text with no tags renders unchanged" {
    expect render("hello, world", [:]) == Ok("hello, world")
}

test "a variable tag interpolates its bound string" {
    expect render("Hello, {{{{name}}}}!", ["name": VStr("Ada")]) == Ok("Hello, Ada!")
}

test "an unbound variable is a named error" {
    match render("Hi {{{{name}}}}", [:]) {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "an each block repeats its body once per list element" {
    let ctx = ["items": VList([VStr("a"), VStr("b"), VStr("c")])]
    expect render("Items: {{{{#each items}}}}[{{{{.}}}}]{{{{/each}}}}", ctx) == Ok("Items: [a][b][c]")
}

test "an each block over an empty list renders as nothing" {
    let ctx = ["items": VList([])]
    expect render("Items: {{{{#each items}}}}[{{{{.}}}}]{{{{/each}}}} done", ctx) == Ok("Items:  done")
}

test "text and variables can surround an each block" {
    let ctx = ["who": VStr("Ada"), "nums": VList([VStr("1"), VStr("2")])]
    expect render("{{{{who}}}}: {{{{#each nums}}}}({{{{.}}}}){{{{/each}}}}!", ctx) == Ok("Ada: (1)(2)!")
}

test "an unterminated each block is a named error" {
    match render("{{{{#each items}}}}[{{{{.}}}}]", ["items": VList([VStr("a")])]) {
        Err(_) => expect true
        Ok(_) => expect false
    }
}
