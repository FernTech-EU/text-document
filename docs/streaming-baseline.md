# Streaming buffer — measured baseline

Captured **before** any change, on `feat/streaming-doc` off `main` (`b2245bf`), via
`cargo bench --bench streaming`. Re-run and compare after each change.

Fixture: a 65-char log line per block. A streaming log/console view does three things in a
loop — append a line at the end, ask whether it is over its scrollback cap, and evict the
oldest lines — so each is measured against document size.

## What a streaming view pays per line

| N | `append_line` (per line) | `append`, held cursor (per line) | `block_count` (per call) | `truncate_front` (20 lines) |
|---:|---:|---:|---:|---:|
| 1 000 | 1.161 ms | 289.9 µs | 252.6 µs | 2.85 ms |
| 10 000 | **15.87 ms** | 5.09 ms | **3.18 ms** | **55.4 ms** |
| 100 000 | *(did not complete — see below)* | | | |

Scaling the document 10× scales every column 13–19×: **everything here is O(N)**.

- `append_line` = `cursor_at(character_count())` + `insert_block` + `insert_text` — the path any
  consumer would naturally write. **15.9 ms per line at 10 k**, i.e. a ceiling of ~63 lines/second,
  on a document of only ten thousand lines.
- `append` with the cursor built once, outside the timing, costs 5.09 ms — so roughly **10.8 ms of
  the 15.9 ms is merely *locating the end***. `character_count()` and `cursor_at()` each run
  `get_document_stats`, which materializes every block's text to compute a word count
  (`get_document_stats_uc.rs:72`) that neither caller wants.
- The remaining 5.09 ms is `insert_block` itself: it calls `collect_block_ids_recursive` +
  `get_block_multi` (`insert_block_uc.rs:83`), fetching **every block entity in the document**
  before it can find the insertion point. `insert_text` has an O(log n) rope fast path
  (`insert_text_uc.rs:288`); `insert_block` never received one.
- `block_count()` is documented *"O(1) — reads cached value"* but measures **3.18 ms at 10 k**,
  via the same `get_document_stats` word-count walk. A viewer checking its scrollback cap once
  per line pays this every line.
- `truncate_front` (evicting 20 lines) costs **55 ms at 10 k**: `delete_text_uc.rs:261` does a
  whole-tree `collect_block_ids_recursive` + `get_block_multi`, then an ungated walk of the
  entire `child_order` refreshing every block's stored position (`delete_text_uc.rs:271`), with a
  rope lookup per block.

**100 000 did not complete.** The run was abandoned after ~25 minutes. That is not an aside —
it *is* the result: at 100 k the per-line cost extrapolates to ~160–200 ms, so merely
*measuring* 3 repetitions × 20 appends, plus the document rebuilds, exceeds any reasonable
budget. A `tail -f` at that size is not slow, it is unusable.

## Perspective

`text-typeset` appends a line in **10.7 µs**, flat at any N (`../text-typeset/docs/streaming-baseline.md`).
At 10 k lines `text-document` needs **15.9 ms** for the same line — roughly **1 500× more**, and
growing. After the layout work, the document layer is the entire bottleneck.

## Why the plan changed here

The intended route — declare `append_block` / `truncate_front` as `undoable: false` use cases in
`qleany.yaml` and generate their shells — is **not available**. A regeneration in this repo would
rewrite **195 files** and create 2 more, including:

```
database.rs:  - pub use self::rope_store::RopeStore as Store;   (reverts the whole storage engine)
              - pub mod rope_store;  + pub mod hashmap_store;
entities.rs:  - pub byte_range: (u32, u32)   (deletes a field rope_helpers depends on)
              + pub text_length: i64  + pub plain_text: String   (resurrects dead fields)
```

The manifest and the code have diverged past the point where codegen is safe: qleany is
effectively decommissioned here, and the manifest is a historical artifact rather than a source of
truth. So the streaming path is **hand-written in `public_api`**, which is not qleany-managed at
all, and touches no generated file.

That turns out to be well-supported rather than a workaround:

- `public_api/src/inner.rs:368` already reaches past the generated layer to call
  `rope_helpers::rope_append_empty_block` directly — the precedent exists.
- The O(log n) primitives already exist: `rope_append_block` (rope insert at the tail +
  an O(1) amortized `push_block`; nothing shifts, because appending at the end shifts nothing),
  `rope_insert_block_boundary`, and `rope_remove_block`.
- An O(1) block count already exists in-tree: `inner.rs:227`'s `check_block_count_changed` reads
  `get_document(...).block_count` straight off the entity, with no stats walk.

## Constraints found while designing the fast path

- **`None` does not mean "no undo".** `add_command_to_stack(cmd, None)` resolves
  `stack_id.unwrap_or(0)` and pushes to stack 0 (erroring if it does not exist) — it does not
  skip. The codebase's actual no-undo idiom is *perform, then `clear_stack`* (`set_plain_text`,
  `document.rs:166`). Per-line that would either grow the undo stack without bound or destroy the
  user's real history, so streaming ops need a dedicated throwaway stack, cleared per batch: one
  allocation per line, negligible against 15.9 ms.
- **`Frame.child_order` is a `Vec<i64>`**, so appending clones it — an O(N) memcpy (~50–80 µs at
  100 k). Small against 15.9 ms, but genuinely not O(1). Removing it would mean changing a core
  entity that the whole engine and its proptests depend on; out of scope by decision.
