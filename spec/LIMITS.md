# Limits and sources

Status: standard. Meters, capacity sources, drain order, hierarchy, and
cross-module contracts.

Liasse meters quantities — credits, quotas, seats, accounting periods —
with three declarations that wire together by name:

```text
$consumes   on a collection: "rows here spend from meter X"
$limits     on any ancestor row: "I hold budget for meter X"
$sources    inside a limit: the views that provide capacity
```

Two rules underneath everything:

```text
You never mutate a bucket. You insert spend rows; capacity, remaining,
and who-funded-what are derived.

Every meter defaults to zero. Capacity exists only where a source
explicitly provides it; consuming anything not explicitly set is
consuming from zero — and rejects the insert.
```

And one convention that answers "which column is what": **`$`-prefixed
fields are structural — the engine reads them. Bare fields are yours —
only your `$order` and `$eligible` read them.** You see the roles at the
projection site, always.

## 1. The simplest meter

A user has top-ups and spends:

```hjson
"users": {
  "$key": "id",

  "topups": {
    "$key": "id",
    "id": { "$type": "uuid", "$default": "= uuid()" },
    "amount": "decimal"
  },

  "spends": {
    "$consumes": "credits",
    "$key": "id",
    "id": { "$type": "uuid", "$default": "= uuid()" },
    "amount": "decimal",
    "occurred_at": { "$type": "timestamp", "$default": "= now()" }
  },

  "$limits": {
    "credits": {
      "$sources": {
        "topup": ".topups { $quantity: .amount }"
      }
    }
  }
}
```

Read the source projection: `.topups { $quantity: .amount }` says *my
`amount` column plays the structural role `$quantity`*. That mapping is the
whole answer to "how do you know which columns are what" — it's written
where the rows enter the meter, and it's checked at model load.

Use it:

```text
.topups + { amount: "1000" }        give capacity
.spends + { amount: "250" }         consume it
```

Read the answers — derived, never stored:

```text
.credits.balance                     -> 750
.credits.pools                       -> live pool instances with .remaining
.spends[x].funding                   -> [{ pool: topup/<id>, amount: 250 }]
```

The spend insert reads nothing but its parameters, so spends never
conflict with each other. There is no `balance = balance - x` anywhere.

## 2. Insufficient funds

```text
.spends + { amount: "2000" }
```

Capacity is an invariant checked before the proposed commit is admitted. Here
it fails, so the insert is rejected deterministically and the caller is told.
You never check-then-spend; you spend, and the meter answers. The same holds
at the extreme: a `$consumes` whose meter has no sources anywhere is a meter
at its default — zero — so every insert is rejected. Nothing is ever
implicitly unlimited.

## 3. Several sources: drain order and overflow

Add expiry to top-ups, and a second source. `$until` is the structural
role for "valid before":

```hjson
"$limits": {
  "credits": {
    "$sources": {
      "topup": ".topups { $quantity: .amount, $until: .expires, price: 0 }",
      "promo": ".promos { $quantity: .amount, $until: .ends, price: .unit_price }"
    },
    "$order": ["$until", "price"]
  }
}
```

`$order` is a plain sort over pool fields — structural (`$until`) and yours
(`price`) mixing freely. Absent values sort last, so pools without an expiry
come after expiring pools; identity is the silent final tiebreak.

State: topup **A** 300 expiring June 30; promo **B** 1000, never expires.
Spend 500:

```text
A: take min(500, 300) = 300      exhausted, 200 still needed
B: take min(200, 1000) = 200     done

spend.funding -> [ topup/A: 300, promo/B: 200 ]
```

That walk is the overflow mechanism: a spend spills across as many pools
as needed, in policy order, chunk by chunk — and never chose a bucket.

## 4. Subscriptions: a table you already have becomes a source

No copying rows into the meter — project your subscriptions table in:

```hjson
"$sources": {
  "topup": ".topups { $quantity: .amount, $until: .expires, price: 0 }",
  "subscription": "/subscriptions[:s | s.account == .] { $quantity: /plans[s.plan].monthly_credits, $from: s.starts, $until: s.ends, $repeat: /plans[s.plan].period, price: /plans[s.plan].unit_price }"
}
```

`$repeat` makes one subscription row an *infinite family* of monthly pool
instances, each with window `[$from + n·P, $from + (n+1)·P)` — half-open,
so months tile exactly. The instance for June exists because arithmetic
says so: no refill job, no scheduler, nothing to miss during downtime.
Unused May credit is gone because May's window is — expiry *is* the window
end. Extend the subscription's `ends` and future instances appear; cancel
it and they stop.

