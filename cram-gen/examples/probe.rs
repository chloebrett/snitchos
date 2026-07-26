//! Scratch probe for batch2's failure classes. Delete after the reference is updated.

fn main() {
    let cases: &[(&str, &str)] = &[
        ("let, no block", "ext f() -> Int =\n    let x = 1\n    x\n"),
        ("let, in block", "ext f() -> Int = {\n    let x = 1\n    x\n}\n"),
        ("list pattern", "ext f(xs: List<Int>) -> Int = match xs { [] => 0  _ => 1 }\n"),
        ("three-arm bar", "ext f(n: Int) -> Int = n > 0 => 1 | 2 | 3\n"),
        ("ext prod on fn", "ext prod g(n: Int) -> Int = n\n"),
        ("List.map", "ext f(xs: List<Int>) -> List<Int> = List.map(xs, x -> x)\n"),
        ("bare map", "ext f(xs: List<Int>) -> List<Int> = map(xs, x -> x)\n"),
        ("type alias", "ext prod Schedule = List<Int>\n"),
    ];
    for (name, src) in cases {
        let verdict = match stitch::gate::run(src) {
            stitch::gate::Outcome::Parse(error) => {
                let message = format!("{error:?}");
                let short = message.split("message: ").nth(1).unwrap_or(&message);
                format!("parse — {}", short.split(", span").next().unwrap_or(short))
            }
            other => other.stage().to_string(),
        };
        println!("{name:>16}: {verdict}");
    }
}
