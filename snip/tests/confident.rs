use snip::{Confidence, Excluded, Included, Selection};

fn inc(confidence: Confidence) -> Included {
    Included { path: "a".into(), reason: "r".into(), confidence, hunks: None }
}

fn exc(confidence: Confidence, omitted: bool) -> Excluded {
    Excluded { path: "b".into(), reason: "r".into(), confidence, omitted }
}

fn sel(overall: Confidence, include: Vec<Included>, exclude: Vec<Excluded>) -> Selection {
    Selection { include, exclude, overall, note: None }
}

#[test]
fn all_high_is_confident() {
    let s = sel(Confidence::High, vec![inc(Confidence::High)], vec![exc(Confidence::High, false)]);
    assert!(s.is_confident());
}

#[test]
fn a_medium_include_is_not_confident() {
    let s = sel(Confidence::High, vec![inc(Confidence::Medium)], vec![]);
    assert!(!s.is_confident());
}

#[test]
fn an_explicit_low_exclude_is_not_confident() {
    // The model itself was unsure this belonged out — it might belong in.
    let s = sel(Confidence::High, vec![inc(Confidence::High)], vec![exc(Confidence::Low, false)]);
    assert!(!s.is_confident());
}

#[test]
fn a_low_omitted_exclude_does_not_block() {
    // Files the model never mentioned are Low-by-omission; they don't count.
    let s = sel(Confidence::High, vec![inc(Confidence::High)], vec![exc(Confidence::Low, true)]);
    assert!(s.is_confident());
}

#[test]
fn a_non_high_overall_is_not_confident() {
    let s = sel(Confidence::Medium, vec![inc(Confidence::High)], vec![]);
    assert!(!s.is_confident());
}