Two subscriptions at once? Two rows match the view, so two families of
instances sit in the same pool set, each anchored to its own start date.
A 600-credit spend on June 20 might fund as
`[pro/June: 180, team/June: 420]` — mid-month offsets respected, nothing
declared.

## 5. Hierarchy: companies meter their subtree

Declare the same meter name at more than one level:

```hjson
"companies": {
  "$key": "id",
  "quota": { "$key": "id", "amount": "decimal",
             "starts": "timestamp", "period": "duration" },

  "$limits": {
    "credits": {
      "$sources": {
        "quota": ".quota { $quantity: .amount, $from: .starts, $repeat: .period }"
      }
    }
  },

  "users": { "…": "as in §4, with their own $limits.credits and spends" },
  "subcompanies": { "$like": "^" }
}
```

The rule: **a spend must clear the meter at every ancestor level that
declares it.** Fred's 600-credit spend is funded three times over — his
own pools, acme-fr's June quota, acme's June quota — and attribution is
kept per level:

```text
spend.funding -> {
  "users/fred":        [ topup: 180, subscription: 420 ],
  "companies/acme-fr": [ quota(2026-06): 600 ],
  "companies/acme":    [ quota(2026-06): 600 ]
}
```

If any level can't cover it — Fred has funds but acme-fr's month is
exhausted — the whole insert is rejected. A level that doesn't declare the
meter passes through. And because `subcompanies` is `$like: "^"`, the
declaration repeats with the shape: recursive companies get recursive
limits with zero extra words.

Levels compose fail-closed: a level that declares the meter enforces its
capacity; a level that doesn't declare it has no opinion and passes
through; and if *no* level declares it, the meter is at its default —
zero — and every insert is rejected. Unlimited never happens by omission; it
would have to be written as a source with infinite quantity, on purpose.
(Linters should still flag a `$consumes` with no limit in sight — legal,
but usually a mistake.)

## 6. Concurrency: two devices, one budget

Two offline devices each spend against June's 300 remaining:

```text
device 1: .spends + { amount: "200" }
device 2: .spends + { amount: "250" }
```

On merge, spends fold in `(time, id)` order — the same on every replica. If
total capacity across eligible pools covers both, **both apply** (the later
one overflows into the next pool). If the meter truly can't cover the later
one, exactly that proposal is rejected, identically everywhere, and the device
learns it from its watch feed. No lock, no hot row, no
read-then-write race — because no spend ever read a balance.

## 7. Earmarked capacity: eligibility

Restrict which pools a spend may touch — still never targeting:

```hjson
"$sources": {
  "topup": ".topups { $quantity: .amount, feature: .feature }"
},
"$eligible": "!has(pool.feature) || pool.feature == spend.feature"
```

`$eligible` is a predicate over the `(pool, spend)` pair; both sides'
*bare* fields are yours, declared in the projections, so a typo is a
load-time error, not a silently-empty filter. Selection in one sentence:
**$eligible filters, $order ranks, the walk overflows, the invariant rejects.**

## 8. Windows without quantity: accounting periods

Leave `$quantity` out and the meter partitions instead of budgeting:

```hjson
"$limits": {
  "ledger": {
    "$sources": {
      "exercise": ".exercises { $from: .starts, $until: .ends }"
    }
  }
},
"entries": { "$consumes": "ledger", "…": "…" }
```

Each entry is assigned to the period whose window contains its spend `$time` —
allocation without capacity. Then:

```text
.ledger.close({ until: @jan_1 })
```

`close` materializes the allocation up to the boundary as ordinary rows and
thereafter rejects any proposal whose spend `$time` falls before the boundary.
Late entries must book in the open period — contre-passation, built in. Before
close, attribution is fluid: fix a mistyped source row and the fold reassigns;
after close, a pre-boundary edit is rejected by the close gate.

The same spectrum, by which roles you use:

```text
$quantity only            a ceiling (constant cap, no windows)
$quantity + windows       a budget (credits, quotas)
windows only              a partition (exercises, periods)
```

No "kind" enum — the roles you project are the kind.

## 9. Crossing module boundaries: nothing implicit

One rule closes every hole:

```text
Meter names are package-local. A bare name in $consumes resolves only
inside the declaring package, and the ancestor walk for $limits stops at
the package boundary. Crossing is always an explicit expose + import,
marked with #.
```

So a module installed inside a company row does **not** draw on the host's
`credits` because of where it sits — installation location grants nothing.
A module's internal meter named `credits` and the host's meter named
`credits` never capture each other. Every cross-module case below is an
explicit contract, checked at install.

### 9a. A module spends against a host meter

The host exposes the meter to its module space — a surface like any other:

```hjson
"modules": {
  "$modules": {
    "$expose": {
      "credits": {
        "$meter": "credits",
        "$spend": { "feature": { "$enum": ["export", "api"], "$optional": true } },
        "$views": ["balance"]
      }
    }
  }
}
```

