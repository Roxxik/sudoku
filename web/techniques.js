"use strict";

// Per-technique blurbs shown at the top of a campaign technique page.
//
// Keyed by the curriculum id (the kebab-case `lab::kinds::NAMES`, same key the
// rest of the app uses). Each entry is hand-written, not templated:
//   description : 1-3 sentences explaining what the technique sees and concludes.
//   aliases     : optional other names the technique goes by.
//   extra       : optional aside (a related pattern, a caveat) shown below.
// This is intentionally the short form -- it'll grow images and worked examples
// later, so the page reads it from here rather than baking text into home.js.

export const TECHNIQUE_INFO = {
  "hidden-single": {
    description:
      "Within a single row, column, or box, a digit has just one cell left where it can legally go — even if that cell still shows other pencil-marks. Place it there.",
    extra:
      "For now Beginner mixes hidden singles in boxes and in lines together; they'll become their own lessons once the generator can target them apart.",
  },
  "naked-single": {
    description:
      "A cell has only one candidate left: every other digit already shows up among its row, column, and box peers. That lone survivor is forced.",
    aliases: ["Sole candidate", "Forced cell"],
  },
  "lc-pointing": {
    description:
      "Inside one box, all the remaining spots for a digit line up on a single row or column. The digit must come from that line within the box, so it can be struck from the rest of that row or column outside the box.",
    aliases: ["Pointing pair / triple"],
  },
  "lc-claiming": {
    description:
      "All the remaining spots for a digit in one row or column fall inside a single box. The digit must land in that box, so it can be erased from the box's other cells.",
    aliases: ["Box-line reduction", "Claiming pair / triple"],
  },
  "naked-pair": {
    description:
      "Two cells in the same house carry exactly the same two candidates and nothing else. Those two digits are pinned to those two cells, so they drop out of every other cell in the house.",
  },
  "hidden-pair": {
    description:
      "Two digits can only be placed in the same two cells of a house. Those cells belong to the pair, so any other candidates sharing them can be erased.",
  },
  "naked-triple": {
    description:
      "Three cells in a house between them use only three candidates — each cell holding two or three of them. The three digits are confined to those cells and clear out of the rest of the house.",
  },
  "hidden-triple": {
    description:
      "Three digits are restricted to the same three cells of a house, even if those cells also list other candidates. The extras can go, since the trio must fill those cells.",
  },
  "naked-quad": {
    description:
      "Four cells in a house collectively contain only four candidates. They are locked to those cells, so the four digits disappear from everywhere else in the house.",
  },
  "hidden-quad": {
    description:
      "Four digits can appear only in the same four cells of a house. Those cells are reserved for the quad, so every other candidate in them can be removed.",
  },
  "x-wing": {
    description:
      "A digit's candidates in two rows are confined to the very same two columns (or two columns to the same two rows), forming a rectangle. The digit must take a diagonal pair of corners, so it is eliminated from those two columns everywhere else.",
    extra:
      "The smallest fish — a size-2 pattern. Swordfish and Jellyfish are the same idea stretched over three and four lines.",
  },
  swordfish: {
    description:
      "The three-line cousin of the X-Wing: across three rows a digit is held within the same three columns (or vice-versa), letting it be struck from those columns outside the pattern.",
    extra: "A size-3 fish. Each line needs only two of the three columns — it does not have to fill all three.",
  },
  jellyfish: {
    description:
      "A four-line fish: a digit spread over four rows stays within four columns (or vice-versa), so it can be removed from those columns elsewhere.",
    extra:
      "A size-4 fish — the largest worth hunting, since a size-5 is just the same pattern seen from the other direction.",
  },
  "xy-wing": {
    description:
      "Three bivalue cells form a hinge holding candidates X and Y and two pincers holding X-Z and Y-Z. Whichever digit the hinge takes, one pincer is forced to Z — so Z can be eliminated from any cell that sees both pincers.",
    aliases: ["Y-Wing"],
  },
  "xyz-wing": {
    description:
      "Like the XY-Wing, but the hinge itself also carries Z (its candidates are X, Y and Z). Any of the three cells could be Z, so Z is removed only from cells that see all three at once.",
  },
  "w-wing": {
    description:
      "Two bivalue cells holding the same pair X-Y are joined by a strong link on one of the digits running through a third house. Whichever way that link falls, one of the two cells must be the other digit — so it can be eliminated from any cell seeing both.",
  },
};
