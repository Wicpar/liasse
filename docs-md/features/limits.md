# Limits and sources

Limits model metered capacity, consumption, and allocation.

## Meters

A meter has consumption declarations and sources. Zero capacity is the default unless sources provide capacity.

## Drain order

When several sources can fund a consumption, order is explicit. Eligibility decides which sources can be used for a specific operation.

## Concurrency

Meter spending joins the same proposal/commit discipline as ordinary mutations. Concurrent spending against the same basis must be admitted consistently or rejected.

## Module boundaries

A module can spend against a host meter or provide host capacity only through declared contracts. Nothing crosses implicitly.
