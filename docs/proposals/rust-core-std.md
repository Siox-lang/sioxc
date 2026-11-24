# What siox can take from Rust's `core` and `std`

Status: **survey** (2026-11-24). [`alloc.md`](alloc.md) covered Rust's `alloc`
crate and is largely implemented; this covers the rest of `core` and `std`,
module by module, against what siox has. Nothing here is decided: each item
says what it would give siox, and the decisions are listed at the end.

## What shapes the answer

siox has two worlds Rust does not separate:

- **Hardware** (entities, ports, signals): every value has a fixed width, every
  loop is bounded at elaboration, nothing allocates.
- **Testbenches** (`#[test]` entities and processes): run in the simulator, may
  allocate (`std::alloc`), read files, print, and wait on simulation time.

And it lacks what much of Rust's library exists to manage: pointers,
references, lifetimes, threads, `unsafe`, destructors. siox values are copied;
concurrency is processes and signals. Rulings that constrain this survey:

- No `Iterator` trait (2026-07-09): loops are `for x in xs` over ranges,
  arrays and stores, bounded where hardware needs it.
- Payload enums are on hold (2026-10-30), so `Option<T>`/`Result<T, E>` do not
  exist yet.
- `BTreeMap` over `HashMap` (2026-11-23): ordered, so runs reproduce.

## Rust `core`, module by module

| Rust | siox today | Verdict |
| ---- | ---------- | ------- |
| `ops` (`Add` … `Not`, `Index`, `Range`) | `core::ops`, plus `CustomOperator`, `Prefix`/`Suffix`, `Resolve`, `LogicEncoding` | **have** |
| `cmp` (`PartialEq`/`Eq`, `Ord`, `Ordering`, `min`/`max`) | `core::cmp` (`Eq`, `Ord`, `Ordering`, `Match`); `min`/`max`/`clamp` in `std::math` | **have** |
| `convert` (`From`/`Into`, `TryFrom`/`TryInto`) | `From`, `Into` | `TryFrom` **needed** (blocked on `Result`) |
| `default` | `core::default::Default` | **have** |
| `fmt` (`Display`, `Debug`, `Formatter`, `Write`) | `Display`, `Formatter`, `{:?}`, format specs | `fmt::Write` for `String` **beneficial** |
| `option`, `result` | — | **needed** (payload enums) |
| `num` (integer methods, `Wrapping`, `Saturating`, `NonZero`, parse) | `std::bits`: `count_ones`, `leading_zeros`, `rotate_*`, `pow`, `abs_diff`, `saturating_add/sub`, … | `checked_*`/`overflowing_*` and `from_str_radix` need `Option`/`Result`; the rest **have** |
| `f64` (methods, consts) | `std::math`: `sqrt`, `sin`, `cos`, `exp`, `log`, `pow`, `floor`, `ceil`, `round`, `PI`, `E` | `tan`, `atan2`, `ln`/`log10`/`log2`, `hypot`, `trunc`, `fract`, `signum`, `to_degrees` **beneficial** (DSP references, filter coefficients) |
| `char` (`is_digit`, `to_digit`, `is_alphabetic`, case) | `std::text` tables (`Unicode`, `Ascii`, `unicode`, `char_of`) | `Char` methods **beneficial** for parsing stimulus |
| `str` (`split`, `trim`, `find`, `starts_with`, `parse`, `lines`) | — | on `String`: **beneficial**; `parse` needs `Result` |
| `array`, `slice` (`contains`, `reverse`, `fill`, `swap`, `sort`, `binary_search`, `concat`, `windows`) | indexing, slices `xs[a..b]`, `'length`, `for` | eager methods on fixed arrays and `Vec` **beneficial**; `reverse`/`fill`/`contains` also make sense in hardware |
| `iter` (`Iterator`, adapters) | `for x in xs` | **not taken** (ruling); the useful results (`sum`, `min`, `max`, `count`, `position`, `any`, `all`) as eager array methods instead |
| `mem` (`swap`, `replace`, `take`, `size_of`) | `swap`, `replace`; widths are `'length` | `take` **nice** (`replace` with `Default`) |
| `hash` | — | **not taken** (no `HashMap`) |
| `time::Duration` | `std::sim::time` (with `NS`, `US`, …) | **have**: siox's `time` is the duration |
| `macros` (`assert_eq!`, `assert_ne!`, `matches!`, `unreachable!`, `todo!`, `debug_assert!`, `dbg!`, `line!`/`file!`, `stringify!`, `concat!`, `include_bytes!`) | `assert!`, `warn!`, `print!`, `format!`, `write!`, `error!` | `assert_eq!`/`assert_ne!` **needed**, `matches!`, `unreachable!`, `todo!` **beneficial**; `line!`/`file!`/`stringify!` **nice**; `include_bytes!` is `read<T>` in hardware already |
| `ffi` (`c_int` …, `CStr`) | `std::ffi` C types | **have**; `CStr` nice-to-have |
| `ascii` | `std::text::Ascii` | **have** |
| `hint`, `intrinsics` | `core::intrinsics` (real math); `attr keep` | **have** what applies |
| `clone`, `marker` (`Copy`, `Send`, `Sync`, `Sized`, `PhantomData`) | values are copied | **not taken** |
| `ptr`, `cell`, `pin`, `any`, `borrow`, `future`, `task`, `async_iter`, `sync::atomic`, `simd`, `arch`, `panic` (unwinding) | — | **not taken**: no pointers, references, async, threads, SIMD or unwinding |

