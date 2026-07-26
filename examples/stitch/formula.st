// formula.st — a tiny spreadsheet: cells hold formulas that can reference
// other cells, evaluated recursively with cycle detection. Distinct from
// calc.st (#7): that file evaluates one self-contained expression tree;
// this one's expressions form a *graph* (a cell can reference another
// cell, which references another, …), so evaluation has to track which
// cells are currently being evaluated on the current path — a cycle is a
// cell reappearing on its own evaluation stack, not a parse-time property.
//
// Evaluation is memoized: `Computed(value, cache)` threads a `List<CacheEntry>`
// through every recursive call (the same explicit-cache-threading shape
// lru.st's `fibMemo` used, since there's no mutable global to cache into).

use Str
use List

sum Expr = Lit(Int) | Ref(Str) | Add(Expr, Expr) | Sub(Expr, Expr) | Mul(Expr, Expr)

prod Cell(name: Str, formula: Expr)
prod Sheet(cells: List<Cell>)

findCell(sheet: Sheet, name: Str) -> Maybe<Cell> = find(sheet.cells, c -> c.name == name)

prod CacheEntry(name: Str, value: Int)
prod Computed(value: Int, cache: List<CacheEntry>)

// `path` is every cell name currently being evaluated on the way here —
// finding `name` already in it means evaluating `name` requires evaluating
// `name`, the definition of a cycle. Checked *before* the cache lookup
// would matter (a cell can't be both mid-evaluation and already cached).
evalCell(sheet: Sheet, name: Str, path: List<Str>, cache: List<CacheEntry>) -> Result<Computed, Str> =
    match find(cache, e -> e.name == name) {
        Some(e) => Ok(Computed(e.value, cache))
        None =>
            contains(path, name)
                => Err("cycle detected at cell " + name)
                | match findCell(sheet, name) {
                    None => Err("no such cell: " + name)
                    Some(cell) => {
                        let r = evalExpr(sheet, cell.formula, concat(path, [name]), cache)?
                        Ok(Computed(r.value, concat(r.cache, [CacheEntry(name, r.value)])))
                    }
                }
    }

evalExpr(sheet: Sheet, expr: Expr, path: List<Str>, cache: List<CacheEntry>) -> Result<Computed, Str> =
    match expr {
        Lit(n) => Ok(Computed(n, cache))
        Ref(name) => evalCell(sheet, name, path, cache)
        Add(l, r) => combine(sheet, l, r, path, cache, (a, b) -> a + b)
        Sub(l, r) => combine(sheet, l, r, path, cache, (a, b) -> a - b)
        Mul(l, r) => combine(sheet, l, r, path, cache, (a, b) -> a * b)
    }

// The cache threads left-to-right through the two operands — `r` is
// evaluated with whatever `l` already cached, which is what lets a
// diamond dependency (two cells both referencing a common ancestor)
// compute that ancestor once instead of twice.
combine(sheet: Sheet, l: Expr, r: Expr, path: List<Str>, cache: List<CacheEntry>, f) -> Result<Computed, Str> = {
    let lc = evalExpr(sheet, l, path, cache)?
    let rc = evalExpr(sheet, r, path, lc.cache)?
    Ok(Computed(f(lc.value, rc.value), rc.cache))
}

evalSheet(sheet: Sheet, name: Str) -> Result<Int, Str> = {
    let computed = evalCell(sheet, name, [], [])?
    Ok(computed.value)
}

// --- tests ---

test "a literal cell evaluates to itself" {
    let sheet = Sheet([Cell("A", Lit(5))])
    expect evalSheet(sheet, "A") == Ok(5)
}

test "a cell can reference another cell" {
    let sheet = Sheet([Cell("A", Lit(2)), Cell("B", Add(Ref("A"), Lit(3)))])
    expect evalSheet(sheet, "B") == Ok(5)
}

test "references chain through several cells" {
    let sheet = Sheet([
        Cell("A", Lit(1)),
        Cell("B", Add(Ref("A"), Lit(2))),
        Cell("C", Mul(Ref("B"), Ref("A"))),
    ])
    expect evalSheet(sheet, "C") == Ok(3)
}

test "a direct cycle is reported by name, not looped forever" {
    let sheet = Sheet([Cell("A", Ref("B")), Cell("B", Ref("A"))])
    expect evalSheet(sheet, "A") == Err("cycle detected at cell A")
}

test "a self-reference is a cycle of length one" {
    let sheet = Sheet([Cell("A", Add(Ref("A"), Lit(1)))])
    expect evalSheet(sheet, "A") == Err("cycle detected at cell A")
}

test "referencing a cell that doesn't exist is a named error" {
    let sheet = Sheet([Cell("A", Ref("Q"))])
    expect evalSheet(sheet, "A") == Err("no such cell: Q")
}

test "a shared ancestor is computed once, not once per path to it" {
    // D depends on both B and C, which both depend on A.
    let sheet = Sheet([
        Cell("A", Lit(5)),
        Cell("B", Add(Ref("A"), Lit(1))),
        Cell("C", Mul(Ref("A"), Lit(2))),
        Cell("D", Add(Ref("B"), Ref("C"))),
    ])
    expect evalSheet(sheet, "D") == Ok(16)
    match evalCell(sheet, "D", [], []) {
        Err(_) => expect false
        Ok(computed) => expect count(filter(computed.cache, e -> e.name == "A")) == 1
    }
}
