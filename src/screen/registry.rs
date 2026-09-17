use crate::screen::ui_element::UiElement;
use std::collections::HashMap;

/// Assigns stable IDs to UI elements across steps. Same element (by
/// fingerprint) gets the same ID every time it appears. IDs never get
/// recycled within a session, so the model can refer to `#22` in step 1 and
/// still mean the same thing in step 10.
pub struct ElementRegistry {
    id_by_fingerprint: HashMap<u64, usize>,
    next_id: usize,
}

impl ElementRegistry {
    pub fn new() -> Self {
        Self {
            id_by_fingerprint: HashMap::new(),
            next_id: 1,
        }
    }

    /// Whether this registry has handed out anything yet. A fresh one in a new
    /// process would restart numbering at 1, which is why it gets seeded.
    pub fn is_empty(&self) -> bool {
        self.id_by_fingerprint.is_empty()
    }

    /// Every assignment made so far, newest ids first. Persisted so a later
    /// process can adopt the whole map: seeding from only the last screen's
    /// elements loses every id for a screen you navigated away from, which is
    /// exactly the case where an agent refers back to a number it saw earlier.
    pub fn assignments(&self, limit: usize) -> Vec<(u64, usize)> {
        let mut all: Vec<(u64, usize)> = self
            .id_by_fingerprint
            .iter()
            .map(|(fp, id)| (*fp, *id))
            .collect();
        all.sort_by_key(|(_, id)| std::cmp::Reverse(*id));
        all.truncate(limit);
        all
    }

    /// Adopt the ids an earlier process handed out, so the same element keeps its
    /// number across a process boundary and not only across steps within one.
    /// Without this the fingerprints written to disk had no reader.
    pub fn seed(&mut self, assigned: impl IntoIterator<Item = (u64, usize)>) {
        for (fp, id) in assigned {
            self.id_by_fingerprint.insert(fp, id);
            self.next_id = self.next_id.max(id + 1);
        }
    }

    /// Return (id, element) pairs for each input element, assigning or
    /// reusing IDs via the fingerprint map.
    pub fn assign<'a, I>(&mut self, elements: I) -> Vec<(usize, &'a UiElement)>
    where
        I: IntoIterator<Item = &'a UiElement>,
    {
        elements
            .into_iter()
            .map(|e| {
                let fp = e.fingerprint();
                let id = *self.id_by_fingerprint.entry(fp).or_insert_with(|| {
                    let id = self.next_id;
                    self.next_id += 1;
                    id
                });
                (id, e)
            })
            .collect()
    }
}

impl Default for ElementRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod seeding {
    use super::*;
    use crate::screen::ui_element::{Bounds, UiElement};

    fn el(text: &str) -> UiElement {
        UiElement {
            class: "android.widget.Button".to_string(),
            text: text.to_string(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: Bounds::new(0, 0, 50, 50),
            clickable: true,
            editable: false,
            is_password: false,
            focused: false,
            scrollable: false,
            enabled: true,
            visible: true,
            checked: false,
            selected: false,
            package: "com.app".to_string(),
        }
    }

    #[test]
    fn a_seeded_registry_reuses_the_previous_process_ids() {
        // The fingerprints written to disk had no reader, so a fresh CLI process
        // restarted numbering at 1 and a number meant something different from
        // what the previous process handed out.
        let submit = el("Submit");
        let cancel = el("Cancel");

        let mut first = ElementRegistry::new();
        let assigned = first.assign(vec![&submit, &cancel]);
        let submit_id = assigned[0].0;
        let cancel_id = assigned[1].0;

        let mut fresh = ElementRegistry::new();
        assert!(fresh.is_empty());
        fresh.seed(vec![
            (submit.fingerprint(), submit_id),
            (cancel.fingerprint(), cancel_id),
        ]);

        // Same elements, same numbers, different process.
        let again = fresh.assign(vec![&submit, &cancel]);
        assert_eq!(again[0].0, submit_id);
        assert_eq!(again[1].0, cancel_id);

        // And a genuinely new element does not collide with a reused id.
        let extra = el("Retry");
        let third = fresh.assign(vec![&extra]);
        assert!(
            third[0].0 > submit_id.max(cancel_id),
            "a new element must take a fresh id, not reuse a seeded one"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::ui_element::Bounds;

    fn mk(text: &str, x: i32, y: i32) -> UiElement {
        UiElement {
            class: "Button".into(),
            text: text.into(),
            content_desc: String::new(),
            resource_id: format!("app:id/{}", text),
            bounds: Bounds::new(x, y, x + 100, y + 50),
            clickable: true,
            editable: false,
            is_password: false,
            focused: false,
            scrollable: false,
            enabled: true,
            visible: true,
            checked: false,
            selected: false,
            package: "com.app".into(),
        }
    }

    #[test]
    fn same_element_same_id_across_calls() {
        let mut reg = ElementRegistry::new();
        let a = mk("OK", 100, 200);
        let b = mk("Cancel", 300, 200);

        let step1 = reg.assign([&a, &b]);
        let step2 = reg.assign([&a, &b]);

        assert_eq!(step1[0].0, step2[0].0, "OK must keep its ID");
        assert_eq!(step1[1].0, step2[1].0, "Cancel must keep its ID");
    }

    #[test]
    fn new_element_gets_new_id_without_shifting_existing() {
        let mut reg = ElementRegistry::new();
        let a = mk("A", 0, 0);
        let b = mk("B", 100, 0);
        let c = mk("C", 200, 0);

        let step1 = reg.assign([&a, &b]);
        let a_id = step1[0].0;
        let b_id = step1[1].0;

        let step2 = reg.assign([&a, &b, &c]);
        assert_eq!(step2[0].0, a_id);
        assert_eq!(step2[1].0, b_id);
        assert!(step2[2].0 != a_id && step2[2].0 != b_id);
    }

    #[test]
    fn disappeared_element_does_not_shift_others() {
        let mut reg = ElementRegistry::new();
        let a = mk("A", 0, 0);
        let b = mk("B", 100, 0);

        let step1 = reg.assign([&a, &b]);
        let b_id = step1[1].0;

        // A disappears; B should keep its ID
        let step2 = reg.assign([&b]);
        assert_eq!(step2[0].0, b_id);
    }

    #[test]
    fn same_label_different_position_are_distinct() {
        let mut reg = ElementRegistry::new();
        let top = mk("More", 500, 100);
        let bottom = mk("More", 500, 2000);
        let ids = reg.assign([&top, &bottom]);
        assert!(ids[0].0 != ids[1].0);
    }
}