## Rust `std` beyond `core` and `alloc`

| Rust | siox today | Verdict |
| ---- | ---------- | ------- |
| `collections` | `std::alloc::collections` (all but `HashMap`/`HashSet`) | **have**; hash maps **not taken** |
| `fs` (`read`, `read_to_string`, `write`, `exists`, `create_dir`) | `read<T>`, `exists` | `write`/`append` **needed** (results, logs, waveform-adjacent dumps; SV `$fwrite`, VHDL `textio`) |
| `io` (`stdin`, `stderr`, `Write`) | `print!` | `eprint!` **nice**; `stdin` **not taken** (tests are non-interactive) |
| `env` (`args`, `var`) | the test binary owns its options (`-o`, filters) | test arguments **beneficial**: SV `$value$plusargs` (seed, iterations, a fixture path) |
| `process` (`exit`, `abort`) | `stop()`, `finish()` | **have** |
| `time` (`Instant`, `SystemTime`) | — | simulation time `now()` **needed** (VHDL `now`, SV `$time`); wall-clock **nice** |
| `sync::mpsc`, `Mutex`, `Condvar`, `Barrier` | processes, signals, `await` | a channel between testbench processes (SV `mailbox`) **beneficial**, with [`multiprocess.md`](multiprocess.md); locks **not taken** |
| `thread` | processes | **not taken** |
| `path` | text paths | `Path` joins **nice** |
| `panic` (`catch_unwind`), test attributes (`#[ignore]`, `#[should_panic]`) | `#[test]`, filtering | `#[should_fail]` (a checker must fail on bad input) and `#[ignore]` **beneficial** |
| `net`, `os`, `backtrace`, `alloc::System` | — | **not taken** (`backtrace`: failures already name their source line) |
| `hash::RandomState`, `random` | `std::rand` (seeded, reproducible) | **have**; random state **not taken** (reproducibility) |

## Proposed, in order

### Needed

1. **`now()`**: the current simulation time as a `time`, in `std::sim`. The
   runtime already tracks it; nothing reads it from siox. Every testbench
   language has it.
2. **`assert_eq!(a, b, ..)` / `assert_ne!`**: on failure they print both
   values (`left: 3, right: 4`), which `assert!(a == b)` cannot. Core macros
   over `assert!` and `{:?}`.
3. **`fs::write(path, text)` / `fs::append(path, text)`**: a testbench writes
   results, logs and expected-value dumps; today only `print!` leaves the
   test. Runtime-provided, as `read` is.
4. **`Option<T>` and `Result<T, E>`**: the one item behind most of the rest
   (`pop`, `get`, `peek`, `checked_add`, `parse`, `TryFrom`). They need payload
   enums, which are on hold; a narrow first step is `Option`/`Result` only.
   This is a decision for the owner.

### Beneficial

5. **Eager array methods** (instead of iterators): `contains`, `position`,
   `reverse`, `fill`, `swap`, `sum`, `min`, `max`, `count`, `any`, `all`,
   `sort` (testbench), written in std over `for`. `reverse` and `fill` unroll
   in hardware.
6. **`String` and `Char` text methods**: `trim`, `split`, `find`,
   `starts_with`, `ends_with`, `lines`, `to_uppercase`; `Char::is_digit`,
   `to_digit`, `is_alphabetic`, `is_whitespace`. For reading stimulus files and
   checking log text.
7. **More `real` math**: `tan`, `atan`, `atan2`, `ln`/`log10`/`log2`, `hypot`,
   `trunc`, `fract`, `signum`, `to_degrees`/`to_radians`; LLVM intrinsics or
   std over them.
8. **`matches!`, `unreachable!`, `todo!`**: core macros over `match` and
   `assert!(false, ..)`.
9. **Test arguments**: `std::env::arg("seed")` (or SV-style plusargs) read from
   the test binary's command line.
10. **`#[should_fail]` and `#[ignore]`** on tests.
11. **A testbench channel** (SV `mailbox`, Rust `mpsc`): `send`, `await recv`
    between processes. Belongs with the multiprocess proposal.
12. **`fmt::Write` for `String`**: `write!(s, "..", args)` appends.

### Nice to have

13. `mem::take`; `line!`/`file!`/`stringify!`; `eprint!`; wall-clock time;
    `Path` joins; `CStr` for C calls.

### Not taken

Pointers and references (`ptr`, `cell`, `pin`, `borrow`), ownership markers
(`Copy`, `Clone`, `Send`, `Sync`, `Sized`), concurrency (`thread`, locks,
atomics), async (`future`, `task`), `Iterator` (ruling), hashing, `simd`,
`arch`, `net`, `os`, unwinding. Rust needs them to manage memory and threads;
siox copies values and runs processes.

## Decisions

1. Unblock payload enums for `Option`/`Result` only, or keep them on hold?
2. `assert_eq!`/`now()`/`fs::write` first, as proposed?
3. Eager array methods in std (`xs.sum()`), keeping the no-`Iterator` ruling?
4. A testbench channel now, or with the multiprocess proposal?
