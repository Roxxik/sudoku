// SpiderMonkey (Firefox engine) benchmark runner for the solver-lab wasm
// module. Mirrors web/bench.mjs (V8/node) but uses the SpiderMonkey js-shell
// API: read(path,"binary"), synchronous WebAssembly.{Module,Instance},
// performance.now(), print(), scriptArgs.
//
// Variants are discovered from the module at runtime (variant_count /
// variant_name), so registering a prover in Rust needs no edit here.
//
// Usage (run from repo root so the relative wasm path resolves):
//   js140 solver-lab/web/bench-sm.js [attempts=4000] [seed=1]

var attempts = Number(scriptArgs[0] || 4000);
var seed = Number(scriptArgs[1] || 1);

var bytes = read(
  "target/wasm32-unknown-unknown/release/solver_lab.wasm",
  "binary",
);
var instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), {});
var ex = instance.exports;

// Discover variant names straight from the module's data segment. The
// SpiderMonkey js-shell has no TextDecoder; names are ASCII, so decode by hand.
function variantName(i) {
  var ptr = ex.variant_name_ptr(i);
  var len = ex.variant_name_len(i);
  var bytes = new Uint8Array(ex.memory.buffer, ptr, len);
  var s = "";
  for (var j = 0; j < len; j++) s += String.fromCharCode(bytes[j]);
  return s;
}
var count = ex.variant_count();

print(
  "solver-lab wasm bench (SpiderMonkey): " +
    attempts +
    " attempts/variant, seed " +
    seed,
);
print(count + " variants registered\n");
print(
  pad("variant", 14) + padl("ms", 9) + padl("us/attempt", 12) + padl("vs base", 9),
);

var baseMs = null;
var baseFp = null;
for (var idx = 0; idx < count; idx++) {
  var name = variantName(idx);
  // Warm up the JIT, then measure.
  ex.bench(idx, Math.min(attempts, 500), seed);
  var t0 = performance.now();
  var fp = ex.bench(idx, attempts, seed);
  var ms = performance.now() - t0;

  if (baseFp === null) baseFp = fp;
  else if (fp !== baseFp) {
    print("FINGERPRINT MISMATCH on " + name + ": " + fp + " != " + baseFp);
    quit(1);
  }

  var speedup = baseMs === null ? 1 : baseMs / ms;
  if (baseMs === null) baseMs = ms;
  var us = (ms * 1000) / attempts;
  print(
    pad(name, 14) +
      padl(ms.toFixed(1), 9) +
      padl(us.toFixed(1), 12) +
      padl(speedup.toFixed(2) + "x", 9),
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
