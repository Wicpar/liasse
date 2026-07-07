# Access, views, refs, and mutations

Status: working draft, syntax pass locked for this iteration.

This document defines the common expression syntax used by Liasse access paths, views, refs, and mutations.

## 1. Expression classes

Liasse separates expression classes by operator meaning.

```text
value expression
  reads values, objects, rows, collections, imports, parameters, and bindings

view expression
  value expression plus projection/construction

mutation expression
  changes data, removes data, calls mutations, or asserts conditions

type expression
  names or constructs types
```

Value and view expressions share access syntax. A view expression adds projection blocks.

### Expression positions

Two position classes govern how expressions are written:

```text
always-expression positions
  $view, $check, $normalize, $as, $back, $expose values, $mut bodies,
  selector filters, and view/mutation blocks
  take bare expressions

literal-or-expression positions
  $default and every value inside $data
  take a literal, or an expression marked with a leading =
```

String escape in literal-or-expression positions:

```text
"= expr"      expression
"'= text"     literal string "= text"
"'text"       literal string "text"; the leading ' is stripped
"''text"      literal string "'text"
"text"        literal string "text"
```

The escape rule is recursive for `$data`: if a `json` field stores a string
that must begin with `=`, author it as `"'= ..."`.

```hjson
"$check": "size(trim(.)) > 0"
"$default": "= uuid()"
"enabled": "= #company.plan == 'fr-pcg'"
"formula": "'= total_ttc"
```

Canonical package artifacts are strict JSON. Authoring tools may accept Hjson
as source input, but the engine first parses Hjson into the same strict JSON
package tree before validation, hashing, or loading. Comments, optional commas,
unquoted keys, quoteless strings, omitted root braces, and triple-quoted
multiline strings are source conveniences only; they add no language semantics.

Documentation convention: `json` fences are complete canonical JSON examples;
`hjson` fences are authoring snippets or fragments.

Hjson quoting does not bypass Liasse expression marking. In a
literal-or-expression position, the parsed string contents are interpreted by
the table above, so a literal string beginning with `=` still needs the leading
`'` escape. `null` from Hjson is ordinary JSON null and is rejected everywhere
except inside fields explicitly typed as `json`; absent/no-value is `none`.

## 1a. Type syntax and canonical primitives

Primitive values have one canonical wire form:

```text
text        JSON string, Unicode scalar sequence, no normalization
bool        JSON true/false
int         JSON string containing canonical base-10 integer digits
decimal     JSON string containing canonical decimal text; no exponent;
            no insignificant trailing zeroes except the single digit 0
uuid        JSON string, lowercase RFC 4122 hyphenated form
date        JSON string, YYYY-MM-DD
timestamp   JSON string, UTC ISO-8601 with fractional seconds normalized
duration    JSON string, ISO-8601 duration
json        canonical JSON value; object keys sorted; numbers encoded by
            the JSON canonicalization profile before hashing
blob        descriptor object { "$sha512", "$bytes", "$media", "$name"? }
enum        JSON string, one of the declared labels
none        expression-level absent/no-value sentinel; serialized as
            { "$none": true } only in canonical value slots, never JSON null
```

Authoring type syntax:

```text
"field": "text"                         primitive or named type
"field": { "$type": "text" }           expanded primitive/named type
"field": { "$type": "text", "$optional": true }
"field": { "$type": "uuid", "$default": "= uuid()" }
"field": { "$type": "text", "$check": [cond, message] }
"field": { "$type": "text", "$normalize": "lower(trim(.))" }
"field": { "$enum": ["admin", "member"] }
"field": { "$ref": "#people", "$on_delete": "restrict" }
"field": { "$set": "text" }
"field": { "$set": { "$ref": "#people", "$on_delete": "cascade" } }
"field": { ... }                         static struct
"rows":  { "$key": "id", ... }          keyed collection
"view":  { "$view": "expr" }            computed view
"field": "named_shape"                  shape alias by name
"field": { "$like": "^" }              positional shape alias
```

Type expressions in signatures and checks:

