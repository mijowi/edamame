use std::fmt::Write;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

/// Number of columns occupied by the line-number gutter: the digit
/// width needed for `line_count` plus two columns of trailing padding
/// between the last digit and the content.
pub fn gutter_width(line_count: usize) -> u16 {
    if line_count == 0 {
        return 0;
    }
    digit_count(line_count) as u16 + 2
}

/// Split a full document area into an optional gutter rect (left) and
/// the remaining content rect (right).  Returns `(None, full)` when
/// `line_count` is 0 or the area is too narrow for even one digit.
pub fn split_gutter(full: Rect, line_count: usize) -> (Option<Rect>, Rect) {
    let gw = gutter_width(line_count);
    if gw == 0 || full.width <= gw {
        return (None, full);
    }
    let gutter = Rect {
        x: full.x,
        y: full.y,
        width: gw,
        height: full.height,
    };
    let content = Rect {
        x: full.x + gw,
        width: full.width - gw,
        ..full
    };
    (Some(gutter), content)
}

/// Paint right-aligned line numbers into `gutter_area`.
///
/// `line_at_visual_row` maps a global visual-row index (scroll +
/// screen row) to `(logical_line_index, sub_row_within_that_line)`.
/// Sub-row 0 gets a number; continuation rows are left blank.
pub fn paint_gutter(
    buf: &mut Buffer,
    gutter_area: Rect,
    scroll: usize,
    line_count: usize,
    line_at_visual_row: impl Fn(usize, usize) -> (usize, usize),
    width: usize,
    style: Style,
) {
    if gutter_area.width == 0 || gutter_area.height == 0 {
        return;
    }
    let digit_w = digit_count(line_count);
    let mut num_buf = String::with_capacity(digit_w + 2);

    for vis_y in 0..gutter_area.height {
        let global_row = scroll + vis_y as usize;
        let (line_idx, sub_row) = line_at_visual_row(global_row, width);

        let y = gutter_area.y + vis_y;

        if sub_row == 0 && line_idx < line_count {
            num_buf.clear();
            let _ = write!(num_buf, "{:>w$}  ", line_idx + 1, w = digit_w);
            buf.set_string(gutter_area.x, y, &num_buf, style);
        } else {
            for x in gutter_area.x..gutter_area.x + gutter_area.width {
                let cell = &mut buf[(x, y)];
                cell.set_char(' ');
                cell.set_style(style);
            }
        }
    }
}

fn digit_count(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_count_edge_cases() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
        assert_eq!(digit_count(999), 3);
        assert_eq!(digit_count(1000), 4);
    }

    #[test]
    fn gutter_width_includes_padding() {
        assert_eq!(gutter_width(0), 0); // empty document — no gutter
        assert_eq!(gutter_width(1), 3); // 1 digit + 2 padding
        assert_eq!(gutter_width(9), 3);
        assert_eq!(gutter_width(10), 4); // 2 digits + 2 padding
        assert_eq!(gutter_width(100), 5); // 3 digits + 2 padding
    }
}
