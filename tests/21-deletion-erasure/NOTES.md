# §21 Deletion and erasure — chapter notes

Custom step keys used by this chapter (FORMAT.md §Steps allows new descriptive
keys documented here). Anything not listed here uses the standard vocabulary.

## Modeling erase / reinsert as surface calls

`erase(row)` and `reinsert(extract)` (§21.2, §21.3) are ordinary mutation
statements. Each case that exercises them exposes a package surface mutation
whose body is exactly `erase(.coll[@id])` or `reinsert(@extract)`, and drives it
with the standard `call` step. This is the only mechanism the spec gives for
reaching an "explicitly exposed erasure call" (§21.2). The value returned by an
erase call is the durable **extract** (§21.2 step 6, "returns the extract"); the
spec pins neither its wire shape nor the type of the `reinsert` parameter, so
cases bind the whole erase return with `$bind:` and match `$any`, and pass it
back verbatim with `$ref:`. No expectation in this chapter depends on the
internal structure of an extract.

## Custom step keys

| key             | semantics |
|-----------------|-----------|
| `tamper_extract`| Produce a corrupted copy of a bound extract. Shape: `{ from: "<bind>", as: "<newbind>", op: "<mutation>" }`. `op: "flip_content"` mutates the extract's content bytes so its content hash no longer verifies (§21.3 "extract content hash"). The tampered handle is then passed to a `reinsert` call. Analogous to §19's `tamper_artifact`; kept chapter-local because §21 extracts are a distinct object from §19 artifacts. |
| `in_sandbox` / `restore` | Reused from §19's corpus: `in_sandbox: "<label>", steps: [...]` runs the nested steps against a fresh empty runtime, and `restore: { from: "<artifact>" }` loads an exported artifact into it. Used here to prove an erased row does not reappear in live state after an export→restore round-trip. |
| `scrub_scope_of_cascaded_row` | A marker assertion (no runtime effect assumed) that pins the *observation* being classified as unspecified: whether a cascade-deleted row's retained history payload is scrubbed by an enclosing `erase`. Shape: `{ collection: "<name>", key: "<key>" }` with `expect: { outcome: unspecified }`. Exists only to make the spec gap in `red/erase-cascade-scrub-scope-unspecified.hjson` executable/recordable rather than silent. |

`expect_one_of` (a FORMAT matcher shown for `concurrently` branches) is also used
once on a plain `expect_view` assertion
(`red/reinsert-historical-does-not-recreate-live-row.hjson`) to encode a genuine
§21.3 disjunction: both the empty (historical-only) and the restored (live) result
are spec-conformant.

## Uncovered / deferred

- **Cross-module inbound-ref immediacy (§13.12, §5.6 line 543).** The rule that a
  ref crossing a module boundary MUST declare `$on_delete` immediately is not
  covered: the spec gives no concrete syntax for a ref whose declared target is
  a row owned by another module instance (module coupling in §13 is via typed
  `$expose`/`$use` interface views and calls, not `$ref` targets). Authoring a
  case would require guessing the boundary-crossing `$ref` form, which violates
  the "externally deducible only" rule. Recorded here rather than guessed.

- **Byte-level scrub of retained history (§21.2 step 3/5).** The corpus can
  observe that an erased row leaves live state and stays absent across
  export/restore, and that only `reinsert` with a matching stub restores bytes.
  It cannot directly inspect that a retained *history* leaf now holds a digest
  stub instead of the payload, because the step vocabulary exposes no
  history-payload read. The rendering of a stubbed leaf under a rolled-back live
  view is genuinely unspecified (see
  `red/erase-cascade-scrub-scope-unspecified.hjson` for the related scope gap).
