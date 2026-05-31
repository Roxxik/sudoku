// SpiderMonkey (Firefox engine) benchmark runner for the generator-lab wasm
// module. Mirrors web/bench.mjs (V8/node) but uses the SpiderMonkey js-shell
// API: read(path,"binary"), synchronous WebAssembly.{Module,Instance},
// performance.now(), print(), scriptArgs.
//
// Usage (run from repo root so the relative wasm path resolves):
//   js140 generator-lab/web/bench-sm.js [attempts=1500] [seed=1] [mode=both|train|drill]

var attempts = Number(scriptArgs[0] || 1500);
var seed = Number(scriptArgs[1] || 1);
var which = scriptArgs[2] || "both";

var bytes = read(
  "target/wasm32-unknown-unknown/release/generator_lab.wasm",
  "binary",
);
var instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), {});
var ex = instance.exports;

var MODES = [
  ["train", 0],
  ["drill", 1],
].filter(function (m) {
  return which === "both" || which === m[0];
});

print("generator-lab wasm bench (SpiderMonkey): " + attempts + " attempts, seed " + seed + "\n");
print(pad("mode", 7) + padl("ms", 9) + padl("us/attempt", 12) + padl("puzzles", 9) + padl("atts/puzzle", 13));

for (var i = 0; i < MODES.length; i++) {
  var name = MODES[i][0];
  var mode = MODES[i][1];
  // Warm up the JIT, then measure.
  ex.bench(mode, Math.min(attempts, 400), seed);
  var t0 = performance.now();
  ex.bench(mode, attempts, seed);
  var ms = performance.now() - t0;
  var puzzles = ex.bench_yield(mode, attempts, seed) >>> 0;

  var us = (ms * 1000) / attempts;
  var attsPer = puzzles > 0 ? (attempts / puzzles).toFixed(0) : "inf";
  print(
    pad(name, 7) +
      padl(ms.toFixed(1), 9) +
      padl(us.toFixed(1), 12) +
      padl(String(puzzles), 9) +
      padl(attsPer, 13),
  );
}

function pad(s, n) {
  s = String(s);
  while (s.length < n) s += " ";
  return s;
}
function padl(s, n) {
  s = String(s);
  while (s.length < n) s = " " + s;
  return s;
}
