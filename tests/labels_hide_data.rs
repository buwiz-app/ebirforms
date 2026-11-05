//! What the Bot's assistant reads of a screen is button labels and field
//! names, and no person's or business's data (the Bot's
//! `app/src/agent_hub/filter.rs`, owner decision O1). A button whose
//! accessibility label differs from its text goes out unnamed, so a button
//! that shows a name carries fixed words as its label. This pins them: a
//! redesign that drops one brings the name back, and nothing else would
//! say so.

const CALENDAR: &str = include_str!("../apps/ebirforms/calendar.tsx");

#[test]
fn the_calendars_taxpayer_option_says_this_taxpayer_not_a_name() {
    assert!(CALENDAR.contains(r#"{ value: "mine", label: mineLabel(), said: "This taxpayer" }"#));
}

#[test]
fn segmented_labels_an_option_that_asks() {
    let at = CALENDAR
        .find("function Segmented")
        .expect("Segmented is gone");
    let label = CALENDAR[at..]
        .find("accessibilityLabel={o().said}")
        .expect("its label is gone");
    let words = CALENDAR[at..]
        .find("{o().label}</Text>")
        .expect("its text is gone");
    assert!(label < words, "the label has to be on the option's row");
}
