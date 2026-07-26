// csv.st — a CSV parser and writer with quoted-field handling: fields may
// be quoted (`"…"`), a quoted field may contain commas and newlines
// literally, and a literal quote inside one is written `""` (doubled).
// The "boring but real" utility entry in the batch — no recursive-descent
// grammar, no AST, just careful state threading through commas and quotes.

use Str
use List

prod Parsed(value: Str, pos: Int)

// One field starting at `pos`: quoted if it opens with `"`, unquoted
// (running to the next comma or end of line) otherwise.
parseField(line: Str, pos: Int) -> Result<Parsed, Str> =
    (pos < Str.length(line) and Str.slice(line, pos, pos + 1) == "\"")
        => parseQuoted(line, pos + 1, "")
        | Ok(parseUnquoted(line, pos))

parseUnquoted(line: Str, pos: Int) -> Parsed = {
    let end = scanUntilComma(line, pos)
    Parsed(Str.slice(line, pos, end), end)
}

scanUntilComma(line: Str, pos: Int) -> Int =
    pos >= Str.length(line) or Str.slice(line, pos, pos + 1) == ","
        => pos
        | scanUntilComma(line, pos + 1)

// `pos` is just past the opening quote. `""` inside a quoted field is a
// literal `"` (the standard CSV escape — doubling, not backslashing); any
// other character, quoted or not, is taken literally, including a comma.
parseQuoted(line: Str, pos: Int, acc: Str) -> Result<Parsed, Str> =
    pos >= Str.length(line)
        => Err("unterminated quoted field")
        | {
            let c = Str.slice(line, pos, pos + 1)
            c == "\""
                => ((pos + 1 < Str.length(line) and Str.slice(line, pos + 1, pos + 2) == "\"")
                    => parseQuoted(line, pos + 2, acc + "\"")
                    | Ok(Parsed(acc, pos + 1)))
                | parseQuoted(line, pos + 1, acc + c)
        }

parseRow(line: Str) -> Result<List<Str>, Str> = parseRowLoop(line, 0, [])

parseRowLoop(line: Str, pos: Int, acc: List<Str>) -> Result<List<Str>, Str> = {
    let field = parseField(line, pos)?
    let grown = concat(acc, [field.value])
    field.pos >= Str.length(line)
        => Ok(grown)
        | (Str.slice(line, field.pos, field.pos + 1) == ","
            => parseRowLoop(line, field.pos + 1, grown)
            | Err("expected ',' after field"))
}

parseCsv(text: Str) -> Result<List<List<Str>>, Str> =
    parseRows(Str.split(text, "\n") |> filter(line -> line != ""))

parseRows(lines: List<Str>) -> Result<List<List<Str>>, Str> =
    match first(lines) {
        None => Ok([])
        Some(line) => {
            let row = parseRow(line)?
            let rest = parseRows(List.removeAt(lines, 0))?
            Ok(concat([row], rest))
        }
    }

// --- writing ---

needsQuoting(field: Str) -> Bool =
    Str.contains(field, ",") or Str.contains(field, "\"") or Str.contains(field, "\n")

escapeField(field: Str) -> Str =
    needsQuoting(field)
        => "\"" + Str.replace(field, "\"", "\"\"") + "\""
        | field

writeRow(fields: List<Str>) -> Str = fields |> map(escapeField) |> Str.join(",")

writeCsv(rows: List<List<Str>>) -> Str = rows |> map(writeRow) |> Str.join("\n")

// --- tests ---

test "parseRow splits plain unquoted fields on commas" {
    expect parseRow("a,b,c") == Ok(["a", "b", "c"])
}

test "parseRow keeps a comma inside a quoted field literal" {
    expect parseRow("a,\"b,c\",d") == Ok(["a", "b,c", "d"])
}

test "parseRow unescapes a doubled quote inside a quoted field" {
    expect parseRow("a,\"say \"\"hi\"\"\",c") == Ok(["a", "say \"hi\"", "c"])
}

test "parseRow treats consecutive or trailing commas as empty fields" {
    expect parseRow("a,,c") == Ok(["a", "", "c"])
    expect parseRow("a,b,") == Ok(["a", "b", ""])
}

test "an unterminated quoted field is a named error" {
    match parseRow("a,\"unterminated") {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "parseCsv reads multiple rows, dropping blank lines" {
    expect parseCsv("a,b\n1,2\n\n3,4") == Ok([["a", "b"], ["1", "2"], ["3", "4"]])
}

test "writeRow quotes only the fields that need it" {
    expect writeRow(["plain", "has,comma", "has\"quote"]) == "plain,\"has,comma\",\"has\"\"quote\""
}

test "writeCsv joins rows with newlines" {
    expect writeCsv([["a", "b"], ["1", "2"]]) == "a,b\n1,2"
}

test "writing then parsing reproduces the original rows" {
    let rows = [["name", "note"], ["ada", "has, a comma"], ["bo", "says \"hi\""], ["cy", ""]]
    expect parseCsv(writeCsv(rows)) == Ok(rows)
}
