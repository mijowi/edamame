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

/// Like [`next_focusable`], but *wrapping*: stepping past the last
/// focusable row continues from the first (and vice versa).  Walks at most
/// `rows.len()` steps so an all-non-focusable set terminates with `None`
/// rather than looping forever.  The walk steps `delta` at a time starting
/// one step from `current`, so when `current` is the only focusable row it
/// is reached on the final wrap step and returned unchanged (a no-op move,
/// matching the prior welcome behavior).
///
/// Used by the welcome and export-HTML modals, whose focus rings wrap (Tab
/// off the last control returns to the first); the settings / keybinds
/// overlays use the non-wrapping [`next_focusable`] instead.
pub fn next_focusable_wrapping<T>(
    rows: &[T],
    current: usize,
    delta: i32,
    is_focusable: impl Fn(&T) -> bool,
) -> Option<usize> {
    if rows.is_empty() || delta == 0 {
        return None;
    }
    let len = rows.len() as i32;
    let mut idx = current as i32;
    // At most `len` steps: enough to visit every slot for a ±1 `delta`,
    // and a hard bound so an all-disabled ring can't spin forever.
    for _ in 0..len {
        idx = (idx + delta).rem_euclid(len);
        let i = idx as usize;
        if is_focusable(&rows[i]) {
            return Some(i);
        }
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

    // ── next_focusable_wrapping ──────────────────────────────────────────

    #[test]
    fn wrapping_wraps_forward_off_the_end() {
        let rows = [true, true, true];
        assert_eq!(next_focusable_wrapping(&rows, 2, 1, |r| *r), Some(0));
    }

    #[test]
    fn wrapping_wraps_backward_off_the_start() {
        let rows = [true, true, true];
        assert_eq!(next_focusable_wrapping(&rows, 0, -1, |r| *r), Some(2));
    }

    #[test]
    fn wrapping_skips_non_focusable_then_wraps() {
        // From the last focusable row, forward skips the trailing
        // non-focusable tail and lands back on the first.
        let rows = [true, false, true, false, false];
        assert_eq!(next_focusable_wrapping(&rows, 2, 1, |r| *r), Some(0));
        // Backward from the first focusable skips back over the head and
        // wraps to the last focusable row.
        assert_eq!(next_focusable_wrapping(&rows, 0, -1, |r| *r), Some(2));
    }

    #[test]
    fn wrapping_lone_focusable_returns_itself() {
        // Only `current` is focusable: the walk wraps all the way around
        // and lands back on `current` (a no-op move, not a panic / None).
        let rows = [false, true, false];
        assert_eq!(next_focusable_wrapping(&rows, 1, 1, |r| *r), Some(1));
        assert_eq!(next_focusable_wrapping(&rows, 1, -1, |r| *r), Some(1));
    }

    #[test]
    fn wrapping_all_non_focusable_returns_none() {
        let rows = [false, false, false];
        assert_eq!(next_focusable_wrapping(&rows, 0, 1, |r| *r), None);
    }

    #[test]
    fn wrapping_zero_delta_and_empty_return_none() {
        let rows = [true, true];
        assert_eq!(next_focusable_wrapping(&rows, 0, 0, |r| *r), None);
        let empty: [bool; 0] = [];
        assert_eq!(next_focusable_wrapping(&empty, 0, 1, |r| *r), None);
    }
}
