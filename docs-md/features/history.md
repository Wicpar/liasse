# History and extraction

## Commits

Commits form a DAG. The fold of admitted commits is the current model state.

## Integrity

History tracks both commit identity and state integrity. Encoding versions are explicit so canonicalization changes can be handled safely.

## `$history`

History is visible as model data through `$history` surfaces. Client-side historical navigation should use this surface rather than an author-level `at` expression.

## Extraction

Erasure is modeled as extraction: data is removed from the live model but can produce a bundle containing the information needed for policy-approved reinsertion.
