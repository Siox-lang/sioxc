# Differential test matrix for the runtime migration

Status: **documentation only**. No test is added or changed by this file. It
names, for each service in [the native process runtime](native-process-runtime.md),
the corpus cases that already exercise it and the exact observable to compare
between the generated-C harness and direct LLVM lowering.

Step 7 of the [unified process pipeline](testbench-software-ir.md) requires
"identical test results, diagnostics, time progression, resolved values, and
VCD/FST samples across the full default and bit-packed corpus". Running both
backends over 176 test cases proves *aggregate* parity; this matrix is what
identifies which service broke when a case fails.

## How to compare

For every case, both backends build a test executable and it is **run**, never
inspected. The comparison is over the executable's own output, because that is
the only thing both backends are contracted to agree on:

- **stdout, byte for byte.** `print!` output and the harness's per-test result
  lines. Ordering is part of the contract, not an accident.
- **exit status.** Pass/fail accounting.
- **failure text.** Message, stable code, and the rendered source snippet with
  its caret column — hardware and testbench failures are required to share
  these, so a divergence in either is a real defect.
- **waveforms**, where the case is run with `-o trace.vcd` / `.fst`: value
  changes and their timestamps, decoded rather than diffed as bytes.

Byte-identical stdout is achievable and has precedent: the NVC stress build's
5,397-byte output is already compared exactly across compiler changes.

## Matrix

| service | corpus cases | observable to compare |
| ------- | ------------ | --------------------- |
| scheduler, delta cycles | 143 cases using `await` | stdout order across suspensions; total simulated time reached |
| time wheel, clocks | 108 using `clk.rising()`, 22 using `after`/`'event` | edge count and the timestamp of each print |
| assertions and failure text | 176 (`assert!`) | message, code, and caret column of the first failure |
| formatting | 20 using `print!` | byte-identical stdout, including width and sign of arbitrary-width decimals |
| strings and UTF-8 | 14 | decoded characters, including multi-byte |
| file services | `fs_test`, `protocol_view_traits_test` | file contents read back, and the `sx_io_fail` message on a missing path |
| deterministic random | `rand_test` | the exact sequence — a differing seed or stride is a divergence, not noise |
| foreign calls | `ffi_test`, `ffi_real_test`, `real_local_test`, `attr_test` | returned values, and `real` bit patterns rather than printed rounding |
| dynamic arrays, checked index | `runtime_vector_index_test`, `nested_array_local_test` | the bounds-failure report: offending value, declared range, direction |
| metavalues and resolution | 52 cases mentioning `Logic` | per-element nine-value output; the `res.siox` sweep against `nvc` remains the external oracle |
| wide values | `wide_const_test`, `string_vector_test` | values above one ABI word, low-word-first |
| waveforms | `tests/build_binary.rs` decodes FST with upstream libfst | signal set, value changes, timestamps |
| descriptors and filtering | any multi-test file | which tests run under a name filter, and the result tally |

## Gaps worth closing before step 7

The matrix is only as good as its thinnest row. Three services are covered by
one or two cases each, which is too little to trust a backend swap:

- **deterministic random** — one case (`rand_test`). A backend could differ in
  seed, stride, or range mapping and still pass it.
- **file services** — two cases, neither of which exercises a failure path; the
  `sx_io_fail` message is untested end to end.
- **foreign calls** — four cases, all scalar. No aggregate or wide operand.

These belong in `Siox-lang/siox-tests` rather than here, and should be added
before the second backend exists, so both are held to the same cases from the
start rather than the new one being validated against whatever the old one
happens to do.

## What this matrix does not cover

- Compile-time behaviour. Diagnostics emitted by the frontend are backend
  independent and already covered by the Rust integration tests.
- Performance. Throughput differences are expected and are not a divergence.
- The 7 corpus files without `#[test]` — they compile to objects and have no
  runtime observable.