```text
text | bool | int | decimal | uuid | date | timestamp | duration | json | blob
named_type
collection.$key                 key type, scalar or composite
/absolute.collection.$key
#surface.$key
set<T>                           unordered unique values
list<T>                          ordered values; parameter/expression type
view<T>                          row stream with identity
optional<T>                      inferred when a value may be none
{ field: T, other?: U }           structural object type; ? means optional
```

Model storage uses `$set` for stored unique membership. `list<T>` is available
for parameters, selector arguments, and expression results; stored ordered
lists are represented explicitly as keyed child rows unless a field is `json`.


## 2. Core symbols

```text
/        package root
.        current value or current object
^        lexical parent
^^       lexical parent's parent
#name    imported surface from $use
@name    parameter
name     local binding
actor    builtin: the session's account row (a ref value)
none     builtin: absent/no value for optional fields and cleared refs
```

Examples:

```text
/companies["acme"]
.name
^.plan
^^.country
#company.plan
#people[.lead]
@id
project.name
task.assignee
```

Bare names are bindings. Local access uses `.`.

Checks and transforms use `.` as the current scalar value:

```hjson
"$check": ["size(trim(.)) > 0", "Name is required"]
```

```hjson
"$normalize": "lower(trim(.))"
```

```hjson
"$as": "trim(.)"
```

## 3. Value access

Field access:

```text
receiver.field
```

Examples:

```text
.name
.profile.address.city
^.plan
#company.plan
project.name
```

Single-key selector:

```text
collection[key]
```

Examples:

```text
.people[@id]
.people[.lead]
.users[actor]
/companies["acme"]
```

A row binding (or a builtin holding a row, like `actor`) is accepted where
a key is expected and means that row's key.

Composite-key selector:

```text
.tax_rates[{ country: @country, code: @code }]
```

Composite-key values use object syntax:

```text
{ country: "FR", code: "standard" }
```

Collection key type/declaration:

```text
.tax_rates.$key
```

## 4. Multi-key selectors

Selectors accept any number of key-producing expressions.

```text
collection[key_a, key_b, key_c]
```

Each selector expression may produce:

```text
collection.$key
list<collection.$key>
set<collection.$key>
view<collection.$key>
```

Examples:

```text
.people[@lead, @manager]
.people[@lead, .reviewers, @extra_people]
.tax_rates[
  { country: "FR", code: "standard" },
  { country: "DE", code: "standard" }
]
.tax_rates[@primary_rate, @allowed_rates]
```

Ordering rules:

```text
selector order is preserved
list order is preserved
view order follows that view's $sort
set order follows the target collection's $sort
repeated keys produce repeated row occurrences in row streams
```

When a keyed view would produce duplicate output identity, the checker reports an overlap unless the view declares a distinct synthetic `$key`.

## 5. Row binding

A row binding gives selected rows a local name.

```text
collection[:name]
collection[:name | condition]
```

Examples:

```text
.projects[:project]
.projects[:project | project.archived == false]
.tasks[:task | task.status == "late"]
```

Binding reference:

```text
project.name
project.$key
task.title
task.$key
```

Chained filters use earlier bindings:

```text
.projects[:project | project.archived == false].tasks[:task | task.assignee == project.lead]
```

Lexical parent traversal:

```text
^.plan
^^.country
```

## 6. Same-name binding shorthand

`::` always means same-name row binding.

```text
.projects::
```

expands to:

```text
.projects[:projects]
```

```text
.projects::tasks
```

expands to:

```text
.projects[:projects].tasks[:tasks]
```

```text
.projects::tasks::comments
```

expands to:

```text
.projects[:projects].tasks[:tasks].comments[:comments]
```

Module aggregate:

```text
.modules::ecriture_templates
```

expands to:

```text
.modules[:modules].ecriture_templates[:ecriture_templates]
```

Explicit aliases replace shorthand:

```text
.modules[:module].ecriture_templates[:template]
```

Bindings:

```text
module
template
module.$key
template.$key
```

Repeated segment names use aliases:

```text
.sections[:section].items[:section_item].options[:option].items[:option_item]
```

## 7. View expressions

A view expression reads data and may construct output.

