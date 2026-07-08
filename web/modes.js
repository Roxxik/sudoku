"use strict";

// The game-mode registry: the single place that answers "what kind of game is this,
// and how does it title / label / bucket?" for every display surface.
//
// Before this, each surface (home, play, stats) re-sniffed a game's mode from its
// spec + ad-hoc tags and ran its own daily->review->custom->campaign if-else ladder,
// with the daily-before-review precedence restated in each. Now a game carries a
// persisted `kind` (store.js), `modeOf(g)` is a field read, and every surface
// dispatches through one descriptor per mode. Adding a mode is adding a descriptor
// here, not touching N surfaces.
//
// Each descriptor is a pure function of the game record:
//   title(g)       -- the short card / header title
//   line(g)        -- the full one-line identity (Continue meta + play header),
//                     without the time/hint suffix the caller appends
//   statsLabel(g)  -- the "mode" column in the Stats history list
//   statsBucket(g) -- { group, key } for the stats aggregator, or null (custom /
//                     imported are too free-form to bucket: headline totals only)

import CURRICULUM from "./curriculum.js";
import * as store from "./store.js";
import { techniqueName } from "./util.js";
import { reviewLessonByKey, reviewIdentity } from "./review.js";
import { levelName, dayLabel } from "./daily.js";
import { campaignUsages } from "./campaign.js";
import { masksFromUsages } from "./spec.js";

// kindIndex -> curriculum id, for the campaign technique name.
const idByKind = {};
for (const t of CURRICULUM) idByKind[t.kindIndex] = t.id;

function modeLabel(mode) {
  return mode === "drill" ? "Drill" : "Train";
}

// The fine-grained stats key: a plain Play and a Play-from-Forced are distinct
// accomplishments, so they never share a bucket (the campaign technique page and the
// Review buttons badge them separately). The Stats SCREEN merges the two at render.
function startKey(g) {
  return g.mode + (g.fromForced ? "Forced" : "");
}

function techName(g) {
  return techniqueName(idByKind[g.kindIndex] || "");
}

function reviewName(g) {
  const lesson = reviewLessonByKey(g.lesson);
  return lesson ? lesson.name : "Lesson";
}

const CAMPAIGN = {
  id: "campaign",
  title: (g) => techName(g),
  line: (g) => `${techName(g)} · ${modeLabel(g.mode)}`,
  statsLabel: (g) => modeLabel(g.mode),
  statsBucket: (g) => ({ group: `campaign:${g.kindIndex}`, key: startKey(g) }),
};

const REVIEW = {
  id: "review",
  title: (g) => `Review · ${reviewName(g)}`,
  line: (g) => `Review · ${reviewName(g)} · ${modeLabel(g.mode)}`,
  statsLabel: (g) => modeLabel(g.mode),
  statsBucket: (g) => ({ group: `review:${g.lesson}`, key: startKey(g) }),
};

const DAILY = {
  id: "daily",
  title: (g) => `Daily · ${levelName(g.daily.level)}`,
  line: (g) => `Daily · ${levelName(g.daily.level)} · ${dayLabel(g.daily.day)}`,
  statsLabel: (g) => dayLabel(g.daily.day),
  statsBucket: (g) => ({ group: "daily", key: String(g.daily.level) }),
};

const CUSTOM = {
  id: "custom",
  title: (g) => g.label || "Custom",
  line: (g) => (g.label ? `Custom · ${g.label}` : "Custom"),
  statsLabel: () => "Custom",
  statsBucket: () => null,
};

const IMPORTED = {
  id: "imported",
  title: (g) => g.label || "Imported",
  line: (g) => g.label || "Imported",
  statsLabel: () => "Imported",
  statsBucket: () => null,
};

const MODES = {
  campaign: CAMPAIGN,
  review: REVIEW,
  daily: DAILY,
  custom: CUSTOM,
  imported: IMPORTED,
};

// A curriculum-free best guess at a record's kind, for the rare game that reaches a
// display before the migration stamped `kind` (or a future kind we don't model). It
// can't tell Review from Custom without the curriculum -- but the boot migration
// recovers that and persists it, so this is only a last-ditch fallback.
function inferKind(g) {
  if (g && g.daily && typeof g.daily.day === "number") return "daily";
  if (g && typeof g.kindIndex === "number") return "campaign";
  if (g && Array.isArray(g.spec)) return "custom";
  return "imported";
}

// The descriptor for a game -- a field read on `kind`, with the inference fallback.
export function modeOf(g) {
  return (g && MODES[g.kind]) || MODES[inferKind(g)];
}

