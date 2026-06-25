//Run from home in the browser console
// Set back game by a day

(() => {
  const K = "sudoku.games.v1";
  const games = JSON.parse(localStorage.getItem(K) || "[]");
  const g = games.filter(x => x.daily && x.status === "active")
                 .sort((a,b) => (b.lastPlayedAt||b.createdAt||0) - (a.lastPlayedAt||a.createdAt||0))[0];
  if (!g) return console.log("no in-progress daily — start one first");
  g.daily.day -= 1;
  localStorage.setItem(K, JSON.stringify(games));
  console.log(`game ${g.id}: daily.day -> ${g.daily.day} (late). Resume from Home › Continue, then solve.`);
})();


// almost solve the last loaded game
(() => {
  const games = JSON.parse(localStorage.getItem("sudoku.games.v1") || "[]");
  const g = games.filter(x => x.status === "active")
                 .sort((a,b) => (b.lastPlayedAt||b.createdAt||0) - (a.lastPlayedAt||a.createdAt||0))[0];
  if (!g) return console.log("no active game");
  const key = "sudoku.game." + g.id;
  const heavy = JSON.parse(localStorage.getItem(key) || "{}");
  const value = new Array(81).fill(0);
  let left = -1;
  for (let i = 0; i < 81; i++) {
    if (g.puzzle[i] !== ".") continue;        // a given -> shown from the puzzle, leave 0
    if (left === -1) { left = i; continue; }   // leave ONE blank to finish by hand
    value[i] = Number(g.solution[i]);
  }
  heavy.value = value;
  heavy.centerMarks = Array.from({length:81}, () => []);
  heavy.cornerMarks = Array.from({length:81}, () => []);
  heavy.history = []; heavy.redo = [];
  localStorage.setItem(key, JSON.stringify(heavy));
  console.log(`prefilled ${g.id}. Reload (F5), Continue, place ${g.solution[left]} at R${(left/9|0)+1}C${left%9+1}.`);
})();


// Re-arm ALL daily games: solvable again, cheat cleared, and a FRESH solve_id so a
// re-submit lands as a NEW leaderboard row (the old id was idempotent -> no-op).
(() => {
  const K = "sudoku.games.v1";
  const games = JSON.parse(localStorage.getItem(K) || "[]");
  const newId = () => (crypto.randomUUID
    ? crypto.randomUUID()
    : Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 10));
  let n = 0;
  for (const g of games) {
    if (!g.daily) continue;
    g.status = "active";
    g.solvedAt = null;
    g.cheated = false;
    g.solve_id = newId();
    n++;
  }
  localStorage.setItem(K, JSON.stringify(games));
  console.log(`re-armed ${n} daily game(s) with fresh solve_ids.`);
})();