Pass-through table view:

```hjson
"project_index": {
  "$view": ".projects"
}
```

Projected table view:

```hjson
"project_index": {
  "$view": ".projects { id, name }"
}
```

Binding when the key is needed:

```hjson
"project_index": {
  "$view": ".projects:: { id, name, source: projects.$key }"
}
```

Module aggregate:

```hjson
"templates": {
  "$view": ".modules::ecriture_templates"
}
```

Projected module aggregate:

```hjson
"templates": {
  "$view": ".modules::ecriture_templates { id, label, source: { module: modules.$key, template: ecriture_templates.$key } }"
}
```

A view has one authoring surface: the `$view` expression.

## 8. View construction block

View construction uses `:`.

```text
{
  field
  field: expr
  @param
  binding.field
  binding:
  nested: { ... }
  sub_view: path { ... }
}
```

Expansion rules:

```text
field
```

means:

```text
field: .field
```

```text
@name
```

means:

```text
name: @name
```

```text
source.name
```

means:

```text
name: source.name
```

```text
binding:
```

means:

```text
binding: binding
```

Examples:

```text
.imports {
  id,
  name,
  email
}
```

```text
.imports:: {
  imports.id,
  imports.name,
  imports.email
}
```

```text
.modules::ecriture_templates {
  id,
  label,
  module: modules.$key,
  template: ecriture_templates.$key
}
```

Each output path has one definition. Overlaps produce diagnostics.

Scoping inside a projection block:

```text
. is always the current source row being projected
bindings from the source chain stay visible
each declared output field becomes a binding, usable by later fields
  in the same block; declaration order matters; cycles are diagnostics
```

```text
.people {
  first,
  last,
  full: first + " " + last
}
```

`first` and `last` expand to `.first` and `.last` (source fields); `full`
then reads the two output bindings.

## 9. View identity

View identity comes from the source row chain.

```text
.projects
```

identity:

```text
projects.$key
```

```text
.projects { id, name }
```

identity:

```text
projects.$key
```

```text
.projects::
```

identity:

```text
projects.$key
```

with binding:

```text
projects
```

```text
.projects::tasks
```

identity:

```text
projects.$key + tasks.$key
```

```text
.modules::ecriture_templates
```

identity:

```text
modules.$key + ecriture_templates.$key
```

Explicit aliases:

```text
.modules[:module].ecriture_templates[:template]
```

identity:

```text
module.$key + template.$key
```

Projection controls visible fields. Source row chain controls identity.

## 10. Synthetic keyed views

A view projection may declare a synthetic `$key`.

```hjson
"account_totals": {
  "$view": ".entries::lines { $key: account, account, debit: sum(group.debit), credit: sum(group.credit), $sort: [account] }"
}
```

Rules:

```text
$key names output fields; rows with equal key values form one output row
group is a builtin binding: the view of source rows sharing the current key
  (identity: the source row chain; order: the source order)
non-aggregate projection of a non-key source field is a diagnostic
aggregates consume view fields (group.debit) or nested view expressions
```

Aggregate typing:

```text
count(v)                 -> int
sum(v.field)             -> element type; int stays int, decimal stays decimal
avg, min, max (v.field)  -> element type; absent values are skipped
empty input              -> count 0, sum zero of the element type,
                            avg/min/max absent (the output field is optional)
mixing int and decimal in one aggregate is impossible by typing
diagnostics carry the source row chain identity of offending rows
```

A nested view remains available when the group itself must be shaped:

```text
by_account: group { entry: entries.$key, debit, credit }
```

## 11. Sort

Collections and views may declare `$sort`.

Default sort:

```text
$key ascending
```

Collection declaration:

```hjson
"projects": {
  "$key": "id",
  "$sort": ["name", "id"],

  "id": "text",
  "name": "text"
}
```

Descending:

```hjson
"$sort": ["-created_at", "id"]
```

Structured form:

```hjson
"$sort": [
  { "$by": "name", "$dir": "asc" },
  { "$by": "created_at", "$dir": "desc" }
]
```

View sort:

```hjson
"templates": {
  "$view": ".modules::ecriture_templates { id, label, module: modules.$key, $sort: [module, label] }"
}
```

Rules:

```text
$sort defines deterministic read order
$sort preserves identity
sort fields are output fields or computable from visible bindings
default order is by key
composite key default order is lexicographic by canonical key-field order
```

## 11a. Bounds: `$skip` and `$limit`

Two orthogonal, independently optional keys inside a projection, applied
after `$sort` in the only sensible order — sort, skip k, keep n:

```hjson
"latest": { "$view": ".entries { id, date, $sort: [-date], $skip: 50, $limit: 50 }" }
```

```text
deterministic: prefix operations over a total order ($sort + identity
  tiebreak) — legal in model views, checks, meters, exposes
live semantics diff at both boundaries: a row entering above the skip
  line shifts the slice (one enter at its top, one exit at its bottom);
  a deletion inside pulls the successor in — ordinary patches, the
  stream stays bounded end to end
on surface entries, declared $skip/$limit are caps and grants: a client
  may narrow within them, never exceed them
```

## 11b. View combinators

View expressions compose with the complete set algebra and conditionals.
Set operations work on **row identity** (the source row chain, or a
synthetic `$key`).

```text
a | b          union: a's rows, then b's rows whose identity is not in a
a & b          intersection: a's rows whose identity is in b
a - b          difference: a's rows whose identity is not in b
cond ? a : b   conditional: cond is a scalar bool; a and b unify in shape
a ?? b         fallback: b when a is absent (scalars) or empty (views)
[]             the empty view, a legal operand and branch
```

Rules:

```text
left bias: & and - keep the left operand's order and projection;
  | is left order, then the right remainder; wrap in a projection with
  $sort to reorder
shape discipline: operands must share row shape and identity domain;
  heterogeneous operands are a load-time diagnostic — project both into
  a synthetic keyed view whose $key declares the common identity first
position, not new symbols: - means delete only in mutation position and
  difference in view position; + means insert only in mutation position
precedence, tightest first: selectors/projections, &, then | and -
  (left-associative), ??, ? : — parentheses as usual
symmetric difference composes: (a | b) - (a & b)
conditions use full CEL boolean algebra (&&, ||, !, comparisons)
the same ? : and ?? apply at scalar type with the same meaning
```

Examples:

```text
.projects[:p | p.archived] & .projects[:p | p.overdue]
.imports - .people                          only the new rows
s.plan == "pro" ? .premium_templates : .basic_templates
^^.storage.stores[:s | s.enabled] ?? (/stores["main"] | /stores["archive"])
.people + (.imports - .people)              mutation: insert the difference
has(#billing) ? #billing.customers : []
```

Combinators are valid wherever a view expression is: `$view`, `$members`,
`$sources`, `$expose`, `$blob_storage` placement views, and mutation sources.

## 12. Refs

A ref is a checked key type whose values point to existing data.

```json
{
  "$ref": "#people",
  "$on_delete": "restrict"
}
```

This stores a key of `#people`.

Dereference:

```text
#people[.lead]
#people[@lead]
```

Set of refs:

```json
{
  "$set": {
    "$ref": "#people",
    "$on_delete": "restrict"
  }
}
```

Dereference many:

```text
#people[.reviewers]
```

Since selectors are typed, `.reviewers` can be a set of keys.

Ref to keyed view:

```json
{
  "$ref": ".modules::ecriture_templates",
  "$on_delete": "restrict"
}
```

Dereference:

```text
.modules::ecriture_templates[.default_template]
```

The ref value type is inferred from the view identity:

```text
{
  modules: <module-key>,
  ecriture_templates: <template-key>
}
```

## 13. Mutation calls

Mutation calls use object arguments.

```text
#people.rename({ id: .lead, name: @name })
```

Parameter shorthand:

```text
#people.rename({ @id, @name })
```

expands to:

```text
#people.rename({ id: @id, name: @name })
```

Arguments are matched by name.

## 14. Mutation patch block

Mutation patch uses `=` and field removal syntax.

```text
{
  field = expr
  @field
  source.field
  .field
  -field
  nested { field = expr }
}
```