// Only a custom game exposes its spec (the "Open spec" affordance re-opens it in the
// builder to regenerate a similar puzzle). Imported has no spec; the rest aren't
// user-editable specs.
export function isCustom(g) {
  return modeOf(g) === CUSTOM;
}

// ---- One-time mode migration ----
// Stamp `kind` (and, for Review, the recovered lesson key + Train/Drill mode) onto
// records saved before those fields existed, and backfill campaign spec masks. Run
// once at boot (app.js) before the first render, so every surface reads `kind`
// directly. Idempotent: a record that already has `kind` is skipped, so on later
// boots it costs just one index pass.
export function migrateModes() {
  for (const g of store.allGames()) {
    if (g.kind) continue;
    store.updateGame(g.id, classifyLegacy(g));
  }
}

// Classify a legacy record the way the old per-surface sniffers did, with the same
// daily-before-review precedence (Expert I's daily spec is byte-identical to Review
// Lesson 1's). This is the ONLY remaining caller of the spec-matching reviewIdentity.
function classifyLegacy(g) {
  if (g.daily && typeof g.daily.day === "number") return { kind: "daily" };
  if (g.mode === "custom" && g.forceAny && Array.isArray(g.spec)) {
    const id = reviewIdentity(CURRICULUM, g.spec);
    if (id) return { kind: "review", lesson: id.key, mode: id.mode };
  }
  if (typeof g.kindIndex === "number") {
    // Older campaign records never stored specMasks (play.js fetched them from wasm);
    // backfill them from the JS builder so the hint tree reads them uniformly now.
    const usages = campaignUsages(CURRICULUM, g.kindIndex, g.mode);
    return { kind: "campaign", specMasks: masksFromUsages(usages) };
  }
  if (Array.isArray(g.spec)) return { kind: "custom" };
  return { kind: "imported" };
}

// ---- Stats aggregation ----
// Bucket solved games by their mode's statsBucket into a two-level map:
//   group -> key -> { count, sumMs, bestMs, lastMs, lastAt }
// `group` separates kinds/modes (campaign:<kindIndex>, review:<lessonKey>, daily);
// `key` splits within a group (train / trainForced / drill / drillForced, or a daily
// level index). Custom/imported have no bucket -- they're headline totals only. The
// split stays fine-grained (a plain Play and a Play-from-Forced apart) so the campaign
// technique page and the Review buttons can badge each start; the Stats SCREEN merges
// the forced pair at render (mergeBuckets). Pass solved games (store.solvedGames()).
export function statsBuckets(games) {
  const out = {};
  for (const g of games) {
    const b = modeOf(g).statsBucket(g);
    if (!b) continue;
    const group = (out[b.group] ||= {});
    const m = (group[b.key] ||= { count: 0, sumMs: 0, bestMs: Infinity, lastMs: 0, lastAt: 0 });
    const ms = g.elapsedMs || 0;
    m.count += 1;
    m.sumMs += ms;
    m.bestMs = Math.min(m.bestMs, ms);
    const at = g.solvedAt || 0;
    if (at >= m.lastAt) {
      m.lastAt = at;
      m.lastMs = ms;
    }
  }
  return out;
}

// The per-key bucket object for one campaign kind / review lesson / the daily group
// ({} when nothing is solved there). Callers index it by start key or level.
export function campaignStats(buckets, kindIndex) {
  return buckets[`campaign:${kindIndex}`] || {};
}
export function reviewStats(buckets, lessonKey) {
  return buckets[`review:${lessonKey}`] || {};
}
export function dailyStats(buckets) {
  return buckets.daily || {};
}

// Total solved count across every key of a group object (from the accessors above).
export function countOf(groupObj) {
  let n = 0;
  for (const k in groupObj) n += groupObj[k].count || 0;
  return n;
}

// The mean solve time of one bucket (0 when empty).
export function avgOf(bucket) {
  return bucket && bucket.count ? bucket.sumMs / bucket.count : 0;
}

// Combine several buckets into one (count/sum summed, best min-ed, last kept), or null
// when all are empty/absent. The Stats screen uses it to fold a start's plain Play and
// Play-from-Forced into a single row.
export function mergeBuckets(...buckets) {
  const out = { count: 0, sumMs: 0, bestMs: Infinity, lastMs: 0, lastAt: 0 };
  for (const b of buckets) {
    if (!b) continue;
    out.count += b.count;
    out.sumMs += b.sumMs;
    out.bestMs = Math.min(out.bestMs, b.bestMs);
    if (b.lastAt >= out.lastAt) {
      out.lastAt = b.lastAt;
      out.lastMs = b.lastMs;
    }
  }
  return out.count ? out : null;
}
