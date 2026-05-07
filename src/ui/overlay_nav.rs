//! Shared focus navigation for the modal overlays.
//!
//! Settings, keybinds, and similar list-style overlays all need the
//! same "advance focus by N, skipping non-focusable rows" loop.  The
//! row types differ — settings has `RowDef { kind: { focusable } }`,
//! keybinds has an enum with `Header` and `Binding` variants — so the
//! helper takes a predicate.
//!
//! The walk is non-wrapping by design: bouncing past the first/last
//! focusable row makes overlay navigation feel jumpy.  When no row in
//! the requested direction is focusable, returns `None` so the caller
//! can leave focus where it was.

/// Find the nearest row index reached by stepping `delta` from
/// `current`, skipping any row for which `is_focusable` returns
/// `false`.  Returns `None` when no focusable row exists in that
/// direction.
pub fn next_focusable<T>(
    rows: &[T],
    current: usize,
    delta: i32,
    is_focusable: impl Fn(&T) -> bool,
) -> Option<usize> {
    if rows.is_empty() || delta == 0 {
        return None;
    }
    let len = rows.len() as i32;
    let mut idx = current as i32 + delta;
    while (0..len).contains(&idx) {
        let i = idx as usize;
        if is_focusable(&rows[i]) {
            return Some(i);
        }
        idx += delta;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_focusable_rows_forward() {
        let rows = [true, false, false, true, false, true];
        assert_eq!(next_focusable(&rows, 0, 1, |r| *r), Some(3));
        assert_eq!(next_focusable(&rows, 3, 1, |r| *r), Some(5));
    }

    #[test]
    fn returns_none_when_no_focusable_in_direction() {
        let rows = [true, false, false];
        assert_eq!(next_focusable(&rows, 0, 1, |r| *r), None);
        assert_eq!(next_focusable(&rows, 0, -1, |r| *r), None);
    }

    #[test]
    fn empty_rows_returns_none() {
        let rows: [bool; 0] = [];
        assert_eq!(next_focusable(&rows, 0, 1, |r| *r), None);
    }

    #[test]
    fn zero_delta_returns_none() {
        let rows = [true, true, true];
        assert_eq!(next_focusable(&rows, 1, 0, |r| *r), None);
    }

    #[test]
    fn skips_backward() {
        let rows = [true, false, false, true];
        assert_eq!(next_focusable(&rows, 3, -1, |r| *r), Some(0));
    }
}
