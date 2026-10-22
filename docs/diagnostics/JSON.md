# Structured diagnostics

`sioxc --error-format=json --emit ir design.siox` writes one UTF-8 JSON object
per LF-terminated line to stderr. Default `--error-format=human` is unchanged.
The library exposes the same encoding through `Diagnostic::render_json` and
`Compilation::render_diagnostics_json`; callers can still consume the Rust
diagnostic structures directly without serializing them.

JSON mode suppresses progress, verbose frontend traces, artifact notices and
textual success/error summaries. Input, root-selection and backend failures
are error diagnostics with no code or source span. Exit status still indicates
compilation success; an empty stream is valid. Objects and executables are
written to their normal paths, and textual artifacts go to stdout. Token dumps
also go to stdout in this mode, reserving stderr for diagnostics. `--explain`
continues to print Markdown on stdout; an unknown code follows the selected
error format. Clap's command-line usage errors/help/version remain text, outside
the compilation diagnostic protocol.

The transport, byte/scalar coordinate conventions and applicability names
follow [rustc's documented JSON output](https://doc.rust-lang.org/rustc/json.html).
Field names follow Siox's existing `Diagnostic` API, not rustc's wire schema.
Consumers should ignore unknown fields and handle future code/severity values.

## Record

| Field | Value |
| --- | --- |
| `$message_type` | `"diagnostic"` |
| `code` | Stable code string, or `null` for codeless messages |
| `severity` | `"error"`, `"warning"`, `"note"` or `"help"` |
| `message` | The complete headline, with JSON string escaping |
| `primary` | A span object, or `null` |
| `labels` | Objects with `span` and `message` |
| `notes` | Supporting strings, in emission order |
| `help` | Human-readable advice, or `null` |
| `suggestions` | Atomic edit groups described below; may be empty |

A span includes `file_id`, `file_name`, `byte_start`, `byte_end`, `line_start`,
`column_start`, `line_end` and `column_end`. File IDs are compilation-local.
Byte ranges are zero-based and half-open; lines/columns are one-based, and
columns count Unicode scalar values, **not** UTF-8 bytes or LSP UTF-16 units.
Use byte offsets to edit the original UTF-8 source. Tabs count as one scalar;
these are source positions, not terminal display widths.

An unknown file has `file_name: null`. A missing file or invalid span has all
four line/column fields set to `null`; its original file ID/byte offsets are
retained for inspection, never clamped to a fabricated valid location. Source
names may refer to embedded or virtual files rather than writable disk paths.

## Suggestions

`Diagnostic::suggest(span, replacement, Applicability)` records one edit.
`suggest_edits` groups several replacements that must be applied **together**.
For example, moving an attribute inside an implementation needs both deletion
of the old prefix and insertion of its binding inside the body:

```json
{
  "applicability": "MachineApplicable",
  "edits": [
    {"span": {"file_id": 0, "file_name": "example.siox", "byte_start": 10, "byte_end": 28,
              "line_start": 2, "column_start": 1, "line_end": 2, "column_end": 19},
     "replacement": ""},
    {"span": {"file_id": 0, "file_name": "example.siox", "byte_start": 40, "byte_end": 40,
              "line_start": 3, "column_start": 12, "line_end": 3, "column_end": 12},
     "replacement": "\nattr precedence = 40;\n"}
  ]
}
```

All edits reference the **unchanged original source**, not the output of an
earlier edit. Validate that source and all spans before applying any group.
Reject overlapping replacements; apply from the highest byte offset downward.
Preserve emission order for insertions sharing an offset. An empty replacement
deletes; an empty span inserts. This compiler reports fixes and does not write
source files or implement `--fix`.

`MachineApplicable` preserves the intended meaning without placeholders;
`MaybeIncorrect` requires review; `HasPlaceholders` requires user input;
`Unspecified` gives no confidence claim. Never automatically apply the latter
three. Current producers migrate `process name {}` to `name: process {}` and
metadata directives to bindings on entities, implementations and signals.
Process comments and attribute value spelling are retained. Comment-bearing
metadata wrappers and parser-recovered input are conservatively marked for
review instead of automatic application. Existing prose/help stays unchanged.
Reparsed macro-token input receives no source edits until expansion identity
can prove which original file/range is safe to change.

## Verification

`bash scripts/test-json.sh /path/to/siox-tests` builds the selected compiler
and checks every annotated UI case against this schema and the source bytes.
It round-trips JSON through Python's standard parser, requires coverage of all
active diagnostic codes and applies/recompiles dedicated migration fixtures.
It also checks unspanned failures, string/path escaping and output-channel
separation. Like the UI runner it supports `SIOXC_FEATURES=bitpack` and an
explicit `--compiler /path/to/sioxc`. CI/local CI run both modes; the shared
host uses the guarded launcher required by HOUSERULES.md. Library tests cover
escaping, Unicode, invalid spans and edit grouping without CLI/LLVM features.
