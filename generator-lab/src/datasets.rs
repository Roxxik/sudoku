//! Shared loader for the normalised external human-difficulty datasets
//! (`datasets/normalized/`, see `docs/grader-external-calibration.md`). Both the Stage-1 scoreboard
//! ([`examples/datasets_correlate`](../examples/datasets_correlate.rs)) and the Stage-3 calibration
//! workbench ([`examples/grade_diag`](../examples/grade_diag.rs)) read groups through here, so there
//! is one parser of the `puzzle,label_value,label_raw,weight` schema.

use std::fs;
use std::path::{Path, PathBuf};

/// One dataset row: the 81-char puzzle, its human label (numeric, higher = harder), and the per-row
/// confidence weight (player count for `synnwang/solve_time`, `1` elsewhere).
#[derive(Clone)]
pub struct Row {
    pub puzzle: String,
    pub label_value: f64,
    pub weight: f64,
}

/// One normalised group (`<group>.csv`): its id, whether the human label is continuous or an ordinal
/// level index, and the rows.
pub struct Group {
    pub id: String,
    pub ordinal: bool,
    pub rows: Vec<Row>,
}

/// Parse one `normalized/<group>.csv`. Group id = file stem with `__` -> `/`. The label kind is read
/// off the data: an ordinal group's `label_raw` is a level name (non-numeric), a continuous group's
/// is the numeric metric. Columns: `puzzle,label_value,label_raw,weight`.
pub fn load_group(path: &Path) -> Option<Group> {
    let text = fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_str()?;
    let id = stem.replacen("__", "/", 1);
    let mut lines = text.lines();
    lines.next()?; // header
    let mut rows = Vec::new();
    let mut ordinal = false;
    let mut first = true;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 4 {
            continue;
        }
        let puzzle = parts[0].trim().to_string();
        let label_value: f64 = parts[1].trim().parse().ok()?;
        let label_raw = parts[2].trim();
        let weight: f64 = parts[3].trim().parse().unwrap_or(1.0);
        if first {
            // Ordinal iff the raw label is a level *name*, not the numeric metric.
            ordinal = label_raw.parse::<f64>().is_err();
            first = false;
        }
        rows.push(Row { puzzle, label_value, weight });
    }
    Some(Group { id, ordinal, rows })
}

/// Load every `<group>.csv` under `dir` (skipping `catalog.json`), in a stable sorted order.
/// A file that fails to parse is silently skipped.
pub fn load_groups(dir: &Path) -> Vec<Group> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "csv").unwrap_or(false))
        .collect();
    files.sort();
    files.iter().filter_map(|p| load_group(p)).collect()
}
