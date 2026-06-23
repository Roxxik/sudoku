"use strict";

// Small shared DOM helpers used by more than one view (the Home continue cards
// and the Stats history list): a static board thumbnail and the title/meta text
// column that sits beside it.

// A small static preview of a game's current grid: givens plus the player's
// entries (no pencil marks). For a solved game `g.value` is the completed grid,
// so this renders the finished board. `size` is an optional size-modifier class
// ("mini-lg" hero, "mini-xl" on the Continue page).
export function miniBoard(g, size) {
  const wrap = document.createElement("div");
  wrap.className = size ? `mini ${size}` : "mini";
  for (let i = 0; i < 81; i++) {
    const cell = document.createElement("span");
    const given = g.puzzle[i] >= "1" && g.puzzle[i] <= "9" ? g.puzzle[i] : 0;
    const entered = (g.value && g.value[i]) || 0;
    if (given) {
      cell.textContent = given;
      cell.className = "g";
    } else if (entered) {
      cell.textContent = entered;
    }
    wrap.appendChild(cell);
  }
  return wrap;
}

// Copy `text` to the clipboard, resolving to whether it worked. Prefers the
// async Clipboard API and falls back to a hidden-textarea execCommand for
// insecure contexts (plain-http deploys) where navigator.clipboard is absent.
export async function copyText(text) {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Permission denied / not focused: fall through to the legacy path.
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

// The generator's difficulty sub-tier names, indexed by `grade.band` (0..4 -- the
// absolute single-puzzle band the worker cuts from its continuous `rating`; see
// web/store.js). These are a *within-technique* sub-rating along one heat axis: the
// card already names the technique, so the lowest band ("Mild") means the mild end
// of that technique, not that the puzzle is easy.
export const GRADE_NAMES = ["Mild", "Medium", "Hot", "Spicy", "Fiery"];

// The band's display word for a game's `grade`, or null if ungraded (old records
// predate grading, or the field is malformed).
export function gradeName(grade) {
  if (!grade || typeof grade.band !== "number") return null;
  return GRADE_NAMES[grade.band] || null;
}

// A small coloured difficulty pill for a game's grade (a green->red ramp by band),
// or null when the game is ungraded -- callers skip appending it then. The colour
// class is keyed on the band index (grade-b0..b4), independent of the name words.
export function gradeBadge(grade) {
  const name = gradeName(grade);
  if (!name) return null;
  const span = document.createElement("span");
  span.className = `grade-badge grade-b${grade.band}`;
  span.textContent = name;
  return span;
}

// The text half of a card: a bold title over one or more muted meta lines.
// Pass extra strings to stack additional muted lines (e.g. a debug seed).
export function textColumn(title, ...metaLines) {
  const col = document.createElement("div");
  col.className = "ci-text";
  const name = document.createElement("span");
  name.className = "ci-name";
  name.textContent = title;
  col.appendChild(name);
  for (const line of metaLines) {
    const m = document.createElement("span");
    m.className = "ci-meta";
    m.textContent = line;
    col.appendChild(m);
  }
  return col;
}
