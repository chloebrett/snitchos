// inventory.st — warehouse inventory valued under pluggable pricing
// strategies (`contract PricingStrategy`), with a per-category telemetry
// report. The "business logic" flavored entry in the batch — a deliberate
// change of register from the systems-y/parser-y programs so far
// (json.st/vm.st/calc.st) and the graph-y ones (graph.st/sched.st).
//
// Grouping items by category is another association-list case
// (`List<CategoryTotal>`, not `Map<Str, Int>`) — see json.st's entry in
// plans/stitch-examples-findings.md for why. By now (program 8) that's
// just how you write "group by a computed key" in this language, not a
// workaround worth re-explaining at length.

use Str

// --- items and strategies ---

prod Item(name: Str, category: Str, basePrice: Int, quantity: Int)

contract PricingStrategy {
    price(item: Item) -> Int
}

prod FlatPricing()

on FlatPricing : PricingStrategy {
    price(item: Item) -> Int = item.basePrice
}

// A flat percentage off, but only once `quantity` reaches `threshold` — the
// discount is all-or-nothing per line, not prorated per unit above the
// threshold (the simpler, and more common, real-world shape: "10 or more,
// 15% off the whole line").
prod BulkDiscount(threshold: Int, percentOff: Int)

on BulkDiscount : PricingStrategy {
    price(item: Item) -> Int =
        item.quantity >= @threshold
            => item.basePrice - (item.basePrice * @percentOff / 100)
            | item.basePrice
}

// `percentAdjust` can be negative (a markdown) — Stitch's unary `-` is an
// ordinary prefix operator (`plans/lang/01-grammar-and-precedence.md`), so
// `Seasonal(-20)` reads exactly like the number it is; no `0 - 20` workaround
// needed the way earlier programs in this batch wrote negative *results*
// (those were computed values inside expressions, not literals — a
// difference that didn't end up mattering here, but is worth knowing this
// works directly).
prod Seasonal(percentAdjust: Int)

on Seasonal : PricingStrategy {
    price(item: Item) -> Int = item.basePrice + (item.basePrice * @percentAdjust / 100)
}

// --- the inventory ---

prod Inventory(items: List<Item>)

lineValue(item: Item, strategy) -> Int = strategy.price(item) * item.quantity

totalValue(inv: Inventory, strategy) -> Int =
    inv.items |> map(item -> lineValue(item, strategy)) |> total

prod CategoryTotal(category: Str, total: Int)

categoryTotals(inv: Inventory, strategy) -> List<CategoryTotal> =
    fold(inv.items, [], (acc, item) -> addToCategory(acc, item.category, lineValue(item, strategy)))

addToCategory(acc: List<CategoryTotal>, category: Str, amount: Int) -> List<CategoryTotal> =
    match find(acc, c -> c.category == category) {
        None => concat(acc, [CategoryTotal(category, amount)])
        Some(_) => map(acc, c -> c.category == category => CategoryTotal(category, c.total + amount) | c)
    }

// One gauge per category (so Grafana shows the inventory's value broken
// down the way a stock report would), plus the same numbers as a
// human-readable string — the two forms `primes.st` already keeps side by
// side (telemetry for the dashboard, a return value for the caller).
report(inv: Inventory, strategy) -> Str uses Telemetry {
    use <- span("inventory.report")
    let totals = categoryTotals(inv, strategy)
    each(totals, t -> emit("inventory.category." + t.category + ".value", t.total))
    totals |> map(t -> t.category + ": $" + toStr(t.total)) |> Str.join("\n")
}

// --- tests ---

test "FlatPricing charges the base price, no adjustment" {
    let item = Item("widget", "hardware", 100, 5)
    expect FlatPricing().price(item) == 100
}

test "BulkDiscount is unchanged below the threshold" {
    let item = Item("widget", "hardware", 100, 3)
    expect BulkDiscount(10, 15).price(item) == 100
}

test "BulkDiscount applies at and above the threshold" {
    let item = Item("widget", "hardware", 100, 10)
    expect BulkDiscount(10, 15).price(item) == 85
    let big = Item("widget", "hardware", 100, 50)
    expect BulkDiscount(10, 15).price(big) == 85
}

test "Seasonal marks a price up or down by a signed percentage" {
    let item = Item("widget", "hardware", 100, 1)
    expect Seasonal(20).price(item) == 120
    expect Seasonal(-20).price(item) == 80
    expect Seasonal(0).price(item) == 100
}

test "lineValue multiplies the strategy's price by quantity" {
    let item = Item("widget", "hardware", 100, 4)
    expect lineValue(item, FlatPricing()) == 400
    expect lineValue(item, Seasonal(10)) == 440
}

test "totalValue sums every line under the given strategy" {
    let inv = Inventory([
        Item("widget", "hardware", 100, 2),
        Item("bolt", "hardware", 10, 50),
        Item("shirt", "apparel", 20, 3),
    ])
    expect totalValue(inv, FlatPricing()) == 200 + 500 + 60
}

test "categoryTotals groups items across categories, preserving first-seen order" {
    let inv = Inventory([
        Item("widget", "hardware", 100, 2),
        Item("shirt", "apparel", 20, 3),
        Item("bolt", "hardware", 10, 50),
    ])
    expect categoryTotals(inv, FlatPricing()) == [
        CategoryTotal("hardware", 200 + 500),
        CategoryTotal("apparel", 60),
    ]
}

test "different strategies value the same inventory differently" {
    let inv = Inventory([Item("widget", "hardware", 100, 10)])
    expect totalValue(inv, FlatPricing()) == 1000
    expect totalValue(inv, BulkDiscount(10, 15)) == 850
    expect totalValue(inv, Seasonal(-10)) == 900
}

test "report renders one line per category and emits its telemetry" uses Telemetry {
    let inv = Inventory([
        Item("widget", "hardware", 100, 2),
        Item("shirt", "apparel", 20, 3),
    ])
    expect report(inv, FlatPricing()) == "hardware: $200\napparel: $60"
}

test "an empty inventory reports as an empty string" uses Telemetry {
    expect report(Inventory([]), FlatPricing()) == ""
    expect totalValue(Inventory([]), FlatPricing()) == 0
}
