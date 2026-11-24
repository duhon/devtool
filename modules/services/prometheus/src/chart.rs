//! Monochrome Braille charts rendered to text, not a terminal or browser canvas.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    symbols::Marker,
    widgets::{Axis, Chart, Dataset, GraphType, Widget},
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub width: u16,
    pub height: u16,
    pub bounds: Option<[f64; 2]>,
}

impl Options {
    pub fn parse(
        width: Option<&str>,
        height: Option<&str>,
        min: Option<&str>,
        max: Option<&str>,
    ) -> Result<Self, String> {
        fn size(
            value: Option<&str>,
            name: &str,
            default: u16,
            range: [u16; 2],
        ) -> Result<u16, String> {
            match value {
                None => Ok(default),
                Some(value) => value
                    .parse::<u16>()
                    .ok()
                    .filter(|value| (range[0]..=range[1]).contains(value))
                    .ok_or_else(|| format!("{name} must be between {} and {}", range[0], range[1])),
            }
        }
        let bounds = match (min, max) {
            (None, None) => None,
            (Some(min), Some(max)) => {
                let min = min.parse::<f64>().map_err(|_| "Invalid Chart-Min")?;
                let max = max.parse::<f64>().map_err(|_| "Invalid Chart-Max")?;
                if !min.is_finite() || !max.is_finite() || min >= max {
                    return Err("Chart-Min and Chart-Max must be finite, with Min < Max".into());
                }
                Some([min, max])
            }
            _ => return Err("Set both Chart-Min and Chart-Max, or neither".into()),
        };
        Ok(Self {
            width: size(width, "Chart-Width", 96, [40, 160])?,
            height: size(height, "Chart-Height", 20, [8, 40])?,
            bounds,
        })
    }
}

#[derive(Default)]
pub struct State {
    identity: String,
    scales: BTreeMap<String, [f64; 2]>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChartState")
            .field("series", &self.scales.len())
            .finish()
    }
}