Expansion rules:

```text
@name
```

means:

```text
name = @name
```

```text
source.name
```

means:

```text
name = source.name
```

```text
.name
```

means:

```text
name = .name
```

Example:

```text
.people[@id] {
  @name,
  -email,
  profile {
    locale = @locale
  }
}
```

## 15. Row and collection patching

A patch block on a row target patches that row.

```text
.people[@id] {
  @name
}
```

A patch block on a selected row source patches every selected row.

```text
.people[@a, @b] {
  active = false
}
```

Filtered bulk patch:

```text
.projects[:project | project.status == "draft"] {
  status = "archived"
}
```

A patch block on a collection patches all rows in the collection.

```text
.people {
  active = true
}
```

Field removal across all rows:

```text
.people {
  -email
}
```

## 16. Field and set mutation

Field set:

```text
.people[@id].name = @name
```

Object field replacement:

```text
.people[@id].profile = {
  display_name: @name,
  locale: @locale
}
```

Object field patch:

```text
.people[@id].profile {
  display_name = @name,
  locale = @locale
}
```

Remove optional field:

```text
.people[@id].email -
```

Set add/remove:

```text
.people[@id].tags + @tag
.people[@id].tags - @tag
.people[@id].reviewers + @person
.people[@id].reviewers - @person
```

Set operand arity follows the value:

```text
.people[@id].reviewers + @people
```

where:

```text
@people : set<#people.$key>
```

## 17. Collection insertion

Insertion uses `+` on a collection target.

The operand is a view expression.

Insert one row from params:

```text
.people + {
  @id,
  @name,
  @email
}
```

Generated-key insert:

```text
.people + {
  @name,
  @email
}
```

The target key is derived from the constructed row. Generated-key fields use defaults.

Insert rows from a collection:

```text
.people + .imports {
  id,
  name,
  email
}
```

Insert from module aggregate:

```text
.templates + .modules::ecriture_templates {
  module: modules.$key,
  template: ecriture_templates.$key,
  label,
  journal,
  lines
}
```

## 18. Collection replacement

Replacement uses `=` on a collection target.

```text
collection = view_expression
```

The resulting collection contains exactly the rows constructed by the view expression.

Replace from rows:

```text
.people = .imports {
  id,
  name,
  email
}
```

Replace from module aggregate:

```text
.templates = .modules::ecriture_templates {
  module: modules.$key,
  template: ecriture_templates.$key,
  label,
  journal,
  lines
}
```

Replace with an empty collection:

```text
.people = []
```

Each constructed row is normalized, checked, and keyed according to the target collection.

## 19. Collection deletion by key

Deletion by key uses `-` on a collection target.

```text
collection - keys
```

The key operand type is:

```text
collection.$key
list<collection.$key>
set<collection.$key>
view<collection.$key>
```

Delete one scalar-key row:

```text
.people - @id
```

Delete multiple scalar-key rows:

```text
.people - @people
```

Delete one composite-key row:

```text
.tax_rates - { country: @country, code: @code }
```

Delete multiple composite-key rows:

```text
.tax_rates - @rates
```

Delete keys constructed from a source view:

```text
.people - .imports { id }
```

Composite keys from source:

```text
.tax_rates - .expired_rates { country, code }
```

## 20. Delete selected rows

Unary delete deletes the selected row source.

```text
-.people[@id]
```

```text
-.people[@a, @b]
```

Filtered delete:

```text
-.people[:person | person.disabled]
```

Deep filtered delete:

```text
-.projects[:project | project.archived].tasks[:task | task.status == "open"]
```

## 21. Deep target mutation

Nested target insertion:

```text
.projects[:project | project.archived == false].tasks + .imports::tasks {
  id,
  title,
  assignee
}
```

Nested target replacement:

```text
.projects[:project | project.archived == false].tasks = .late_task_updates {
  id,
  status,
  assignee
}
```

Nested target patch:

```text
.projects[:project | project.archived == false].tasks[:task | task.status == "late"] {
  status = "blocked"
}
```

Deep source, different target:

```text
.work_queue + .projects[:project | project.archived == false].tasks[:task | task.status == "late"] {
  project: project.$key,
  task: task.$key,
  status: "blocked",
  reason: "late task"
}
```

## 22. Mutations declared on views

Mutations are declarable with `$mut` on views, keyed collections, and
static structs alike; a parent surface's `"$mut": [names]` list references
mutations declared on the containing row's shape.

A `$mut` body is one mutation expression, or an array of mutation
expressions forming one atomic sequence: statements apply in order, later
statements see earlier effects, and any failure rejects the whole call before
commit admission.

Views may declare mutations.

```hjson
"people_index": {
  "$view": ".people { id, name, email }",

  "$mut": {
    "rename({ id: uuid, name: text })": ".people[@id] { @name }",
    "import({ source: json })": ".people + @source.people { id, name, email }",
    "replace_all({ source: json })": ".people = @source.people { id, name, email }"
  }
}
```

Mutation calls use object arguments:

```text
#people.rename({ id: .lead, name: @name })
```

## 23. Assertions and zero-match behavior

Assertion:

```text
assert(count(.projects[:project | project.status == "draft"]) > 0, "No draft projects")
```

Filtered multi-row mutation with zero selected rows succeeds at expression level. If the entire call lowers to an empty op tree, the engine returns `unchanged` and admits no commit.

Keyed row patch with a missing row rejects because keyed patch targets an existing row.

## Locked syntax summary

```text
/                         package root
.                         current value/object
^, ^^, ^^^                lexical parent traversal
#name                     imported surface
@name                     parameter
name                      local binding

collection[key]           one row by typed key expression
collection[a, b, c]       ordered multi-selection by typed key/keyset/view expressions

collection[:x]            bind rows as x
collection[:x | cond]     bind filtered rows as x

collection::              bind rows using the collection name
collection::child         bind collection, then bind child rows
:: always means same-name row binding

binding                   bound object
binding.$key              bound object key value
collection.$key           collection key type/declaration

View construction:
  { field, field: expr, @field, source.field, binding: }

View sort:
  $sort inside table declarations and view projections
  default sort is by key

Mutation patch:
  { field = expr, @field, source.field, .field, -field }

Mutation call:
  target.mutation({ param: expr, @param })

Collection insert:
  collection + view_expression

Collection replacement:
  collection = view_expression

Collection patch:
  row_source { patch_block }

Collection delete by key:
  collection - keys

Selected-row delete:
  -row_source

Field set/remove:
  field = value_expression
  field -

Set add/remove:
  set_field + value_expression
  set_field - value_expression

View identity:
  inferred from source row chain unless $key declares a synthetic keyed view

Ref:
  checked key type whose values point to existing data

Projection scoping:
  . is the source row; declared output fields become bindings for later fields
  group binds same-key source rows inside a synthetic keyed view

Mutation sequences:
  a $mut body may be an array of statements applied as one atomic outcome

Bounds:
  $skip: k   $limit: n   after $sort; deterministic slices, two-edge diffs

View combinators:
  a | b   a & b   a - b        identity-based union/intersection/difference
  cond ? a : b   a ?? b   []   conditional, empty/absent fallback, empty view

Expression positions:
  always-expression positions take bare expressions
  $default and $data values take literals, or expressions marked with =;
  leading ' forces a literal and is stripped

Builtins:
  actor  the session's account row
  none   absent/no value

Stubs:
  ~sha512:…  an erased occurrence (value or key segment); literal keys
  starting with ~ escape as '~ (see HISTORY 7)

Meta accessors:
  path.$history  change continuity view with $time, $actor, $value, $prior
  path.$bytes    live + history size ($bytes_live / $bytes_history)

Conditional declarations:
  "$if": expr  guards any declaration block; inactive = absent, data
  archived/restored (replaces $with; see MODULES 6c)

Shapes:
  a type name in field position takes that type's full shape (types use
  the $model grammar; recursion by self-reference); { "$like": "^" } is
  the positional spelling for anonymous recursion; typing is structural
  (see COLLECTIONS 13-14)
```