Read it as the contract it is: *modules here may spend meter `credits`;
a spend carries these declared fields; they may read the balance.* The
`$spend` block is the spend-side interface — exactly the fields the host's
`$eligible` may reference — so the host can never depend on module-private
columns, and the module knows precisely what a spend looks like.

The module imports and consumes through its handle:

```hjson
"$use": { "credits": "$parent" },

"exports": {
  "$consumes": { "#credits": { "$amount": "= .size_mb", "feature": "export" } },
  "$key": "id",
  "size_mb": "decimal",
  "occurred_at": { "$type": "timestamp", "$default": "= now()" }
}
```

`#credits` makes the crossing visible at the point of use. The module's
spends then clear the host's limit at every host level above the exposure
point — Fred's company, its parent, recursively — and attribution shows
the module instance as the actor. If the host can't cover it, the insert
is rejected and the module handles the rejection like any writer. What the module
does *not* see: the host's sources, pools, or other spenders — only what
the surface names (`balance` here).

A collection can consume internal and imported meters at once — the
multi-meter map takes bare names and handles side by side:

```hjson
"$consumes": {
  "internal_quota": "= 1",
  "#credits": "= .size_mb"
}
```

### 9b. A module provides capacity to a host meter

The reverse direction uses the machinery modules already have — the host
declares the pool shape as a module-space interface, modules expose rows
into it, and the host *explicitly* lists the aggregate as a source:

```hjson
"$modules": {
  "$interfaces": {
    "credit_pool": { "$key": "id", "id": "text",
                     "amount": "decimal", "expires": "timestamp" }
  }
},

"$limits": {
  "credits": {
    "$sources": {
      "topup":   ".topups { $quantity: .amount, $until: .expires }",
      "partner": ".modules::credit_pool { $quantity: .amount, $until: .expires }"
    }
  }
}
```

Both sides opted in: the module chose to expose into `credit_pool`, the
host chose to project that aggregate into the meter. The host can narrow
it (`.modules[.approved_packs]::credit_pool`) like any view. Install a
partner-credits pack and a new source joins the drain order; uninstall it
and its pools vanish — attribution refolds, and spends are rejected only if
capacity genuinely no longer covers them.

### 9c. Module to module

A package exposes a meter publicly the same way it exposes anything:

```hjson
"$expose": {
  "pool": { "$meter": "credits", "$spend": { }, "$views": ["balance"] }
}
```

Another module imports it directly — `"$use": { "pool": "vendor.pkg/pool@1" }`
— and consumes `#pool`. Same contract shape, same checks; peers never
touch each other's meters except through what was exposed.

### 9d. The boundary rules, complete

```text
resolution   bare meter names: package-local; ancestor walk stops at the
             package boundary; #handle: the imported contract, nothing else
absent       an unbound optional meter import is fail-closed: the consuming
             declaration is absent if guarded by $if, otherwise inserts are
             rejected from zero capacity
close        close is a $mut; a module can call it only if the surface
             lists it; a host close rejects module spends before the boundary
             like anyone's
versioning   the exposed meter contract ($spend fields, listed views,
             eligibility-relevant semantics) is part of the package's
             compatibility surface: narrowing it is a breaking change,
             majors are never auto-applied
attribution  host-side funding views show module spends with the instance
             path as actor ($as carries the human); module-side visibility
             is exactly the exposed views, no more
```

## Reference card

```text
wire         $consumes: "name"            spends here draw meter "name"
             $consumes: { name: "= expr" } several meters, custom amounts
budget       $limits.name on any ancestor row; AND across all levels
capacity     $sources: named views projecting into the pool roles
roles        pools:  $quantity  $from  $until  $repeat     ($ = engine's)
             spends: $amount (default .amount)  $time (default .occurred_at)
             bare fields = yours, for $order / $eligible / reporting
policy       $order: sort over pool fields, identity tiebreak appended
             $eligible: predicate over (pool, spend)
recurrence   $repeat: analytic instance family, half-open windows,
             no scheduler, downtime-immune
the drain    take min(needed, remaining) per pool, in order, per level
failure      shortfall at any level rejects the whole insert before
             admission, deterministically
default      meters are zero until a source provides capacity; consuming
             the unset consumes from zero and rejects — never unlimited
derived      .name.balance   .name.pools[..].remaining   spend.funding
close        .name.close({ until }) freezes allocation, gates the past
boundaries   meter names are package-local; crossing is explicit:
             host exposes { $meter, $spend, $views } -> module consumes
             #handle; module capacity enters via declared $interfaces
             pools -> host lists the aggregate as a source; unbound
             optional meters fail closed
```