fn number(value: f64) -> String {
    let abs = value.abs();
    if abs != 0.0 && !(0.001..1_000_000.0).contains(&abs) {
        format!("{value:.2e}")
    } else {
        let value = format!("{value:.3}");
        value.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn date(value: f64, format: &str) -> String {
    let seconds = value.floor();
    let nanos = ((value - seconds) * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
    chrono::DateTime::from_timestamp(seconds as i64, nanos)
        .map(|time| time.format(format).to_string())
        .unwrap_or_else(|| number(value))
}

fn ratio(value: f64, bounds: [f64; 2]) -> f64 {
    let scale = bounds[0].abs().max(bounds[1].abs()).max(1.0);
    (value / scale - bounds[0] / scale) / (bounds[1] / scale - bounds[0] / scale)
}

fn interpolate(bounds: [f64; 2], fraction: f64) -> f64 {
    bounds[0] * (1.0 - fraction) + bounds[1] * fraction
}

fn expanded(min: f64, max: f64) -> [f64; 2] {
    let padding = if min == max {
        if min == 0.0 { 1.0 } else { min.abs() * 0.1 }
    } else {
        (max * 0.5 - min * 0.5) * 0.1
    };
    let lo = min - padding;
    let hi = max + padding;
    if lo == hi {
        [min.next_down(), max.next_up()]
    } else {
        [
            if lo.is_finite() { lo } else { min },
            if hi.is_finite() { hi } else { max },
        ]
    }
}

// Retain the first, extrema and last samples of each horizontal dot column.
// This limits drawing work without smoothing away peaks or joining separate segments.
fn compress(points: &[(f64, f64)], columns: usize) -> Vec<(f64, f64)> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < points.len() {
        let column = (points[start].0 * columns as f64).floor() as usize;
        let mut end = start + 1;
        while end < points.len() && (points[end].0 * columns as f64).floor() as usize == column {
            end += 1;
        }
        let mut min = start;
        let mut max = start;
        for index in start..end {
            if points[index].1 < points[min].1 {
                min = index;
            }
            if points[index].1 > points[max].1 {
                max = index;
            }
        }
        let mut keep = vec![start, min, max, end - 1];
        keep.sort_unstable();
        keep.dedup();
        result.extend(keep.into_iter().map(|index| points[index]));
        start = end;
    }
    result
}

pub fn render(
    options: &Options,
    interval: [f64; 2],
    step: Option<f64>,
    identity: &str,
    rows: &[Vec<Value>],
    state: &mut State,
) -> Result<String, String> {
    let mut series: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for row in rows {
        if row.get(3).is_some_and(|histogram| !histogram.is_null()) {
            return Err("Charts need numeric samples, not native histograms; query histogram_sum() or histogram_count(), or use table/JSON".into());
        }
        let value = row[2]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or("Charts need numeric samples; use table or JSON for string results")?;
        let labels = row[1].as_object().ok_or("Invalid chart labels")?;
        let labels: BTreeMap<_, _> = labels.iter().collect();
        let label = serde_json::to_string(&labels).expect("labels serialize");
        series
            .entry(label)
            .or_default()
            .push((row[0].as_f64().ok_or("Invalid chart timestamp")?, value));
    }
    if state.identity != identity {
        state.identity = identity.to_owned();
        state.scales.clear();
    }
    let x_bounds = if interval[0] == interval[1] {
        let padding = interval[0].abs().max(1.0) * 1e-6;
        [interval[0] - padding, interval[1] + padding]
    } else {
        interval
    };
    let mut output = format!(
        "UTC {} → {}{}\n",
        date(interval[0], "%Y-%m-%d %H:%M:%S%.3f"),
        date(interval[1], "%Y-%m-%d %H:%M:%S%.3f"),
        step.map(|step| format!(" · step {}s", number(step)))
            .unwrap_or_default(),
    );
    if series.is_empty() {
        output.push_str("No data");
        return Ok(output);
    }
    state.scales.retain(|label, _| series.contains_key(label));
    for (index, (label, mut samples)) in series.into_iter().enumerate() {
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        let finite: Vec<_> = samples.iter().filter(|(_, y)| y.is_finite()).collect();
        let observed = finite.iter().fold(None::<[f64; 2]>, |bounds, (_, value)| {
            Some(match bounds {
                None => [*value, *value],
                Some([min, max]) => [f64::min(min, *value), f64::max(max, *value)],
            })
        });
        let bounds = if let Some(bounds) = options.bounds {
            bounds
        } else {
            let current = state.scales.get(&label).copied();
            let next = match (current, observed) {
                (Some(bounds), Some([min, max])) if min < bounds[0] || max > bounds[1] => {
                    let new = expanded(min, max);
                    [bounds[0].min(new[0]), bounds[1].max(new[1])]
                }
                (Some(bounds), _) => bounds,
                (None, Some([min, max])) => expanded(min, max),
                (None, None) => [-1.0, 1.0],
            };
            state.scales.insert(label.clone(), next);
            next
        };
        let mut segments: Vec<Vec<(f64, f64)>> = vec![vec![]];
        let mut previous = None;
        let mut clipped = 0;
        let mut gaps = 0;
        for (time, value) in &samples {
            if !value.is_finite()
                || (interval[0] != interval[1]
                    && (*time < interval[0] - 0.001 || *time > interval[1] + 0.001))
            {
                if !segments.last().unwrap().is_empty() {
                    segments.push(vec![]);
                }
                previous = None;
                gaps += 1;
                continue;
            }
            if let (Some(before), Some(step)) = (previous, step)
                && *time - before > step * 1.5
            {
                segments.push(vec![]);
                gaps += 1;
            }
            clipped += usize::from(*value < bounds[0] || *value > bounds[1]);
            segments.last_mut().unwrap().push((
                if interval[0] == interval[1] {
                    0.5
                } else {
                    ratio(*time, x_bounds).clamp(0.0, 1.0)
                },
                ratio(*value, bounds).clamp(0.0, 1.0),
            ));
            previous = Some(*time);
        }
        let segments: Vec<_> = segments
            .iter()
            .filter(|segment| !segment.is_empty())
            .map(|segment| compress(segment, usize::from(options.width) * 2))
            .collect();
        let datasets = segments
            .iter()
            .map(|segment| {
                Dataset::default()
                    .data(segment)
                    .marker(Marker::Braille)
                    .graph_type(GraphType::Line)
            })
            .collect();
        let time_format = if interval[1] - interval[0] >= 86_400.0 {
            "%m-%d %H:%M"
        } else {
            "%H:%M:%S"
        };
        let x_labels: Vec<_> = [0.0, 0.5, 1.0]
            .into_iter()
            .map(|fraction| date(interpolate(interval, fraction), time_format))
            .collect();
        let y_labels: Vec<_> = [0.0, 0.25, 0.5, 0.75, 1.0]
            .into_iter()
            .map(|fraction| format!("{:>11}", number(interpolate(bounds, fraction))))
            .collect();
        let area = Rect::new(0, 0, options.width, options.height);
        let mut buffer = Buffer::empty(area);
        Chart::new(datasets)
            .x_axis(Axis::default().bounds([0.0, 1.0]).labels(x_labels))
            .y_axis(Axis::default().bounds([0.0, 1.0]).labels(y_labels))
            .render(area, &mut buffer);
        output.push_str(&format!("\nSeries {} · {label}\n", index + 1));
        for line in buffer.content.chunks(usize::from(options.width)) {
            for cell in line {
                output.push_str(cell.symbol());
            }
            output.push('\n');
        }
        output.push_str(&format!(
            "{} samples · {} gaps · {} clipped\n",
            samples.len(),
            gaps,
            clipped
        ));
        if output.len() > 64 * 1024 * 1024 {
            return Err("Chart text exceeds 64 MiB; use a more selective query".into());
        }
    }
    Ok(output)
}
