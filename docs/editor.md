# Editor support (`siox-lsp`)

siox ships a language server, `siox-lsp`. It speaks LSP over stdin/stdout, so
any editor with LSP support can use it.

## Setup

Build it and point your editor at the binary:

```bash
cargo build -p siox-lsp
target/debug/siox-lsp --stdio --std ./std
```

Configure your editor to launch that command for the `siox` language. The
`--std <dir>` flag tells it where the standard library lives (default `./std`).
Document changes are synchronised as full text; compiler snapshots are cached
and rebuilt when a document changes.

## What it provides

- Live lexer, parser, name-resolution, and type-checking diagnostics.
- Definition / type-definition navigation, references, highlights, and safe
  rename.
- Hover, contextual completion, signature help, and parameter hints.
- Semantic tokens, document/workspace symbols, folding and selection ranges.
- Quick fixes for suggested names and safely removable unused imports.
- Standard-library import links and navigation.
- Canonical whole-document formatting for comment-free source.

## Current limitations

- **Formatting returns no edit when comments are present** — the compiler's
  canonical printer does not yet retain comment trivia, so it declines rather
  than delete them.
- **Cross-file user-module analysis** follows the compiler's current
  single-entry-file limitation; standard-library modules are loaded
  transitively.
