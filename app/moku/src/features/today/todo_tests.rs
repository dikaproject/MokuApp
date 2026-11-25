//! To-do add / edit / delete / snooze, with due times and persistence (test module of `today`).

use std::path::PathBuf;

use chrono::{NaiveDate, NaiveDateTime};

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("moku-today-todo-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Wednesday 2026-11-25 10:00.
fn now() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(10, 0, 0).unwrap()
}

fn due_of(t: &Todo) -> NaiveDateTime {
    local_of(t.due.expect("a due time")).unwrap()
}

#[test]
fn adding_reads_the_due_time_out_of_the_words() {
    let mut l = vec![];
    let a = add_with_due(&mut l, "rapat jam 3", now()).unwrap();
    let b = add_with_due(&mut l, "kirim laporan besok 09:00", now()).unwrap();
    let c = add_with_due(&mut l, "beli kopi", now()).unwrap();
    assert_eq!(l.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["rapat", "kirim laporan", "beli kopi"]);
    assert_eq!(due_of(&l[0]).to_string(), "2026-11-25 15:00:00");
    assert_eq!(due_of(&l[1]).to_string(), "2026-11-26 09:00:00");
    assert_eq!(l[2].due, None);
    assert!(l.iter().all(|t| !t.done && !t.fired));
    assert!(a != b && b != c);
    // Whitespace is normalised and an empty text is refused, as before.
    assert!(add_with_due(&mut l, "   ", now()).is_err());
    assert!(add_with_due(&mut l, "jam 3", now()).is_ok(), "only a time: the text stays");
    assert_eq!(l[3].text, "jam 3");
    // Bounded like the plain add.
    let long = format!("{} jam 3", "x".repeat(10_000));
    add_with_due(&mut l, &long, now()).unwrap();
    assert!(l.last().unwrap().text.chars().count() <= MAX_TODO_CHARS);
}

#[test]
fn editing_changes_the_text_and_maybe_the_time() {
    let mut l = vec![];
    let id = add_with_due(&mut l, "rapat jam 3", now()).unwrap();
    l[0].fired = true;
    // New words without a time: the text changes, the time and the "already reminded" stay.
    assert_eq!(edit_todo(&mut l, id, "rapat tim produk", now()), Ok(true));
    assert_eq!(l[0].text, "rapat tim produk");
    assert_eq!(due_of(&l[0]).to_string(), "2026-11-25 15:00:00");
    assert!(l[0].fired);
    // A new time replaces the old one and arms the reminder again.
    assert_eq!(edit_todo(&mut l, id, "rapat tim produk besok 09:00", now()), Ok(true));
    assert_eq!(l[0].text, "rapat tim produk");
    assert_eq!(due_of(&l[0]).to_string(), "2026-11-26 09:00:00");
    assert!(!l[0].fired);
    // Nothing to edit, or nothing typed.
    assert_eq!(edit_todo(&mut l, 999, "x", now()), Ok(false));
    assert_eq!(edit_todo(&mut l, id, "   ", now()), Err("empty text"));
    assert_eq!(l[0].text, "rapat tim produk", "an empty edit changes nothing");
    // Removing the time.
    assert!(clear_due(&mut l, id));
    assert_eq!(l[0].due, None);
    assert!(!clear_due(&mut l, 999));
}

#[test]
fn deleting_and_snoozing() {
    let mut l = vec![];
    let a = add_with_due(&mut l, "a jam 3", now()).unwrap();
    let b = add_with_due(&mut l, "b", now()).unwrap();
    assert!(delete_todo(&mut l, a));
    assert!(!delete_todo(&mut l, a), "already gone");
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].id, b);
    // Ids are not reused while others exist: the next is the highest + 1.
    let c = add_todo(&mut l, "c").unwrap();
    assert_eq!(c, b + 1);
    // Snooze: 10 minutes from now, and the reminder is armed again.
    l[0].fired = true;
    assert!(snooze_todo(&mut l, b, 1_000_000, 10));
    assert_eq!((l[0].due, l[0].fired), (Some(1_000_600), false));
    assert!(!snooze_todo(&mut l, 999, 1_000_000, 10));
    assert!(mark_fired(&mut l, b) && l[0].fired);
    assert!(!mark_fired(&mut l, 999));
}

#[test]
fn due_times_and_reminder_state_persist() {
    let dir = temp_dir("persist");
    let path = dir.join("todo.json");
    let mut l = vec![];
    add_with_due(&mut l, "rapat jam 3", now()).unwrap();
    add_with_due(&mut l, "beli kopi", now()).unwrap();
    l[0].fired = true;
    save_todos(&path, &l).unwrap();
    let back = load_todos(&path);
    assert_eq!(back, l);
    assert!(back[0].due.is_some() && back[0].fired);
    // A to-do with no due time writes no due / fired keys: the file stays what v0.2 wrote.
    let text = std::fs::read_to_string(&path).unwrap();
    let second: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(second[1].get("due").is_none() && second[1].get("fired").is_none(), "{text}");
    // Edits and deletes survive a reload too.
    let mut back = back;
    let (first, second_id) = (back[0].id, back[1].id);
    edit_todo(&mut back, second_id, "beli kopi susu", now()).unwrap();
    delete_todo(&mut back, first);
    save_todos(&path, &back).unwrap();
    let again = load_todos(&path);
    assert_eq!(again.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["beli kopi susu"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_v0_2_file_still_loads() {
    let dir = temp_dir("old-file");
    let path = dir.join("todo.json");
    std::fs::write(&path, r#"[{"id":1,"text":"Bayar tagihan","done":false},{"id":2,"text":"Review PR","done":true}]"#).unwrap();
    let l = load_todos(&path);
    assert_eq!(l.len(), 2);
    assert!(l.iter().all(|t| t.due.is_none() && !t.fired));
    assert!(l[1].done);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_preview_for_the_launcher_and_the_field() {
    // Only an invented phrase with a time: the label is in the language of the island (Indonesian
    // unless another test changed it), so compare the parts that do not depend on it.
    let (title, label) = preview("rapat jam 15.30").unwrap();
    assert_eq!(title, "rapat");
    assert!(label.ends_with("15.30") || label.ends_with("15:30"), "{label}");
    assert!(preview("beli kopi").is_none());
    assert!(preview("").is_none());
}
