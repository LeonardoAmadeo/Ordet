//! Explicit split-based layout engine.

use crate::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    /// Exact number of cells (shrunk only on overflow).
    Length(u16),
    /// At least `n` cells; absorbs remaining space if no `Fill` constraints exist.
    Min(u16),
    /// At most `n` cells.
    Max(u16),
    /// Percentage of the available axis (`0..=100`).
    Percentage(u16),
    /// Proportional share of remaining space (flex weight).
    Fill(u16),
}

/// Splits `area` along `direction` according to `constraints`.
pub fn split(area: Rect, direction: Direction, constraints: &[Constraint]) -> Vec<Rect> {
    if constraints.is_empty() {
        return Vec::new();
    }

    let total = match direction {
        Direction::Horizontal => area.width,
        Direction::Vertical => area.height,
    };

    let mut lengths = constraints
        .iter()
        .map(|constraint| match *constraint {
            Constraint::Length(value) => value,
            Constraint::Percentage(percent) => {
                (u32::from(total) * u32::from(percent.min(100)) / 100) as u16
            }
            Constraint::Min(value) => value.min(total),
            Constraint::Max(value) => value.min(total),
            Constraint::Fill(_) => 0,
        })
        .collect::<Vec<_>>();

    let mut used: u16 = lengths.iter().fold(0u16, |acc, &v| acc.saturating_add(v));

    if used < total {
        let remaining = total - used;
        let fill_total: u32 = constraints
            .iter()
            .filter_map(|constraint| match *constraint {
                Constraint::Fill(weight) => Some(u32::from(weight.max(1))),
                _ => None,
            })
            .sum();

        if fill_total > 0 {
            let mut distributed = 0u16;
            let mut last_fill_idx = None;
            for (index, constraint) in constraints.iter().enumerate() {
                if let Constraint::Fill(weight) = *constraint {
                    let share = (u32::from(remaining) * u32::from(weight.max(1)))
                        .checked_div(fill_total)
                        .unwrap_or(0) as u16;
                    lengths[index] = lengths[index].saturating_add(share);
                    distributed = distributed.saturating_add(share);
                    last_fill_idx = Some(index);
                }
            }
            if let Some(index) = last_fill_idx {
                let leftover = remaining.saturating_sub(distributed);
                lengths[index] = lengths[index].saturating_add(leftover);
            }
            used = total;
        } else if let Some(index) = constraints
            .iter()
            .enumerate()
            .rfind(|(_, constraint)| matches!(constraint, Constraint::Min(_)))
            .map(|(index, _)| index)
        {
            lengths[index] = lengths[index].saturating_add(remaining);
            used = total;
        }
    }

    if used < total {
        if let Some(last) = lengths.last_mut() {
            *last = last.saturating_add(total - used);
        }
    }

    if used > total {
        let mut overflow = used - total;
        for length in lengths.iter_mut().rev() {
            let reduction = (*length).min(overflow);
            *length -= reduction;
            overflow -= reduction;
            if overflow == 0 {
                break;
            }
        }
    }

    let mut cursor_x = area.x;
    let mut cursor_y = area.y;
    let mut rects = Vec::with_capacity(lengths.len());

    for length in lengths {
        let rect = match direction {
            Direction::Horizontal => {
                let rect = Rect::new(cursor_x, area.y, length, area.height);
                cursor_x = cursor_x.saturating_add(length);
                rect
            }
            Direction::Vertical => {
                let rect = Rect::new(area.x, cursor_y, area.width, length);
                cursor_y = cursor_y.saturating_add(length);
                rect
            }
        };
        rects.push(rect);
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::{split, Constraint, Direction};
    use crate::Rect;

    #[test]
    fn min_constraint_absorbs_remaining_space() {
        let rects = split(
            Rect::new(0, 0, 20, 4),
            Direction::Horizontal,
            &[Constraint::Length(5), Constraint::Min(1)],
        );
        assert_eq!(rects[0].width, 5);
        assert_eq!(rects[1].width, 15);
    }

    #[test]
    fn fill_constraints_distribute_remaining_space_proportionally() {
        let rects = split(
            Rect::new(0, 0, 100, 10),
            Direction::Horizontal,
            &[
                Constraint::Length(10),
                Constraint::Fill(1),
                Constraint::Fill(2),
            ],
        );
        assert_eq!(rects[0].width, 10);
        assert_eq!(rects[1].width, 30);
        assert_eq!(rects[2].width, 60);
    }

    #[test]
    fn max_constraint_caps_at_available_space() {
        let rects = split(
            Rect::new(0, 0, 10, 5),
            Direction::Horizontal,
            &[Constraint::Max(20), Constraint::Fill(1)],
        );
        assert_eq!(rects[0].width, 10);
        assert_eq!(rects[1].width, 0);
    }

    #[test]
    fn overflow_trims_from_the_end_without_panicking() {
        let rects = split(
            Rect::new(0, 0, 10, 4),
            Direction::Horizontal,
            &[Constraint::Length(8), Constraint::Length(8)],
        );
        assert_eq!(rects[0].width, 8);
        assert_eq!(rects[1].width, 2);
        assert_eq!(rects[1].right(), 10);
    }

    #[test]
    fn empty_constraints_return_empty_vec() {
        assert!(split(Rect::new(0, 0, 80, 24), Direction::Vertical, &[]).is_empty());
    }
}
