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
    Length(u16),
    Min(u16),
    Percentage(u16),
}

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
            Constraint::Percentage(percent) => total.saturating_mul(percent).saturating_div(100),
            Constraint::Min(value) => value.min(total),
        })
        .collect::<Vec<_>>();

    let mut used: u16 = lengths.iter().sum();

    if used < total {
        if let Some(index) = constraints
            .iter()
            .enumerate()
            .rfind(|(_, constraint)| matches!(constraint, Constraint::Min(_)))
            .map(|(index, _)| index)
        {
            lengths[index] = lengths[index].saturating_add(total - used);
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
}
