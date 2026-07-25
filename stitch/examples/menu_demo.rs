use stitch::complete::{Completion, complete, menu};

fn main() {
    for line in ["use M.", "use", "let x = ", "greet", "use M.{ a", "greet(a"] {
        let text = match complete(line, line.len()) {
            Completion::Forced(lexeme) => format!("inserts {lexeme:?}"),
            Completion::Choices(choices) => menu(&choices),
            Completion::None => "(nothing can follow)".into(),
        };
        println!("{line:>12?}  {text}");
    }
}
